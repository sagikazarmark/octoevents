#[cfg(feature = "tower")]
use std::{
    convert::Infallible,
    task::{Context, Poll},
};
use std::{fmt, future::Future, marker::PhantomData, sync::Arc};

use bytes::{Bytes, BytesMut};
use http::{Request, Response};
use http_body::Body;
use http_body_util::{BodyExt as _, Empty};
#[cfg(feature = "tower")]
use tower_service::Service;

#[cfg(feature = "tower")]
use crate::runtime::BoxFuture;
#[cfg(feature = "tracing")]
use crate::{Action, BoxedError};
use crate::{
    DEFAULT_BODY_LIMIT, Envelope, EventKind, EventMeta, Handler, HeaderView, MaybeSend, MaybeSync,
    ReceiveError, ResponseStatus, Verifier, trace,
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
    /// unsupported content type, a body over the limit) is a status code and
    /// a span field, never a handler error, and a `ping` short-circuited by
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
    ///     DispatchError, Dispatcher, Envelope, EventMeta, Secret, Verifier, WebhookReceiverBuilder,
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
    /// let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new("current secret")))
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
    /// `E: Error` and records the error on that same event, still one, in
    /// the `octoevents.receive` span; an [`on_error`](Self::on_error)
    /// observer, if any, runs beside it. The fields and their forms are
    /// under [Tracing](crate#tracing).
    ///
    /// `Error` rather than `Display`, because a `Display` bound cannot walk
    /// `source()`: the text of a [`DispatchError`](crate::DispatchError) says
    /// where the delivery failed, and why is its source, the application
    /// error. A `thiserror` enum qualifies, and so does a `DispatchError`
    /// over one. `Box<dyn Error + Send + Sync>` and a `DispatchError` over
    /// it are no `Error`, and are
    /// [`trace_boxed_errors`](Self::trace_boxed_errors)'; an error type that
    /// is only `Display` (a `String`, say) has no one-line path, and an
    /// `on_error` observer that emits its own event is the way for one.
    ///
    /// With a [`Dispatcher`](crate::Dispatcher) as the handler, the `fmt`
    /// subscriber renders the two fields as:
    ///
    /// ```text
    /// error=delivery 72d3162e-cc78-11e3-81ab-4c9367dc0958 (issues.opened) failed in the always tier at the handler `app::main::{{closure}}` registered at src/main.rs:12:6 source=database is down
    /// ```
    ///
    /// ```
    /// use octoevents::{DecodeError, Dispatcher, Envelope, Secret, Verifier, WebhookReceiverBuilder};
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
    /// let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new("current secret")))
    ///     .trace_errors()
    ///     .build(dispatcher);
    /// # let _ = receiver;
    /// ```
    #[cfg(feature = "tracing")]
    #[must_use]
    pub fn trace_errors(mut self) -> Self
    where
        E: std::error::Error,
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
    /// use octoevents::{Dispatcher, Envelope, Secret, Verifier, WebhookReceiverBuilder};
    ///
    /// type BoxError = Box<dyn std::error::Error + Send + Sync>;
    ///
    /// async fn print(envelope: Envelope) -> Result<(), BoxError> {
    ///     println!("{} {}", envelope.meta.delivery_id, envelope.meta.kind);
    ///     Ok(())
    /// }
    ///
    /// let dispatcher = Dispatcher::<BoxError>::builder().always(print).build();
    /// let webhook = WebhookReceiverBuilder::new(Verifier::new(Secret::new("development-secret")))
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
    /// use octoevents::{Dispatcher, Secret, Verifier, WebhookReceiverBuilder, header};
    ///
    /// type BoxError = Box<dyn std::error::Error + Send + Sync>;
    ///
    /// # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
    /// let dispatcher = Dispatcher::<BoxError>::builder().build();
    /// let verifier = Verifier::new(Secret::new("test-secret"));
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
    #[allow(clippy::manual_async_fn)]
    pub fn receive<B>(
        &self,
        request: Request<B>,
    ) -> impl Future<Output = ServiceResponse> + MaybeSend
    where
        B: Body<Data = Bytes> + MaybeSend + Unpin,
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
            )
        )
    )]
    async fn process<B>(&self, request: Request<B>) -> ResponseStatus
    where
        B: Body<Data = Bytes> + Unpin,
    {
        let (parts, mut body) = request.into_parts();
        let headers = HeaderView::from(&parts.headers);
        record_headers(&headers);

        // A request whose signature header cannot authenticate is refused on
        // the headers alone, so unsigned traffic never occupies `body_limit`
        // bytes of memory. `Envelope::from_signed` repeats the check for
        // transports that construct envelopes directly.
        if let Err(error) = headers.require_signature() {
            return record_outcome(ResponseStatus::for_receive_error(&error.into()));
        }

        let Config {
            verifier,
            body_limit,
            handle_ping,
            observer,
            error_fields,
        } = &self.config;

        // The comparison is in `u64` so a hint above `usize::MAX` (possible
        // on 32-bit targets, wasm included) still takes the fast path.
        if u64::try_from(*body_limit).is_ok_and(|limit| body.size_hint().lower() > limit) {
            return record_outcome(body_too_large(*body_limit));
        }

        let mut bytes = BytesMut::new();
        while let Some(frame) = body.frame().await {
            let Ok(frame) = frame else {
                return record_outcome(ResponseStatus::BadRequest);
            };
            let Ok(data) = frame.into_data() else {
                continue;
            };
            if bytes
                .len()
                .checked_add(data.len())
                .is_none_or(|length| length > *body_limit)
            {
                return record_outcome(body_too_large(*body_limit));
            }
            bytes.extend_from_slice(&data);
        }

        let envelope = match Envelope::from_signed(verifier, &headers, bytes.freeze()) {
            Ok(envelope) => envelope,
            Err(error) => return record_outcome(ResponseStatus::for_receive_error(&error)),
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
/// use octoevents::{Envelope, Handler, Secret, Verifier, WebhookReceiverBuilder};
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
/// let verifier = Verifier::new(Secret::new("current secret"))
///     .also(Secret::new("previous secret"));
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
    B: Body<Data = Bytes> + MaybeSend + Unpin + 'static,
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

fn body_too_large(limit: usize) -> ResponseStatus {
    ResponseStatus::for_receive_error(&ReceiveError::BodyTooLarge { limit })
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
    /// [`source`](std::error::Error::source) as `source`.
    const fn of_error() -> Self
    where
        E: std::error::Error,
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
#[allow(clippy::unused_self)]
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

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        pin::Pin,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        task::{Context, Poll},
    };

    use bytes::Bytes;
    use http::{HeaderMap, Request, StatusCode};
    use http_body::{Body as _, Frame};
    use http_body_util::Full;
    #[cfg(feature = "tower")]
    use tower::ServiceExt as _;

    use super::{WebhookReceiverBuilder, empty_response, outcome_label};
    use crate::{
        Action, DecodeError, Dispatcher, Envelope, EventKind, EventMeta, Handler, ResponseStatus,
        Secret, Verifier, test_support::AppError,
    };

    /// A production-shaped handler: dependencies as fields, borrowed through
    /// `&self`, and deliberately not `Clone`.
    struct Recorder {
        calls: Arc<AtomicUsize>,
    }

    impl Handler<Envelope> for Recorder {
        type Error = std::convert::Infallible;

        // A real handler awaits its dependencies; this one only counts.
        #[allow(clippy::unused_async_trait_impl)]
        async fn handle(&self, _envelope: Envelope) -> Result<(), Self::Error> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    /// A consumer-defined view of an `issues` payload: three fields, no
    /// octocrab dependency, bound to its kind by its `Payload` impl.
    #[derive(serde::Deserialize)]
    struct IssueView {
        action: String,
        issue: IssueNumber,
    }

    #[derive(serde::Deserialize)]
    struct IssueNumber {
        number: u64,
    }

    impl crate::Payload for IssueView {
        const KIND: EventKind = EventKind::Issues;
    }

    /// The single-handler path: a handler over the envelope that decodes one
    /// kind's view itself with `decode_payload`. What a kind mismatch or a
    /// payload that does not fit the view does is `decode_payload`'s own
    /// test; here it is one handler the receiver hands the envelope to.
    struct IssueRecorder {
        seen: Arc<std::sync::Mutex<Vec<(String, String, u64)>>>,
    }

    impl Handler<Envelope> for IssueRecorder {
        type Error = DecodeError;

        #[allow(clippy::unused_async_trait_impl)]
        async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error> {
            let payload = envelope.decode_payload::<IssueView>()?;
            self.seen.lock().unwrap().push((
                envelope.meta.delivery_id,
                payload.action,
                payload.issue.number,
            ));
            Ok(())
        }
    }

    /// A body as it arrives over a real connection: one frame per poll and
    /// the default size hint, so the receiver has no total to check up front
    /// and must count as it reads. `polls` says how far it read, which tells
    /// a 413 answered from inside the read loop from one the size hint
    /// answered before the first poll.
    struct Frames {
        frames: VecDeque<Frame<Bytes>>,
        polls: Arc<AtomicUsize>,
    }

    impl Frames {
        fn new(frames: impl IntoIterator<Item = Frame<Bytes>>) -> Self {
            Self {
                frames: frames.into_iter().collect(),
                polls: Arc::default(),
            }
        }

        /// One data frame per chunk.
        fn data(chunks: &[&'static [u8]]) -> Self {
            Self::new(
                chunks
                    .iter()
                    .map(|chunk| Frame::data(Bytes::from_static(chunk))),
            )
        }

        fn polls(&self) -> Arc<AtomicUsize> {
            Arc::clone(&self.polls)
        }
    }

    impl http_body::Body for Frames {
        type Data = Bytes;
        type Error = std::convert::Infallible;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
            self.polls.fetch_add(1, Ordering::Relaxed);
            Poll::Ready(self.frames.pop_front().map(Ok))
        }
    }

    fn request(body: &'static [u8], event: &str) -> Request<Full<Bytes>> {
        request_over(
            Full::new(Bytes::from_static(body)),
            event,
            &verifier().sign(body),
        )
    }

    /// A request over a body the test shapes itself, carrying `signature` as
    /// its signature header: `verifier().sign(..)` over the bytes the body
    /// yields authenticates, anything else does not.
    fn request_over<B>(body: B, event: &str, signature: &str) -> Request<B> {
        Request::builder()
            .header("content-type", "application/json")
            .header("x-github-delivery", "delivery")
            .header("x-github-event", event)
            .header("x-hub-signature-256", signature)
            .body(body)
            .unwrap()
    }

    /// A signed request with one header replaced: the tampering the receiver
    /// must refuse.
    fn with_header(
        body: &'static [u8],
        event: &str,
        name: &'static str,
        value: &str,
    ) -> Request<Full<Bytes>> {
        let mut request = request(body, event);
        request.headers_mut().insert(name, value.parse().unwrap());
        request
    }

    /// A signed request with one header removed.
    fn without_header(body: &'static [u8], event: &str, name: &str) -> Request<Full<Bytes>> {
        let mut request = request(body, event);
        request.headers_mut().remove(name);
        request
    }

    const WRONG_SIGNATURE: &str =
        "sha256=0000000000000000000000000000000000000000000000000000000000000000";

    /// The verifier the receivers here are built with; it signs the requests
    /// they accept.
    fn verifier() -> Verifier {
        Verifier::new(Secret::new("secret"))
    }

    #[tokio::test]
    async fn returns_no_content_after_successful_dispatch() {
        let receiver =
            WebhookReceiverBuilder::new(verifier()).build(|_: Envelope| async { Ok::<_, ()>(()) });

        let response = receiver.receive(request(b"{}", "push")).await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn accepts_a_struct_handler_that_is_not_clone() {
        let calls = Arc::new(AtomicUsize::new(0));
        let receiver = WebhookReceiverBuilder::new(verifier()).build(Recorder {
            calls: Arc::clone(&calls),
        });

        let response = receiver.receive(request(b"{}", "push")).await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn accepts_an_async_fn_item_over_the_envelope() {
        async fn count(envelope: Envelope) -> Result<(), std::convert::Infallible> {
            COUNTED_BYTES.fetch_add(envelope.raw_payload.len(), Ordering::Relaxed);
            Ok(())
        }
        static COUNTED_BYTES: AtomicUsize = AtomicUsize::new(0);

        let receiver = WebhookReceiverBuilder::new(verifier()).build(count);

        let response = receiver.receive(request(b"{}", "push")).await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(COUNTED_BYTES.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn accepts_an_arc_shared_struct_handler_and_leaves_the_caller_its_handle() {
        let recorder = Arc::new(Recorder {
            calls: Arc::new(AtomicUsize::new(0)),
        });
        let receiver = WebhookReceiverBuilder::new(verifier()).build(Arc::clone(&recorder));

        let response = receiver.receive(request(b"{}", "push")).await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(recorder.calls.load(Ordering::Relaxed), 1);
    }

    #[cfg(feature = "tower")]
    #[tokio::test]
    async fn the_tower_service_impl_applies_the_same_policy() {
        let receiver =
            WebhookReceiverBuilder::new(verifier()).build(|_: Envelope| async { Ok::<_, ()>(()) });

        let response = receiver.oneshot(request(b"{}", "push")).await.unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[cfg(feature = "tower")]
    #[tokio::test]
    async fn the_tower_service_impl_accepts_a_struct_handler() {
        let calls = Arc::new(AtomicUsize::new(0));
        let receiver = WebhookReceiverBuilder::new(verifier()).build(Recorder {
            calls: Arc::clone(&calls),
        });

        let response = receiver.oneshot(request(b"{}", "push")).await.unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn a_single_kind_handler_receives_its_decoded_view_with_the_metadata() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let receiver = WebhookReceiverBuilder::new(verifier()).build(IssueRecorder {
            seen: Arc::clone(&seen),
        });

        let response = receiver
            .receive(request(
                br#"{"action":"opened","issue":{"number":7,"title":"ignored"}}"#,
                "issues",
            ))
            .await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            [("delivery".to_owned(), "opened".to_owned(), 7)]
        );
    }

    #[tokio::test]
    async fn the_always_tier_receives_the_exact_bytes_the_receiver_verified() {
        // Irregular whitespace and a trailing newline: any re-encoding between
        // verification and the always tier would normalize them away.
        const BODY: &[u8] = b"{ \"action\" :\t\"opened\",\n  \"number\": 7 }\n";

        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let handler_seen = Arc::clone(&seen);
        let dispatcher = Dispatcher::<AppError>::builder()
            .always(move |envelope: Envelope| {
                let seen = Arc::clone(&handler_seen);
                async move {
                    seen.lock()
                        .unwrap()
                        .push((envelope.meta.kind, envelope.raw_payload));
                    Ok::<_, std::convert::Infallible>(())
                }
            })
            .build();
        let receiver = WebhookReceiverBuilder::new(verifier()).build(dispatcher);

        let response = receiver.receive(request(BODY, "pull_request")).await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            [(EventKind::PullRequest, Bytes::from_static(BODY))]
        );
    }

    #[tokio::test]
    async fn short_circuits_ping_unless_enabled() {
        let calls = Arc::new(AtomicUsize::new(0));
        let handler_calls = Arc::clone(&calls);
        let receiver = WebhookReceiverBuilder::new(verifier()).build(move |_: Envelope| {
            handler_calls.fetch_add(1, Ordering::Relaxed);
            async { Ok::<_, ()>(()) }
        });

        let response = receiver.receive(request(b"{}", "ping")).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(calls.load(Ordering::Relaxed), 0);

        let handler_calls = Arc::clone(&calls);
        let receiver = WebhookReceiverBuilder::new(verifier())
            .handle_ping(true)
            .build(move |_: Envelope| {
                handler_calls.fetch_add(1, Ordering::Relaxed);
                async { Ok::<_, ()>(()) }
            });
        receiver.receive(request(b"{}", "ping")).await;
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn answers_a_receive_failure_with_the_status_the_contract_maps_it_to() {
        // The receiver's one test dedicated to the mapping. Which failure a
        // request earns is pinned on `Envelope::from_signed`, and which
        // status each failure maps to on `ResponseStatus::for_receive_error`,
        // so one failure through HTTP shows the receiver answers with the
        // mapped status; the refusal before the body is read has its own
        // test below.
        let receiver =
            WebhookReceiverBuilder::new(verifier()).build(|_: Envelope| async { Ok::<_, ()>(()) });

        let mismatch = with_header(b"{}", "push", "x-hub-signature-256", WRONG_SIGNATURE);
        let response = receiver.receive(mismatch).await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.body().size_hint().exact(), Some(0));
    }

    #[tokio::test]
    async fn refuses_an_unsigned_request_without_reading_the_body() {
        // A body that fails on first poll: reaching the read loop shows up as
        // 400, so a 401 proves the signature headers were decisive alone.
        struct FailingBody;

        impl http_body::Body for FailingBody {
            type Data = Bytes;
            type Error = &'static str;

            fn poll_frame(
                self: Pin<&mut Self>,
                _context: &mut Context<'_>,
            ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
                Poll::Ready(Some(Err("body must not be read")))
            }
        }

        let receiver = || {
            WebhookReceiverBuilder::new(verifier()).build(|_: Envelope| async { Ok::<_, ()>(()) })
        };
        let request = |signature: Option<&str>| {
            let builder = Request::builder()
                .header("content-type", "application/json")
                .header("x-github-delivery", "delivery")
                .header("x-github-event", "push");
            match signature {
                Some(signature) => builder.header("x-hub-signature-256", signature),
                // GitHub's legacy SHA-1 header alone does not sign a request.
                None => builder.header("x-hub-signature", "sha1=legacy"),
            }
            .body(FailingBody)
            .unwrap()
        };

        assert_eq!(
            receiver().receive(request(None)).await.status(),
            StatusCode::UNAUTHORIZED
        );
        // A signed request reaches the read loop and reports the body failure.
        let signed = verifier().sign(b"{}");
        assert_eq!(
            receiver().receive(request(Some(&signed))).await.status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn refuses_an_unsigned_oversized_request_on_its_headers() {
        // The body's exact size hint is over the limit, so 413 would say the
        // limit was checked first; 401 says the signature headers were.
        let receiver = WebhookReceiverBuilder::new(verifier())
            .body_limit(64)
            .build(|_: Envelope| async { Ok::<_, ()>(()) });

        let unsigned = without_header(&[b' '; 65], "push", "x-hub-signature-256");
        let response = receiver.receive(unsigned).await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn stops_at_the_body_limit_before_authentication() {
        // Carrying the signature that earns 401 in
        // `maps_authentication_and_request_errors`: were it verified first,
        // the answer would be 401 here too. Once with an exact size hint, so
        // the limit answers before the first poll, and once streamed, so it
        // answers from inside the read loop.
        const OVERSIZED: &[u8] = &[b' '; 65];
        let receiver = || {
            WebhookReceiverBuilder::new(verifier())
                .body_limit(64)
                .build(|_: Envelope| async { Ok::<_, ()>(()) })
        };

        let hinted = Full::new(Bytes::from_static(OVERSIZED));
        let response = receiver()
            .receive(request_over(hinted, "push", WRONG_SIGNATURE))
            .await;
        assert_eq!(
            response.status(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "an exact size hint over the limit is refused before verification"
        );

        let streamed = Frames::data(&[&OVERSIZED[..32], &OVERSIZED[32..]]);
        let response = receiver()
            .receive(request_over(streamed, "push", WRONG_SIGNATURE))
            .await;
        assert_eq!(
            response.status(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "a streamed body crossing the limit is refused before verification"
        );
    }

    #[tokio::test]
    async fn enforces_the_body_limit_across_frames() {
        // Three frames of 5, 5 and 3 bytes: no frame is over an 8-byte limit
        // on its own, so only the running total can refuse the body. The
        // signature is over the concatenation, so a 204 also says the frames
        // were assembled in order.
        const CHUNKS: &[&[u8]] = &[br#"{"a":"#, br#"1,"b""#, b":2}"];
        const PAYLOAD: &[u8] = br#"{"a":1,"b":2}"#;
        let signed = verifier().sign(PAYLOAD);
        let receiver = |limit: usize| {
            WebhookReceiverBuilder::new(verifier())
                .body_limit(limit)
                .build(|_: Envelope| async { Ok::<_, ()>(()) })
        };

        let body = Frames::data(CHUNKS);
        let polls = body.polls();
        let response = receiver(8)
            .receive(request_over(body, "push", &signed))
            .await;
        assert_eq!(
            response.status(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "the second frame takes the total past the limit"
        );
        assert_eq!(
            polls.load(Ordering::Relaxed),
            2,
            "answered from inside the read loop at the frame that crossed the limit: \
             the size hint would have polled none, reading the body out would have polled four"
        );

        let body = Frames::data(CHUNKS);
        let response = receiver(PAYLOAD.len())
            .receive(request_over(body, "push", &signed))
            .await;
        assert_eq!(
            response.status(),
            StatusCode::NO_CONTENT,
            "a total exactly at the limit is within it"
        );
    }

    #[tokio::test]
    async fn trailers_do_not_count_toward_the_body_limit() {
        // Under a zero limit a single counted byte is refused, so a 204 says
        // the trailers were passed over; the signature is over the empty
        // payload, so it also says they left no bytes behind.
        let mut trailers = HeaderMap::new();
        trailers.insert("x-checksum", "crc32c=00000000".parse().unwrap());
        let body = Frames::new([Frame::trailers(trailers)]);
        let receiver = WebhookReceiverBuilder::new(verifier())
            .body_limit(0)
            .build(|_: Envelope| async { Ok::<_, ()>(()) });

        let response = receiver
            .receive(request_over(body, "push", &verifier().sign(b"")))
            .await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn handler_errors_return_bare_internal_server_errors() {
        let receiver = WebhookReceiverBuilder::new(verifier())
            .build(|_: Envelope| async { Err::<(), _>("private error") });

        let response = receiver.receive(request(b"{}", "push")).await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(response.body().size_hint().exact(), Some(0));
    }

    #[tokio::test]
    async fn the_error_observer_receives_the_event_meta_and_a_dispatchers_error_before_the_500() {
        use crate::DispatchError;

        // With a dispatcher as the receiver's handler, the observer's error is
        // the dispatcher's, `DispatchError<E>`; what that error says about the
        // tier and the registration site is the dispatcher's own test to pin.
        // This one pins the observer's side: it runs once, with the meta the
        // receiver built, probed fields included, and the error whose source
        // is the handler's own failure; and the response stays GitHub's
        // delivery record, a bare 500 that says nothing of either.
        let seen: Arc<std::sync::Mutex<Vec<(EventMeta, AppError)>>> = Arc::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .always(|_: Envelope| async { Err::<(), _>("audit") })
            .build();
        let observer_seen = Arc::clone(&seen);
        let receiver = WebhookReceiverBuilder::new(verifier())
            .on_error(move |meta: &EventMeta, error: &DispatchError<AppError>| {
                observer_seen
                    .lock()
                    .unwrap()
                    .push((meta.clone(), error.clone().into_source()));
            })
            .build(dispatcher);

        let response = receiver
            .receive(request(
                br#"{"action":"opened","installation":{"id":42}}"#,
                "pull_request",
            ))
            .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(response.body().size_hint().exact(), Some(0));

        let seen = seen.lock().unwrap();
        let [(meta, source)] = seen.as_slice() else {
            panic!("expected the observer to run once, saw {}", seen.len());
        };
        assert_eq!(meta.delivery_id, "delivery");
        assert_eq!(meta.kind, EventKind::PullRequest);
        assert_eq!(meta.action, Some(Action::Opened));
        assert_eq!(meta.installation_id, Some(42));
        assert_eq!(*source, AppError::Handler("audit"));
    }

    #[tokio::test]
    async fn the_error_observer_accepts_a_boxed_error_and_walks_its_source_chain() {
        use std::error::Error;

        // `Box<dyn Error + Send + Sync>` is not itself an `Error`, which is
        // what broke the wrapper recipe this observer replaces.
        type Boxed = Box<dyn Error + Send + Sync>;

        #[derive(Debug, thiserror::Error)]
        #[error("database is down")]
        struct Database;

        #[derive(Debug, thiserror::Error)]
        #[error("could not label the issue")]
        struct Label(#[source] Database);

        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observer_seen = Arc::clone(&seen);
        let receiver = WebhookReceiverBuilder::new(verifier())
            .on_error(move |meta: &EventMeta, error: &Boxed| {
                let mut lines = vec![format!("{} {}: {error}", meta.delivery_id, meta.kind)];
                let mut cause = error.source();
                while let Some(error) = cause {
                    lines.push(format!("  caused by: {error}"));
                    cause = error.source();
                }
                observer_seen.lock().unwrap().push(lines.join("\n"));
            })
            .build(|_: Envelope| async { Err::<(), Boxed>(Box::new(Label(Database))) });

        let response = receiver.receive(request(b"{}", "issues")).await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            ["delivery issues: could not label the issue\n  caused by: database is down"]
        );
    }

    #[tokio::test]
    async fn the_error_observer_places_no_bound_on_the_error_type() {
        // Neither `Error`, `Display` nor `Debug`: the observer still receives
        // it, and a consumer decides what to do with an opaque error.
        struct Opaque;

        let calls = Arc::new(AtomicUsize::new(0));
        let observer_calls = Arc::clone(&calls);
        let receiver = WebhookReceiverBuilder::new(verifier())
            .on_error(move |_: &EventMeta, _: &Opaque| {
                observer_calls.fetch_add(1, Ordering::Relaxed);
            })
            .build(|_: Envelope| async { Err::<(), _>(Opaque) });

        let response = receiver.receive(request(b"{}", "push")).await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn the_error_observer_is_silent_unless_the_handler_fails() {
        // Receive failures are status codes and span fields, not handler
        // errors; a short-circuited ping and a success never reach a
        // handler at all. None of them has an error to observe.
        let calls = Arc::new(AtomicUsize::new(0));
        let receiver = |handler_fails: bool| {
            let observer_calls = Arc::clone(&calls);
            WebhookReceiverBuilder::new(verifier())
                .body_limit(64)
                .on_error(move |_: &EventMeta, _: &&str| {
                    observer_calls.fetch_add(1, Ordering::Relaxed);
                })
                .build(move |_: Envelope| async move {
                    if handler_fails {
                        Err("private error")
                    } else {
                        Ok(())
                    }
                })
        };

        let response = receiver(true).receive(request(b"{}", "push")).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "a failing handler is observed"
        );
        calls.store(0, Ordering::Relaxed);

        let response = receiver(false).receive(request(b"{}", "push")).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = receiver(true).receive(request(b"{}", "ping")).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let mismatch = with_header(b"{}", "push", "x-hub-signature-256", WRONG_SIGNATURE);
        let response = receiver(true).receive(mismatch).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let unsigned = without_header(b"{}", "push", "x-hub-signature-256");
        let response = receiver(true).receive(unsigned).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let form = with_header(
            b"{}",
            "push",
            "content-type",
            "application/x-www-form-urlencoded",
        );
        let response = receiver(true).receive(form).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let no_delivery_id = without_header(b"{}", "push", "x-github-delivery");
        let response = receiver(true).receive(no_delivery_id).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let too_large = request(&[b' '; 65], "push");
        let response = receiver(true).receive(too_large).await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn debug_and_clone_do_not_constrain_the_handler_or_its_error() {
        // Neither the handler nor its error type reaches either impl: error
        // types are routinely not `Clone`, and the handler is not required
        // to be, either. The builder holds an observer over the error type
        // and is under the same rule.
        struct NotCloneOrDebug;

        let builder = WebhookReceiverBuilder::new(Verifier::new(Secret::new("super-secret")))
            .body_limit(64)
            .on_error(|_: &EventMeta, _: &NotCloneOrDebug| {});
        let debug = format!("{:?}", builder.clone());
        assert!(debug.contains("body_limit: 64"), "{debug}");
        assert!(debug.contains("on_error: true"), "{debug}");
        assert!(debug.contains("trace_errors: false"), "{debug}");
        assert!(!debug.contains("super-secret"), "{debug}");

        let receiver = builder.build(|_: Envelope| async { Err::<(), _>(NotCloneOrDebug) });

        let debug = format!("{:?}", receiver.clone());
        assert!(debug.contains("body_limit: 64"), "{debug}");
        assert!(debug.contains("on_error: true"), "{debug}");
        assert!(debug.contains("[REDACTED]"), "{debug}");
        assert!(!debug.contains("super-secret"), "{debug}");

        let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new("super-secret")))
            .build(Recorder {
                calls: Arc::new(AtomicUsize::new(0)),
            });
        let debug = format!("{:?}", receiver.clone());
        assert!(debug.contains("on_error: false"), "{debug}");
    }

    #[cfg(feature = "tracing")]
    #[test]
    fn debug_says_whether_the_failed_delivery_event_carries_the_error() {
        let receiver = WebhookReceiverBuilder::new(verifier())
            .trace_errors()
            .build(|_: Envelope| async { Err::<(), _>(std::fmt::Error) });
        let debug = format!("{receiver:?}");
        assert!(debug.contains("trace_errors: true"), "{debug}");

        let receiver = WebhookReceiverBuilder::new(verifier())
            .trace_boxed_errors()
            .build(|_: Envelope| async {
                Err::<(), Box<dyn std::error::Error + Send + Sync>>("boxed".into())
            });
        let debug = format!("{receiver:?}");
        assert!(debug.contains("trace_errors: true"), "{debug}");
    }

    #[test]
    fn response_contract_uses_empty_bodies() {
        let response = empty_response(ResponseStatus::BadRequest);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.body().size_hint().exact(), Some(0));
    }

    #[test]
    fn converts_every_status_to_the_matching_http_status_code() {
        for status in [
            ResponseStatus::NoContent,
            ResponseStatus::BadRequest,
            ResponseStatus::Unauthorized,
            ResponseStatus::PayloadTooLarge,
            ResponseStatus::InternalServerError,
        ] {
            assert_eq!(StatusCode::from(status).as_u16(), status.as_u16());
        }
    }

    #[test]
    fn labels_every_status_with_the_outcome_the_receive_span_records() {
        // The whole table, one row per status. The labels are the front
        // page's vocabulary for the receive span's `outcome`, and a dashboard
        // filters on them verbatim, so each is a literal here, not derived
        // from the variant's name. That the receiver records them on the
        // span, beside the code as `status`, is `tests/tracing_outcome.rs`'s
        // test.
        let table = [
            (ResponseStatus::NoContent, "ok"),
            (ResponseStatus::BadRequest, "bad_request"),
            (ResponseStatus::Unauthorized, "unauthorized"),
            (ResponseStatus::PayloadTooLarge, "payload_too_large"),
            (ResponseStatus::InternalServerError, "handler_error"),
        ];

        for (status, label) in table {
            assert_eq!(outcome_label(status), label, "{status:?}");
        }
    }
}
