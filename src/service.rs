#[cfg(feature = "tower")]
use std::{
    convert::Infallible,
    task::{Context, Poll},
};
use std::{fmt, future::Future, marker::PhantomData, pin::pin, sync::Arc};

use bytes::{Bytes, BytesMut};
use http::{Request, Response};
use http_body::Body;
use http_body_util::{BodyExt as _, Empty};
#[cfg(feature = "tower")]
use tower_service::Service;

#[cfg(feature = "tower")]
use crate::runtime::BoxFuture;
#[cfg(feature = "tracing")]
use crate::{Action, BoxedError, TracedError};
use crate::{
    BodyError, DEFAULT_BODY_LIMIT, Envelope, EventKind, EventMeta, Handler, HeaderView, MaybeSend,
    MaybeSync, ReceiveError, ResponseStatus, Verifier, trace,
};

type ServiceResponse = Response<Empty<Bytes>>;

// The erased `on_error` observer. A trait object admits only one non-auto
// trait, so this cannot be written as `dyn Fn(..) + MaybeSend + MaybeSync` and
// carries the platform split by hand; see `runtime` for the rationale.
#[cfg(not(target_arch = "wasm32"))]
type ErrorObserver<E> = Arc<dyn Fn(&EventMeta, &E) + Send + Sync + 'static>;
#[cfg(target_arch = "wasm32")]
type ErrorObserver<E> = Arc<dyn Fn(&EventMeta, &E) + 'static>;

/// The receiver's policy: what the builder collects and the receiver applies.
/// One type, so `build` moves it whole and the builder and receiver print it
/// the same way.
struct Config<E> {
    verifier: Verifier,
    body_limit: usize,
    handle_ping: bool,
    observer: Option<ErrorObserver<E>>,
    error_fields: ErrorFields<E>,
}

impl<E> Config<E> {
    fn debug_fields(&self, debug: &mut fmt::DebugStruct<'_, '_>) {
        // The observer is a closure and never `Debug`; whether one is
        // registered is the configuration worth printing, as is whether the
        // failed-delivery event carries the error.
        debug
            .field("verifier", &self.verifier)
            .field("body_limit", &self.body_limit)
            .field("handle_ping", &self.handle_ping)
            .field("on_error", &self.observer.is_some())
            .field("trace_errors", &self.error_fields.is_some());
    }
}

// Written out rather than derived: the derive would demand `E: Clone`, and
// error types are routinely not `Clone`; the observer is shared, not copied.
impl<E> Clone for Config<E> {
    fn clone(&self) -> Self {
        Self {
            verifier: self.verifier.clone(),
            body_limit: self.body_limit,
            handle_ping: self.handle_ping,
            observer: self.observer.clone(),
            error_fields: self.error_fields,
        }
    }
}

/// Builds a [`WebhookReceiver`].
///
/// `E` is the handler's error type, which [`on_error`](Self::on_error)
/// observes. It is fixed by [`build`](Self::build), so a chain that ends in
/// `build` never names it; a builder held in a field or returned from a
/// function spells it out.
pub struct WebhookReceiverBuilder<E> {
    config: Config<E>,
}

impl<E> WebhookReceiverBuilder<E> {
    /// Creates a builder with GitHub's 25 MiB payload cap and ping short-circuiting.
    ///
    /// The verifier is required rather than configurable: GitHub webhooks
    /// without a secret are intentionally unsupported, so a receiver that
    /// cannot authenticate is not constructible.
    #[must_use]
    pub fn new(verifier: Verifier) -> Self {
        Self {
            config: Config {
                verifier,
                body_limit: DEFAULT_BODY_LIMIT,
                handle_ping: false,
                observer: None,
                error_fields: ErrorFields::none(),
            },
        }
    }

    /// Sets the maximum bytes read from an unauthenticated request.
    ///
    /// GitHub never sends payloads above [`DEFAULT_BODY_LIMIT`]. Lower values
    /// reduce memory exposure when an application's real events are smaller;
    /// raising the limit does not enable larger GitHub deliveries.
    #[must_use]
    pub const fn body_limit(mut self, limit: usize) -> Self {
        self.config.body_limit = limit;
        self
    }

    /// Controls whether verified `ping` events reach the handler.
    ///
    /// GitHub sends a `ping` when a webhook is created. By default the
    /// receiver answers a verified one with 204 before any handler runs, so
    /// a [`Dispatcher`](crate::Dispatcher) never routes it and its `always`
    /// tier never sees it. `handle_ping(true)` passes it through instead, for
    /// an `always` handler that records every delivery to record that one
    /// too. An unsigned `ping` is 401 either way, and a short-circuited one
    /// never reaches the [`on_error`](Self::on_error) observer.
    #[must_use]
    pub const fn handle_ping(mut self, handle: bool) -> Self {
        self.config.handle_ping = handle;
        self
    }

    /// Registers an observer called with the event meta and the handler's
    /// error whenever the handler fails, before the receiver answers 500.
    ///
    /// The response stays a bare 500 either way: the observer is where an
    /// operator learns why a delivery failed (a log line, a metric), not a
    /// way to change the answer. It is synchronous and returns nothing.
    ///
    /// `E` is the handler's error type, fixed by [`build`](Self::build), and
    /// nothing is asked of it: no `Error`, `Display` or `Debug` bound, so a
    /// `Box<dyn Error + Send + Sync>` is as observable as a named enum.
    /// Annotate the error parameter (`error: &AppError`) when the body calls
    /// methods on it: `build` comes later in the chain than the closure, so
    /// rustc cannot read the type off it there. A body that only formats the
    /// error needs no annotation.
    ///
    /// The observer runs only when a handler ran and failed. A receive
    /// failure (a signature that does not verify, a missing header, an
    /// unsupported content type, a body frame the transport could not
    /// produce, a body over the limit) is a status code and fields on the
    /// receive span, never a handler error, and a `ping` short-circuited by
    /// [`handle_ping`](Self::handle_ping) reaches no handler; neither calls
    /// it.
    ///
    /// With the `tracing` feature, a failed delivery already emits one ERROR
    /// event naming the delivery, observer or not; the error's text is what
    /// it lacks by default, and the observer is not how it gets it. That is
    /// `trace_errors` on this builder, or `trace_boxed_errors` for a
    /// `Box<dyn Error + Send + Sync>` and a `DispatchError` over one, which
    /// put the text and its sources on that same event. The observer is for
    /// what tracing does not do: a metric, a dead letter, a line on stderr;
    /// it runs beside the event either way. The contract is under
    /// [Tracing](crate#tracing).
    ///
    /// With a [`Dispatcher`](crate::Dispatcher) as the handler, the error is
    /// a [`DispatchError`](crate::DispatchError) naming the tier, the
    /// delivery, the failing handler (by type name; a closure's, here) and
    /// the line that registered it; its source is the application error:
    ///
    /// ```
    /// use std::error::Error as _;
    ///
    /// use octoevents::{
    ///     DispatchError, Dispatcher, Envelope, EventMeta, Verifier, WebhookReceiverBuilder,
    ///     WebhookSecret,
    /// };
    /// # use octoevents::DecodeError;
    /// # #[derive(Debug, thiserror::Error)]
    /// # enum AppError {
    /// #     #[error(transparent)]
    /// #     Decode(#[from] DecodeError),
    /// #     #[error("database is down")]
    /// #     Database,
    /// # }
    ///
    /// let dispatcher = Dispatcher::<AppError>::builder()
    ///     .always(|_: Envelope| async { Err::<(), _>(AppError::Database) })
    ///     .build();
    ///
    /// // A failed delivery logs, before the 500:
    /// //   delivery 72d3162e-cc78-11e3-81ab-4c9367dc0958 (issues.opened) failed in the always tier at the handler `app::main::{{closure}}` registered at src/main.rs:12:6
    /// //     caused by: database is down
    /// let receiver = WebhookReceiverBuilder::new(Verifier::new(WebhookSecret::new("current secret")))
    ///     .on_error(|_: &EventMeta, error: &DispatchError<AppError>| {
    ///         eprintln!("{error}");
    ///         let mut cause = error.source();
    ///         while let Some(error) = cause {
    ///             eprintln!("  caused by: {error}");
    ///             cause = error.source();
    ///         }
    ///     })
    ///     .build(dispatcher);
    /// # let _ = receiver;
    /// ```
    #[must_use]
    pub fn on_error<F>(mut self, observer: F) -> Self
    where
        F: Fn(&EventMeta, &E) + MaybeSend + MaybeSync + 'static,
    {
        self.config.observer = Some(Arc::new(observer));
        self
    }

    /// Puts the handler's error on the failed-delivery event: its text as
    /// `error`, its source as `source`, the chain beneath rendered by the
    /// subscriber.
    ///
    /// With the `tracing` feature the receiver emits one event at ERROR for
    /// every failed delivery, and by default nothing of the error is on it,
    /// since the receiver places no bound on the error type. This asks
    /// [`TracedError`], which every `Error` is, and records the error on
    /// that same event, still one, in the `octoevents.receive` span; an
    /// [`on_error`](Self::on_error) observer, if any, runs beside it. The
    /// fields and their forms are under [Tracing](crate#tracing).
    ///
    /// `Error` rather than `Display`, because a `Display` bound cannot walk
    /// `source()`: the text of a [`DispatchError`](crate::DispatchError) says
    /// where the delivery failed, and why is its source, the application
    /// error. A `thiserror` enum qualifies, and so does a `DispatchError`
    /// over one. `Box<dyn Error + Send + Sync>` and a `DispatchError` over
    /// it are no `Error` and go through
    /// [`trace_boxed_errors`](Self::trace_boxed_errors) instead; the compiler
    /// says so, in the crate's words, for the front page's own error type:
    ///
    /// ```compile_fail,E0277
    /// use octoevents::{Dispatcher, Verifier, WebhookReceiverBuilder, WebhookSecret};
    /// type BoxError = Box<dyn std::error::Error + Send + Sync>;
    ///
    /// let dispatcher = Dispatcher::<BoxError>::builder().build();
    /// let receiver = WebhookReceiverBuilder::new(Verifier::new(WebhookSecret::new("current secret")))
    ///     .trace_errors() // `DispatchError<Box<dyn Error + Send + Sync>>` is not an `Error`, so `trace_errors` cannot record it
    ///     .build(dispatcher);
    /// ```
    ///
    /// An error type that is only `Display` (a `String`, say) has no
    /// one-line path, and an `on_error` observer that emits its own event is
    /// the way for one.
    ///
    /// With a [`Dispatcher`](crate::Dispatcher) as the handler, the `fmt`
    /// subscriber renders the two fields as:
    ///
    /// ```text
    /// error=delivery 72d3162e-cc78-11e3-81ab-4c9367dc0958 (issues.opened) failed in the always tier at the handler `app::main::{{closure}}` registered at src/main.rs:12:6 source=database is down
    /// ```
    ///
    /// ```
    /// use octoevents::{DecodeError, Dispatcher, Envelope, Verifier, WebhookReceiverBuilder, WebhookSecret};
    /// # #[derive(Debug, thiserror::Error)]
    /// # enum AppError {
    /// #     #[error(transparent)]
    /// #     Decode(#[from] DecodeError),
    /// #     #[error("database is down")]
    /// #     Database,
    /// # }
    ///
    /// let dispatcher = Dispatcher::<AppError>::builder()
    ///     .always(|_: Envelope| async { Err::<(), _>(AppError::Database) })
    ///     .build();
    ///
    /// let receiver = WebhookReceiverBuilder::new(Verifier::new(WebhookSecret::new("current secret")))
    ///     .trace_errors()
    ///     .build(dispatcher);
    /// # let _ = receiver;
    /// ```
    ///
    /// [`TracedError`]: crate::TracedError
    #[cfg(feature = "tracing")]
    #[must_use]
    pub fn trace_errors(mut self) -> Self
    where
        E: TracedError,
    {
        self.config.error_fields = ErrorFields::of_error();
        self
    }

    /// [`trace_errors`](Self::trace_errors) for a [`BoxedError`]: an error
    /// behind a pointer, `Box<dyn Error + Send + Sync>` foremost, or a
    /// `DispatchError` over one, neither of which is an `Error` itself.
    ///
    /// The event is the same one with the same two fields. For a boxed
    /// error, `error` is its text and `source` its own source; for a
    /// [`DispatchError`](crate::DispatchError) over one, `error` is the
    /// dispatch error's text, saying where, and `source` the boxed error,
    /// saying why, with its chain rendered by the subscriber.
    ///
    /// A receiver whose handlers return `Box<dyn Error + Send + Sync>`, as
    /// the crate front page's does:
    ///
    /// ```
    /// use octoevents::{Dispatcher, Envelope, Verifier, WebhookReceiverBuilder, WebhookSecret};
    ///
    /// type BoxError = Box<dyn std::error::Error + Send + Sync>;
    ///
    /// async fn print(envelope: Envelope) -> Result<(), BoxError> {
    ///     println!("{} {}", envelope.meta.delivery_id, envelope.meta.kind);
    ///     Ok(())
    /// }
    ///
    /// let dispatcher = Dispatcher::<BoxError>::builder().always(print).build();
    /// let webhook = WebhookReceiverBuilder::new(Verifier::new(WebhookSecret::new("development-secret")))
    ///     .trace_boxed_errors()
    ///     .build(dispatcher);
    /// # let _ = webhook;
    /// ```
    ///
    /// [`BoxedError`]: crate::BoxedError
    #[cfg(feature = "tracing")]
    #[must_use]
    pub fn trace_boxed_errors(mut self) -> Self
    where
        E: BoxedError,
    {
        self.config.error_fields = ErrorFields::of_boxed_error();
        self
    }

    /// Builds a receiver around one caller-owned handler.
    ///
    /// The handler is any [`Handler`] over the [`Envelope`]: an `async fn`
    /// taking the envelope, a struct with dependencies, a closure, or a
    /// `Dispatcher`. It does not need to be `Clone`.
    ///
    /// A handler error is answered with a bare 500: the response is GitHub's
    /// delivery record, not a log, so the receiver places no `Display` bound
    /// on `H::Error` and never reads it. To see why a delivery failed,
    /// register an [`on_error`](Self::on_error) observer.
    #[must_use]
    pub fn build<H>(self, handler: H) -> WebhookReceiver<H>
    where
        H: Handler<Envelope, Error = E> + MaybeSend + MaybeSync + 'static,
    {
        WebhookReceiver {
            inner: Arc::new(Inner {
                config: self.config,
                handler,
            }),
        }
    }
}

impl<E> fmt::Debug for WebhookReceiverBuilder<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = formatter.debug_struct("WebhookReceiverBuilder");
        self.config.debug_fields(&mut debug);
        debug.finish()
    }
}

impl<E> Clone for WebhookReceiverBuilder<E> {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
        }
    }
}

/// Authenticates, bounds, and dispatches GitHub webhooks.
///
/// [`WebhookReceiver::receive`] is the entry point everywhere; enabling the
/// `tower` feature additionally implements `tower_service::Service` over the
/// same policy, for routers that want it.
///
/// The caller's router remains responsible for paths and methods. Responses
/// intentionally have empty bodies: handler details belong in logs, not in the
/// delivery record GitHub stores, and the builder's `on_error` observer is
/// where they are handed over.
// Bounded on the struct, as `Inner` is, because the observer's type names
// `H::Error`. Nothing is lost: `build` already required a handler.
pub struct WebhookReceiver<H: Handler<Envelope>> {
    // Shared rather than owned so the receiver is `Clone` for any handler:
    // Tower routers clone a service per connection and its future must own
    // its state, and a struct handler should not need `Clone` for that.
    inner: Arc<Inner<H>>,
}

struct Inner<H: Handler<Envelope>> {
    config: Config<H::Error>,
    handler: H,
}

impl<H: Handler<Envelope>> fmt::Debug for WebhookReceiver<H> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The handler is elided rather than bounded: closures are never
        // `Debug`, and the configuration is what is worth printing.
        let mut debug = formatter.debug_struct("WebhookReceiver");
        self.inner.config.debug_fields(&mut debug);
        debug.finish_non_exhaustive()
    }
}

impl<H: Handler<Envelope>> Clone for WebhookReceiver<H> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<H> WebhookReceiver<H>
where
    H: Handler<Envelope> + MaybeSend + MaybeSync + 'static,
{
    /// Authenticates, bounds, and dispatches one request.
    ///
    /// The request is any `http::Request` whose body yields [`Bytes`]:
    /// axum's, a Cloudflare Worker's, or a `String` in a test, which drives
    /// the receiver with a signed synthetic request and no server.
    /// [`Verifier::sign`] gives the signature GitHub would send for the
    /// body, and a request needs the four headers [`header`](crate::header)
    /// names:
    ///
    /// ```
    /// use octoevents::{Dispatcher, Verifier, WebhookReceiverBuilder, WebhookSecret, header};
    ///
    /// type BoxError = Box<dyn std::error::Error + Send + Sync>;
    ///
    /// # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
    /// let dispatcher = Dispatcher::<BoxError>::builder().build();
    /// let verifier = Verifier::new(WebhookSecret::new("test-secret"));
    /// let webhook = WebhookReceiverBuilder::new(verifier.clone()).build(dispatcher);
    ///
    /// let body = r#"{"action":"opened","sender":{"login":"octocat"}}"#;
    /// let request = http::Request::builder()
    ///     .method("POST")
    ///     .uri("/webhook")
    ///     .header(header::CONTENT_TYPE, "application/json")
    ///     .header(header::DELIVERY_ID, "delivery-1")
    ///     .header(header::EVENT_NAME, "issues")
    ///     .header(header::SIGNATURE, verifier.sign(body.as_bytes()))
    ///     .body(body.to_string())
    ///     .unwrap();
    ///
    /// let response = webhook.receive(request).await;
    ///
    /// assert_eq!(response.status(), 204);
    /// # });
    /// ```
    ///
    /// Build the receiver over another secret and the same request is
    /// answered 401.
    ///
    /// The body's error type must be `Display`. `http_body` asks nothing of
    /// it, so this is a requirement the receiver adds, and one the transports
    /// the crate is used with meet: hyper's, axum's and a Worker's error types,
    /// and `Infallible`. It is what lets a frame the transport cannot produce
    /// be answered 400 as [`ReceiveError::BodyRead`], carrying the transport's
    /// text as its source and nothing else of the transport's type.
    ///
    /// This path never boxes and never crosses a Tower or native executor
    /// boundary, so on `wasm32` a Cloudflare Worker can hand an
    /// `http::Request<worker::Body>` straight in with a handler holding
    /// JavaScript values.
    ///
    /// The future is `Send` on native targets whenever the handler and the
    /// body are, the bounds the `tower` `Service` impl places, so a plain
    /// axum handler calling `receive` accepts every handler `post_service`
    /// does.
    ///
    /// That is promised here rather than left to an `async fn`: an `async fn`
    /// borrowing `&self` leaves its `Send` proof to auto-trait leakage over
    /// the concrete handler, and for a [`Dispatcher`] over
    /// `Box<dyn Error + Send + Sync>`, whose error type the receiver's state
    /// names, that proof fails inside an `async move` block with
    /// "implementation of `Send` is not general enough".
    ///
    /// [`Dispatcher`]: crate::Dispatcher
    // Written as `fn -> impl Future` for the bound on the return type; the
    // body is the `async` block an `async fn` would desugar to.
    #[expect(clippy::manual_async_fn)]
    pub fn receive<B>(
        &self,
        request: Request<B>,
    ) -> impl Future<Output = ServiceResponse> + MaybeSend
    where
        B: Body<Data = Bytes> + MaybeSend,
        B::Error: fmt::Display,
    {
        async move { empty_response(self.inner.process(request).await) }
    }
}

impl<H> Inner<H>
where
    H: Handler<Envelope>,
{
    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(
            name = "octoevents.receive",
            skip_all,
            fields(
                delivery_id = tracing::field::Empty,
                event = tracing::field::Empty,
                outcome = tracing::field::Empty,
                status = tracing::field::Empty,
                error = tracing::field::Empty,
            )
        )
    )]
    async fn process<B>(&self, request: Request<B>) -> ResponseStatus
    where
        B: Body<Data = Bytes>,
        B::Error: fmt::Display,
    {
        let (parts, body) = request.into_parts();
        let headers = HeaderView::from(&parts.headers);
        record_headers(&headers);

        // A request whose signature header cannot authenticate is refused on
        // the headers alone, so unsigned traffic never occupies `body_limit`
        // bytes of memory. `Envelope::from_signed` repeats the check for
        // transports that construct envelopes directly.
        if let Err(error) = headers.require_signature() {
            return refuse(&error.into());
        }

        let Config {
            verifier,
            body_limit,
            handle_ping,
            observer,
            error_fields,
        } = &self.config;

        let bytes = match read_body(body, *body_limit).await {
            Ok(bytes) => bytes,
            Err(error) => return refuse(&error),
        };

        let envelope = match Envelope::from_signed(verifier, &headers, bytes) {
            Ok(envelope) => envelope,
            Err(error) => return refuse(&error),
        };

        if !handle_ping && matches!(envelope.meta.kind, EventKind::Ping) {
            return record_outcome(ResponseStatus::NoContent);
        }

        // The handler takes the envelope by value, so the meta a failure is
        // reported with, to the observer and to the tracing event, is cloned
        // beforehand, and only when there is something to report to: with
        // the `tracing` feature the failed-delivery event always is.
        let reporting = observer.is_some() || cfg!(feature = "tracing");
        let meta = reporting.then(|| envelope.meta.clone());
        match self.handler.handle(envelope).await {
            Ok(()) => record_outcome(ResponseStatus::NoContent),
            Err(error) => {
                // The outcome goes on the span first, so the event and the
                // observer run inside a span that already says how the
                // delivery ended.
                let status = record_outcome(ResponseStatus::InternalServerError);
                if let Some(meta) = &meta {
                    error_fields.handler_failed(meta, &error, status.as_u16());
                    if let Some(observer) = observer {
                        observer(meta, &error);
                    }
                }
                status
            }
        }
    }
}

/// Lets a Tower router own the path and method while the receiver owns the
/// policy.
///
/// ```
/// use axum::{Router, routing::post_service};
/// use octoevents::{Envelope, Handler, Verifier, WebhookReceiverBuilder, WebhookSecret};
///
/// struct Persist { /* database pool */ }
///
/// impl Handler<Envelope> for Persist {
///     type Error = std::io::Error;
///
///     async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error> {
///         // Persist or forward before returning; process asynchronously.
///         let _ = (envelope.meta.delivery_id, envelope.raw_payload);
///         Ok(())
///     }
/// }
///
/// let verifier = Verifier::new(WebhookSecret::new("current secret"))
///     .also(WebhookSecret::new("previous secret"));
///
/// let webhook = WebhookReceiverBuilder::new(verifier)
///     .body_limit(1024 * 1024)
///     .build(Persist {});
///
/// let app: Router = Router::new().route("/webhook", post_service(webhook));
/// # let _ = app;
/// ```
#[cfg(feature = "tower")]
impl<H, B> Service<Request<B>> for WebhookReceiver<H>
where
    H: Handler<Envelope> + MaybeSend + MaybeSync + 'static,
    B: Body<Data = Bytes> + MaybeSend + 'static,
    B::Error: fmt::Display,
{
    type Response = ServiceResponse;
    type Error = Infallible;
    type Future = BoxFuture<'static, Result<ServiceResponse, Infallible>>;

    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<B>) -> Self::Future {
        // The boxed future must be `'static`, so it owns a reference-counted
        // handle to the receiver state rather than borrowing `self`.
        let inner = Arc::clone(&self.inner);

        Box::pin(async move { Ok(empty_response(inner.process(request).await)) })
    }
}

fn empty_response(status: ResponseStatus) -> ServiceResponse {
    Response::builder()
        .status(http::StatusCode::from(status))
        .body(Empty::new())
        .expect("an empty response with a fixed status always builds")
}

// A `respond` type's conversion, kept here rather than beside the type so
// `respond` stays free of the `http` cfg: the receiver is the one place that
// answers with an `http::StatusCode`.
impl From<ResponseStatus> for http::StatusCode {
    fn from(status: ResponseStatus) -> Self {
        match status {
            ResponseStatus::NoContent => Self::NO_CONTENT,
            ResponseStatus::BadRequest => Self::BAD_REQUEST,
            ResponseStatus::Unauthorized => Self::UNAUTHORIZED,
            ResponseStatus::PayloadTooLarge => Self::PAYLOAD_TOO_LARGE,
            ResponseStatus::InternalServerError => Self::INTERNAL_SERVER_ERROR,
        }
    }
}

/// Reads `body` into memory, within `limit` bytes.
///
/// The one place the receiver touches the transport, so every way a body can
/// fail to arrive has its `ReceiveError` here. A body whose size hint is
/// already over the limit is refused before the first poll, one that crosses
/// it mid-stream at the frame that crossed, both as
/// [`ReceiveError::BodyTooLarge`]; a frame the transport could not produce is
/// [`ReceiveError::BodyRead`], with the transport's error as text. Trailers
/// are passed over and do not count toward the limit.
async fn read_body<B>(body: B, limit: usize) -> Result<Bytes, ReceiveError>
where
    B: Body<Data = Bytes>,
    B::Error: fmt::Display,
{
    // Pinned here, where the body is polled, so `B` owes no `Unpin` to the
    // caller: a transport's body type is whatever it is.
    let mut body = pin!(body);

    // The comparison is in `u64` so a hint above `usize::MAX` (possible on
    // 32-bit targets, wasm included) still takes the fast path.
    if u64::try_from(limit).is_ok_and(|limit| body.size_hint().lower() > limit) {
        return Err(ReceiveError::BodyTooLarge { limit });
    }

    let mut bytes = BytesMut::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|error| ReceiveError::BodyRead(BodyError::new(error)))?;
        let Ok(data) = frame.into_data() else {
            continue;
        };
        if bytes
            .len()
            .checked_add(data.len())
            .is_none_or(|length| length > limit)
        {
            return Err(ReceiveError::BodyTooLarge { limit });
        }
        bytes.extend_from_slice(&data);
    }

    Ok(bytes.freeze())
}

fn record_headers(headers: &HeaderView<'_>) {
    if let Some(delivery_id) = headers.delivery_id.as_deref() {
        trace::record("delivery_id", delivery_id);
    }
    if let Some(event) = headers.event_name.as_deref() {
        trace::record("event", event);
    }
}

fn record_outcome(status: ResponseStatus) -> ResponseStatus {
    trace::record("outcome", outcome_label(status));
    trace::record("status", status.as_u16());
    status
}

/// Answers a request refused before any handler ran: the status the contract
/// maps `error` to, recorded as the span's outcome, and the error's text as
/// the span's `error`, so the span says which refusal it was where `outcome`
/// says only its class. Every pre-handler failure is an error value and goes
/// through here, so none selects a status on its own.
fn refuse(error: &ReceiveError) -> ResponseStatus {
    let status = record_outcome(ResponseStatus::for_receive_error(error));
    record_refusal(error);
    status
}

/// Records the refusal's text on the receive span as `error`, through
/// `tracing::field::display`, the form the failed-delivery event fixed for
/// that name, so `error` is one field to a subscriber wherever it appears.
///
/// The text alone, and unconditionally: every `ReceiveError` message is the
/// crate's own fixed wording, so it can carry nothing from the request. Its
/// source is not recorded. Beneath `BodyRead` that is the transport's text,
/// which is the transport's to write and could quote the request, signature
/// included; it stays on the error value, as a handler's error text stays
/// off the failed-delivery event until `trace_errors` asks for it.
#[cfg(feature = "tracing")]
fn record_refusal(error: &ReceiveError) {
    trace::record("error", tracing::field::display(error));
}

/// Records nothing: the `tracing` feature is disabled.
#[cfg(not(feature = "tracing"))]
fn record_refusal(_error: &ReceiveError) {}

/// The value the `octoevents.receive` span records as `outcome`.
///
/// A label rather than the code, so `outcome` is a string on every span the
/// crate opens; the code is the span's `status` field. The vocabulary is the
/// receive span's own, which is why it lives with the receiver and not on
/// [`ResponseStatus`].
const fn outcome_label(status: ResponseStatus) -> &'static str {
    match status {
        ResponseStatus::NoContent => "ok",
        ResponseStatus::BadRequest => "bad_request",
        ResponseStatus::Unauthorized => "unauthorized",
        ResponseStatus::PayloadTooLarge => "payload_too_large",
        ResponseStatus::InternalServerError => "handler_error",
    }
}

/// Which fields of the handler's error the failed-delivery event carries:
/// the receiver's setting, made on the builder.
///
/// The event always carries the event meta's identifying fields and the
/// status. Recording the error's text and source as well needs a bound on
/// the handler's error type, which the receiver itself does not place, so
/// the default, [`none`](Self::none), records neither, and `trace_errors` or
/// `trace_boxed_errors` swaps in a function that reads the error through the
/// bound it asked for. A function pointer rather than a trait object: the
/// three are known, capture nothing, and one is picked at build time.
///
/// It is one event either way, never a second one for the text: the
/// receiver knows which it emits, where an `on_error` observer emitting the
/// text could not tell the receiver to stay quiet.
///
/// Without the `tracing` feature the setting has nothing to hold and its
/// methods are no-ops, so the receiver calls them without a `cfg`.
struct ErrorFields<E> {
    #[cfg(feature = "tracing")]
    emit: Option<fn(&EventMeta, &E, u16)>,
    // `fn(&E)` rather than `E`: the receiver's `Send` and `Sync` must not
    // depend on the error type, and neither must this type's.
    error: PhantomData<fn(&E)>,
}

// Hand-written for the same reason `Config`'s is: a derive would ask
// `E: Clone` and `E: Debug`, and the type holds no `E`.
impl<E> Clone for ErrorFields<E> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<E> Copy for ErrorFields<E> {}

impl<E> ErrorFields<E> {
    /// The default: the identifying fields and the status, nothing of the
    /// error.
    const fn none() -> Self {
        Self {
            #[cfg(feature = "tracing")]
            emit: None,
            error: PhantomData,
        }
    }
}

#[cfg(feature = "tracing")]
impl<E> ErrorFields<E> {
    /// Emits the event for a failed delivery, with the fields this setting
    /// asks for.
    fn handler_failed(self, meta: &EventMeta, error: &E, status: u16) {
        match self.emit {
            Some(emit) => emit(meta, error, status),
            None => handler_failed(meta, status, None, None),
        }
    }

    /// Whether the event carries the error, for the receiver's `Debug`.
    fn is_some(self) -> bool {
        self.emit.is_some()
    }

    /// The error's [`Display`](std::fmt::Display) as `error` and its
    /// [`source`](std::error::Error::source) as `source`. The bound is the
    /// one `trace_errors` asks; `TracedError` is an `Error`, so the body
    /// reads the error through that.
    const fn of_error() -> Self
    where
        E: TracedError,
    {
        Self {
            emit: Some(|meta, error, status| {
                handler_failed(meta, status, Some(error), error.source());
            }),
            error: PhantomData,
        }
    }

    /// A [`BoxedError`]'s text as `error` and its source as `source`: for an
    /// error behind a pointer, that error's own; for a `DispatchError` over
    /// one, the dispatch error's text and the boxed error.
    const fn of_boxed_error() -> Self
    where
        E: BoxedError,
    {
        Self {
            emit: Some(|meta, error, status| {
                handler_failed(meta, status, Some(error.text()), error.source());
            }),
            error: PhantomData,
        }
    }
}

// The stubs are methods, as the struct doc says, so the receiver reads a
// setting that does not exist without the feature through the same calls.
#[cfg(not(feature = "tracing"))]
#[expect(clippy::unused_self)]
impl<E> ErrorFields<E> {
    /// Emits nothing: the `tracing` feature is disabled.
    fn handler_failed(self, _meta: &EventMeta, _error: &E, _status: u16) {}

    /// Never: the `tracing` feature is disabled.
    fn is_some(self) -> bool {
        false
    }
}

/// The one `tracing::error!` for a failed delivery, so the event's fields
/// are declared in one place whatever the receiver was asked to record.
///
/// `error` is recorded through its `Display` and `source` as an error value
/// the subscriber walks itself. Two fields rather than the error alone as
/// one value, because a subscriber's error value must be `Error + 'static`,
/// and the error `trace_boxed_errors` traces, a
/// [`DispatchError`](crate::DispatchError) over a boxed error, is no `Error`:
/// its text and its source are all it can offer, so every shape offers the
/// same two. The fields it shares with the spans (`delivery_id`, `event`,
/// `action`, `installation_id`, `status`) are recorded in the forms `trace`
/// fixes for them.
#[cfg(feature = "tracing")]
fn handler_failed(
    meta: &EventMeta,
    status: u16,
    error: Option<&dyn std::fmt::Display>,
    source: Option<&(dyn std::error::Error + 'static)>,
) {
    tracing::error!(
        delivery_id = meta.delivery_id.as_str(),
        event = meta.kind.as_str(),
        action = meta.action.as_ref().map(Action::as_str),
        installation_id = meta.installation_id,
        status,
        error = error.map(tracing::field::display),
        source,
        "handler failed"
    );
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
