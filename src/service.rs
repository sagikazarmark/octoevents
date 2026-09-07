#[cfg(feature = "tower")]
use std::{
    convert::Infallible,
    task::{Context, Poll},
};
use std::{fmt, future::Future, sync::Arc};

use bytes::{Bytes, BytesMut};
use http::{Request, Response};
use http_body::Body;
use http_body_util::{BodyExt as _, Empty};
#[cfg(feature = "tower")]
use tower_service::Service;

#[cfg(feature = "tower")]
use crate::runtime::BoxFuture;
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
    failed: trace::HandlerFailed<E>,
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
            .field("trace_errors", &self.failed.traces_error());
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
            failed: self.failed,
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
                failed: trace::HandlerFailed::bound_free(),
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

    /// Puts the handler's error on the ERROR event a failed delivery emits:
    /// its text as `error`, its source and the chain beneath as `source`.
    ///
    /// With the `tracing` feature the receiver emits one event at ERROR for
    /// every failed delivery, carrying the delivery's identifying fields and
    /// the status but, by default, nothing of the error, since it places no
    /// bound on the error type. This asks `E: Error` and records the error
    /// on that same event: `error` is its `Display` and `source` its
    /// [`source`](std::error::Error::source), an error value the subscriber
    /// renders with the sources beneath it (the `fmt` subscriber prints
    /// `error=<text> source=<cause> source.sources=[<cause>, ..]`). Still one
    /// event, in the `octoevents.receive` span; an [`on_error`](Self::on_error)
    /// observer, if any, runs beside it. The contract is under
    /// [Tracing](crate#tracing).
    ///
    /// `Error` rather than `Display`, because a `Display` bound cannot walk
    /// `source()`: the text of a [`DispatchError`](crate::DispatchError) says
    /// where the delivery failed, and why is its source, the application
    /// error. A `thiserror` enum qualifies, and so does a `DispatchError`
    /// over one. A `Box<dyn Error + Send + Sync>` does not, since std
    /// implements `Error` for `Box<E>` only for a sized `E`, and neither does
    /// a `DispatchError` over one; both are
    /// [`trace_boxed_errors`](Self::trace_boxed_errors)'.
    ///
    /// With a [`Dispatcher`](crate::Dispatcher) as the handler, the event for
    /// a failed delivery reads, on the `fmt` subscriber:
    ///
    /// ```text
    /// ERROR octoevents.receive{delivery_id="72d3162e-cc78-11e3-81ab-4c9367dc0958" event="issues" outcome="handler_error" status=500}: octoevents::trace: handler failed delivery_id="72d3162e-cc78-11e3-81ab-4c9367dc0958" event="issues" action="opened" installation_id=42 status=500 error=delivery 72d3162e-cc78-11e3-81ab-4c9367dc0958 (issues.opened) failed in the always tier at the handler `app::main::{{closure}}` registered at src/main.rs:12:6 source=database is down
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
        self.config.failed = trace::HandlerFailed::with_error();
        self
    }

    /// [`trace_errors`](Self::trace_errors) for an error behind a pointer,
    /// `Box<dyn Error + Send + Sync>` foremost, or a `DispatchError` over one.
    ///
    /// `Box<dyn Error + Send + Sync>` is not an `Error` (std implements
    /// `Error` for `Box<E>` only for a sized `E`), so a
    /// [`DispatchError`](crate::DispatchError) over it is not one either,
    /// and `trace_errors` refuses both. This asks [`BoxedError`] instead,
    /// which both are, along with `Arc<dyn Error + Send + Sync>`,
    /// `anyhow::Error` and `eyre::Report`. The event is the same one with
    /// the same two fields: for a boxed error, `error` is its text and
    /// `source` its own source; for a `DispatchError` over one, `error` is
    /// the dispatch error's text, saying where, and `source` the boxed
    /// error, saying why, with its chain rendered by the subscriber.
    ///
    /// The crate front page's receiver, whose handlers return
    /// `Box<dyn Error + Send + Sync>`:
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
        E: crate::BoxedError,
    {
        self.config.failed = trace::HandlerFailed::with_boxed_error();
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
            failed,
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
        // beforehand, and only when there is something to report to.
        let reporting = observer.is_some() || trace::ENABLED;
        let meta = reporting.then(|| envelope.meta.clone());
        match self.handler.handle(envelope).await {
            Ok(()) => record_outcome(ResponseStatus::NoContent),
            Err(error) => {
                // The outcome goes on the span first, so the event and the
                // observer run inside a span that already says how the
                // delivery ended.
                let status = record_outcome(ResponseStatus::InternalServerError);
                if let Some(meta) = &meta {
                    failed.emit(meta, &error, status.as_u16());
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
///         let _ = (envelope.meta.delivery_id, envelope.raw);
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
    type Future = BoxFuture<Result<ServiceResponse, Infallible>>;

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
    trace::record("outcome", status.label());
    trace::record("status", status.as_u16());
    status
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use bytes::Bytes;
    use hmac::{Hmac, KeyInit, Mac};
    use http::{Request, StatusCode};
    use http_body::Body as _;
    use http_body_util::Full;
    use sha2::Sha256;
    #[cfg(feature = "tower")]
    use tower::ServiceExt as _;

    use super::{WebhookReceiverBuilder, empty_response};
    use crate::{
        Action, DecodeError, Dispatcher, Envelope, Event, EventKind, EventMeta, Handler,
        ResponseStatus, Secret, Verifier, test_support::AppError,
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
    /// octocrab dependency, bound to its kind by `impl_payload!`.
    #[derive(serde::Deserialize)]
    struct IssueView {
        action: String,
        issue: IssueNumber,
    }

    #[derive(serde::Deserialize)]
    struct IssueNumber {
        number: u64,
    }

    crate::impl_payload!(IssueView => EventKind::Issues);

    /// The single-handler path: a handler over the envelope that decodes one
    /// kind's view itself with `decode_payload`, so a delivery of another
    /// kind or a payload that does not fit the view fails the delivery.
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

    fn request(body: &'static [u8], event: &str) -> Request<Full<Bytes>> {
        Request::builder()
            .header("content-type", "application/json")
            .header("x-github-delivery", "delivery")
            .header("x-github-event", event)
            .header("x-hub-signature-256", signature(b"secret", body))
            .body(Full::new(Bytes::from_static(body)))
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

    fn signature(secret: &[u8], body: &[u8]) -> String {
        use std::fmt::Write as _;

        let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
        mac.update(body);
        let tag = mac.finalize().into_bytes();
        let mut out = String::from("sha256=");
        for byte in tag {
            write!(out, "{byte:02x}").unwrap();
        }
        out
    }

    #[tokio::test]
    async fn returns_no_content_after_successful_dispatch() {
        let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
            .build(|_: Envelope| async { Ok::<_, ()>(()) });

        let response = receiver.receive(request(b"{}", "push")).await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn accepts_a_struct_handler_that_is_not_clone() {
        let calls = Arc::new(AtomicUsize::new(0));
        let receiver =
            WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(Recorder {
                calls: Arc::clone(&calls),
            });

        let response = receiver.receive(request(b"{}", "push")).await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn accepts_an_async_fn_item_over_the_envelope() {
        async fn count(envelope: Envelope) -> Result<(), std::convert::Infallible> {
            COUNTED_BYTES.fetch_add(envelope.raw.len(), Ordering::Relaxed);
            Ok(())
        }
        static COUNTED_BYTES: AtomicUsize = AtomicUsize::new(0);

        let receiver =
            WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(count);

        let response = receiver.receive(request(b"{}", "push")).await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(COUNTED_BYTES.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn accepts_an_arc_shared_struct_handler_and_leaves_the_caller_its_handle() {
        let recorder = Arc::new(Recorder {
            calls: Arc::new(AtomicUsize::new(0)),
        });
        let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
            .build(Arc::clone(&recorder));

        let response = receiver.receive(request(b"{}", "push")).await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(recorder.calls.load(Ordering::Relaxed), 1);
    }

    #[cfg(feature = "tower")]
    #[tokio::test]
    async fn the_tower_service_impl_applies_the_same_policy() {
        let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
            .build(|_: Envelope| async { Ok::<_, ()>(()) });

        let response = receiver.oneshot(request(b"{}", "push")).await.unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[cfg(feature = "tower")]
    #[tokio::test]
    async fn the_tower_service_impl_accepts_a_struct_handler() {
        let calls = Arc::new(AtomicUsize::new(0));
        let receiver =
            WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(Recorder {
                calls: Arc::clone(&calls),
            });

        let response = receiver.oneshot(request(b"{}", "push")).await.unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn a_single_kind_handler_receives_its_decoded_view_with_the_metadata() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(
            IssueRecorder {
                seen: Arc::clone(&seen),
            },
        );

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
    async fn a_single_kind_handler_fails_a_delivery_of_another_kind() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(
            IssueRecorder {
                seen: Arc::clone(&seen),
            },
        );

        // The body would decode as an IssueView; only the kind is wrong.
        let response = receiver
            .receive(request(
                br#"{"action":"opened","issue":{"number":7}}"#,
                "pull_request",
            ))
            .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_single_kind_handler_fails_a_delivery_whose_payload_does_not_decode() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(
            IssueRecorder {
                seen: Arc::clone(&seen),
            },
        );

        let response = receiver
            .receive(request(br#"{"action":"opened"}"#, "issues"))
            .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_always_handler_runs_for_a_payload_no_routed_handler_can_decode() {
        type Seen = Arc<std::sync::Mutex<Vec<(String, EventKind, Option<Action>)>>>;

        struct Auditor {
            seen: Seen,
        }

        impl Handler<Envelope> for Auditor {
            type Error = std::convert::Infallible;

            #[allow(clippy::unused_async_trait_impl)]
            async fn handle(&self, envelope: Envelope) -> Result<(), Self::Error> {
                let meta = envelope.meta;
                self.seen
                    .lock()
                    .unwrap()
                    .push((meta.delivery_id, meta.kind, meta.action));
                Ok(())
            }
        }

        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let dispatcher = Dispatcher::<AppError>::builder()
            .always(Auditor {
                seen: Arc::clone(&seen),
            })
            .build();
        let receiver =
            WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(dispatcher);

        // A pull request octocrab cannot represent: nothing is decoded on the
        // always tier's behalf, so the delivery still succeeds.
        let response = receiver
            .receive(request(
                include_bytes!("../tests/fixtures/unrepresentable.json"),
                "pull_request",
            ))
            .await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            [("delivery".to_owned(), EventKind::PullRequest, None)]
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
                        .push((envelope.meta.kind, envelope.raw));
                    Ok::<_, std::convert::Infallible>(())
                }
            })
            .build();
        let receiver =
            WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(dispatcher);

        let response = receiver.receive(request(BODY, "pull_request")).await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            [(EventKind::PullRequest, Bytes::from_static(BODY))]
        );
    }

    #[tokio::test]
    async fn accepts_a_dispatcher_as_its_handler() {
        // A consumer view over the ping payload: the dispatcher routes it by
        // the kind the view declares, with no octocrab in the picture.
        #[derive(serde::Deserialize)]
        struct Zen {
            zen: String,
        }
        crate::impl_payload!(Zen => EventKind::Ping);

        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let handler_seen = Arc::clone(&seen);
        let dispatcher = Dispatcher::<AppError>::builder()
            .on_payload(move |Event { meta, payload }: Event<Zen>| {
                let seen = Arc::clone(&handler_seen);
                async move {
                    seen.lock().unwrap().push((meta.kind, payload.zen));
                    Ok::<_, std::convert::Infallible>(())
                }
            })
            .build();
        let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
            .handle_ping(true)
            .build(dispatcher);

        let response = receiver
            .receive(request(
                include_bytes!("../tests/fixtures/ping.json"),
                "ping",
            ))
            .await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            [(EventKind::Ping, "Design for failure.".to_owned())]
        );
    }

    #[cfg(feature = "octocrab")]
    #[tokio::test]
    async fn a_handler_over_event_of_webhook_event_receives_octocrabs_decoded_event_with_the_metadata()
     {
        use octocrab::models::webhook_events::{WebhookEvent, WebhookEventPayload};

        type Seen = Arc<std::sync::Mutex<Vec<(String, Option<u64>, u64)>>>;

        struct EventRecorder {
            seen: Seen,
        }

        impl Handler<Event<WebhookEvent>> for EventRecorder {
            type Error = std::convert::Infallible;

            #[allow(clippy::unused_async_trait_impl)]
            async fn handle(
                &self,
                Event { meta, payload }: Event<WebhookEvent>,
            ) -> Result<(), Self::Error> {
                let WebhookEventPayload::PullRequest(pull_request) = payload.specific else {
                    panic!("expected a pull request payload");
                };
                self.seen.lock().unwrap().push((
                    meta.delivery_id,
                    meta.installation_id,
                    pull_request.number,
                ));
                Ok(())
            }
        }

        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let dispatcher = Dispatcher::<AppError>::builder()
            .on(
                EventKind::PullRequest,
                EventRecorder {
                    seen: Arc::clone(&seen),
                },
            )
            .build();
        let receiver =
            WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(dispatcher);

        let response = receiver
            .receive(request(
                include_bytes!("../tests/fixtures/pull_request.opened.json"),
                "pull_request",
            ))
            .await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            [("delivery".to_owned(), Some(7_777_777), 2)]
        );
    }

    #[cfg(feature = "octocrab")]
    #[tokio::test]
    async fn a_handler_over_octocrabs_payload_receives_it_for_its_kind() {
        use octocrab::models::webhook_events::payload::{
            PullRequestWebhookEventAction, PullRequestWebhookEventPayload,
        };

        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let handler_seen = Arc::clone(&seen);
        let dispatcher = Dispatcher::<AppError>::builder()
            .on_payload(
                move |Event { meta, payload }: Event<PullRequestWebhookEventPayload>| {
                    let seen = Arc::clone(&handler_seen);
                    async move {
                        seen.lock().unwrap().push((
                            meta.delivery_id,
                            payload.number,
                            payload.action,
                            payload.pull_request.title,
                        ));
                        Ok::<_, std::convert::Infallible>(())
                    }
                },
            )
            .build();
        let receiver =
            WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(dispatcher);

        let response = receiver
            .receive(request(
                include_bytes!("../tests/fixtures/pull_request.opened.json"),
                "pull_request",
            ))
            .await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            [(
                "delivery".to_owned(),
                2,
                PullRequestWebhookEventAction::Opened,
                Some("[do not merge] test commit".to_owned()),
            )]
        );
    }

    #[tokio::test]
    async fn short_circuits_ping_unless_enabled() {
        let calls = Arc::new(AtomicUsize::new(0));
        let handler_calls = Arc::clone(&calls);
        let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(
            move |_: Envelope| {
                handler_calls.fetch_add(1, Ordering::Relaxed);
                async { Ok::<_, ()>(()) }
            },
        );

        let response = receiver.receive(request(b"{}", "ping")).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(calls.load(Ordering::Relaxed), 0);

        let handler_calls = Arc::clone(&calls);
        let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
            .handle_ping(true)
            .build(move |_: Envelope| {
                handler_calls.fetch_add(1, Ordering::Relaxed);
                async { Ok::<_, ()>(()) }
            });
        receiver.receive(request(b"{}", "ping")).await;
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn maps_authentication_and_request_errors() {
        let receiver = || {
            WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
                .build(|_: Envelope| async { Ok::<_, ()>(()) })
        };

        let mismatch = with_header(b"{}", "push", "x-hub-signature-256", WRONG_SIGNATURE);
        assert_eq!(
            receiver().receive(mismatch).await.status(),
            StatusCode::UNAUTHORIZED
        );

        let mut sha1_only = without_header(b"{}", "push", "x-hub-signature-256");
        sha1_only
            .headers_mut()
            .insert("x-hub-signature", "sha1=legacy".parse().unwrap());
        assert_eq!(
            receiver().receive(sha1_only).await.status(),
            StatusCode::UNAUTHORIZED
        );

        let malformed = with_header(b"{}", "push", "x-hub-signature-256", "invalid");
        assert_eq!(
            receiver().receive(malformed).await.status(),
            StatusCode::BAD_REQUEST
        );

        let form = with_header(
            b"{}",
            "push",
            "content-type",
            "application/x-www-form-urlencoded",
        );
        assert_eq!(
            receiver().receive(form).await.status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn refuses_an_unsigned_request_without_reading_the_body() {
        use std::{
            pin::Pin,
            task::{Context, Poll},
        };

        // A body that fails on first poll: reaching the read loop shows up as
        // 400, so a 401 proves the signature headers were decisive alone.
        struct FailingBody;

        impl http_body::Body for FailingBody {
            type Data = Bytes;
            type Error = &'static str;

            fn poll_frame(
                self: Pin<&mut Self>,
                _context: &mut Context<'_>,
            ) -> Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
                Poll::Ready(Some(Err("body must not be read")))
            }
        }

        let receiver = || {
            WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
                .build(|_: Envelope| async { Ok::<_, ()>(()) })
        };
        let request = |signature: Option<&str>| {
            let builder = Request::builder()
                .header("content-type", "application/json")
                .header("x-github-delivery", "delivery")
                .header("x-github-event", "push");
            match signature {
                Some(signature) => builder.header("x-hub-signature-256", signature),
                None => builder,
            }
            .body(FailingBody)
            .unwrap()
        };

        assert_eq!(
            receiver().receive(request(None)).await.status(),
            StatusCode::UNAUTHORIZED
        );
        // A signed request reaches the read loop and reports the body failure.
        let signed = signature(b"secret", b"{}");
        assert_eq!(
            receiver().receive(request(Some(&signed))).await.status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn stops_at_the_body_limit_before_authentication() {
        let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
            .body_limit(1)
            .build(|_: Envelope| async { Ok::<_, ()>(()) });

        let response = receiver.receive(request(b"{}", "push")).await;

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn handler_errors_return_bare_internal_server_errors() {
        let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
            .build(|_: Envelope| async { Err::<(), _>("private error") });

        let response = receiver.receive(request(b"{}", "push")).await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(response.body().size_hint().exact(), Some(0));
    }

    #[tokio::test]
    async fn a_dispatch_error_is_still_a_bare_internal_server_error() {
        // The dispatcher's error names the tier, the handler and the
        // registration site; none of it reaches the response, which stays
        // GitHub's delivery record. The `on_error` observer is where a
        // consumer reads it.
        let dispatcher = Dispatcher::<AppError>::builder()
            .always(|_: Envelope| async { Err::<(), _>("audit") })
            .build();
        let receiver =
            WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(dispatcher);

        let response = receiver.receive(request(b"{}", "push")).await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(response.body().size_hint().exact(), Some(0));
    }

    #[tokio::test]
    async fn the_error_observer_sees_the_event_meta_and_the_dispatch_error_before_the_500() {
        use crate::{DispatchError, Tier};

        type Seen = Arc<std::sync::Mutex<Vec<(EventMeta, DispatchError<AppError>)>>>;

        let seen: Seen = Arc::default();
        let dispatcher = Dispatcher::<AppError>::builder()
            .always(|_: Envelope| async { Err::<(), _>("audit") })
            .build();
        let observer_seen = Arc::clone(&seen);
        let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
            .on_error(move |meta: &EventMeta, error: &DispatchError<AppError>| {
                observer_seen
                    .lock()
                    .unwrap()
                    .push((meta.clone(), error.clone()));
            })
            .build(dispatcher);

        let response = receiver
            .receive(request(
                br#"{"action":"opened","installation":{"id":42}}"#,
                "pull_request",
            ))
            .await;

        // The response is still GitHub's delivery record: a bare 500.
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(response.body().size_hint().exact(), Some(0));

        let seen = seen.lock().unwrap();
        let [(meta, error)] = seen.as_slice() else {
            panic!("expected the observer to run once, saw {}", seen.len());
        };
        assert_eq!(meta.delivery_id, "delivery");
        assert_eq!(meta.kind, EventKind::PullRequest);
        assert_eq!(meta.action, Some(Action::Opened));
        assert_eq!(meta.installation_id, Some(42));
        assert_eq!(error.tier, Tier::Always);
        assert_eq!(error.source, AppError::Handler("audit"));
        assert_eq!(error.delivery_id, "delivery");
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
        let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
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
        let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
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
            WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
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
        let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
            .trace_errors()
            .build(|_: Envelope| async { Err::<(), _>(std::fmt::Error) });
        let debug = format!("{receiver:?}");
        assert!(debug.contains("trace_errors: true"), "{debug}");

        let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
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
}
