#[cfg(feature = "http-body")]
use std::pin::pin;
#[cfg(feature = "tower")]
use std::{
    convert::Infallible,
    task::{Context, Poll},
};
use std::{fmt, future::Future, sync::Arc};

use bytes::Bytes;
#[cfg(feature = "http-body")]
use bytes::BytesMut;
use http::{HeaderMap, StatusCode};
#[cfg(feature = "http-body")]
use http::{Request, Response};
#[cfg(feature = "http-body")]
use http_body::Body;
#[cfg(feature = "http-body")]
use http_body_util::{BodyExt as _, Empty};
#[cfg(feature = "tower")]
use tower_service::Service;

#[cfg(feature = "tracing")]
use crate::Action;
#[cfg(feature = "http-body")]
use crate::BodyError;
#[cfg(feature = "tower")]
use crate::runtime::BoxFuture;
use crate::{
    BoxError, DEFAULT_BODY_LIMIT, Envelope, EventKind, EventMeta, Handler, MaybeSend, MaybeSync,
    ReceiveError, Verifier, envelope::Refusal, header, trace,
};

#[cfg(feature = "http-body")]
type ReceiveResponse = Response<Empty<Bytes>>;

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
}

impl<E> Config<E> {
    fn debug_fields(&self, debug: &mut fmt::DebugStruct<'_, '_>) {
        // The observer is a closure and never `Debug`; whether one is
        // registered is the configuration worth printing.
        debug
            .field("verifier", &self.verifier)
            .field("body_limit", &self.body_limit)
            .field("handle_ping", &self.handle_ping)
            .field("on_error", &self.observer.is_some());
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
            },
        }
    }

    /// Sets the maximum body size, in bytes, of a request the receiver
    /// accepts.
    ///
    /// On `receive` it is the most the receiver accumulates from the
    /// transport: a body whose size hint is already over is refused before
    /// the first frame, and the read stops at the frame that would carry the
    /// total past the limit, so the receiver holds at most the limit plus
    /// one frame. How large a frame the transport yields is the transport's
    /// own bound (hyper's and axum's are small; a body that hands over its
    /// whole payload as one frame hands it over whatever the limit). On
    /// `receive_bytes` the body is the caller's already, and the limit is
    /// checked against its length. Either way a body over it is
    /// [`ReceiveError::BodyTooLarge`](crate::ReceiveError::BodyTooLarge),
    /// 413. GitHub never sends payloads above [`DEFAULT_BODY_LIMIT`]. Lower
    /// values reduce memory exposure when an application's real events are
    /// smaller; raising the limit does not enable larger GitHub deliveries.
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
    /// The response stays a bare 500 either way: the observer is for what
    /// tracing does not do (a metric, a dead letter, a line on stderr for a
    /// binary without a subscriber), not a way to change the answer. It is
    /// synchronous and returns nothing.
    ///
    /// `E` is the handler's error type, fixed by [`build`](Self::build), and
    /// the observer sees it as the handler returned it, before the receiver
    /// boxes it: a named enum is matched on, a `DispatchError`'s fields are
    /// read. Annotate the error parameter (`error: &AppError`) when the body
    /// calls methods on it: `build` comes later in the chain than the closure,
    /// so rustc cannot read the type off it there. A body that only formats
    /// the error needs no annotation.
    ///
    /// The observer runs only when a handler ran and failed. A receive
    /// failure (a signature that does not verify, a missing header, an
    /// unsupported content type, a body frame the transport could not
    /// produce, a body over the limit) is a status code and fields on the
    /// receive span, never a handler error, and a `ping` short-circuited by
    /// [`handle_ping`](Self::handle_ping) reaches no handler; neither calls
    /// it.
    ///
    /// With the `tracing` feature, a failed delivery emits one ERROR event
    /// naming the delivery and carrying the error, observer or not; the
    /// observer runs beside it and changes nothing about it. The contract is
    /// under [Tracing](crate#tracing).
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
    /// # #[derive(Debug, thiserror::Error)]
    /// # enum AppError {
    /// #     #[error("database is down")]
    /// #     Database,
    /// # }
    ///
    /// let dispatcher = Dispatcher::builder()
    ///     .always(|_: Envelope| async { Err::<(), _>(AppError::Database) })
    ///     .build();
    ///
    /// // A failed delivery logs, before the 500:
    /// //   delivery 72d3162e-cc78-11e3-81ab-4c9367dc0958 (issues.opened) failed in the always tier at the handler `app::main::{{closure}}` registered at src/main.rs:12:6
    /// //     caused by: database is down
    /// let receiver = WebhookReceiverBuilder::new(Verifier::new(WebhookSecret::new("current secret")))
    ///     .on_error(|_: &EventMeta, error: &DispatchError| {
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
    ///
    /// The application error behind a dispatch error is boxed; an observer
    /// that wants its own type back downcasts the source:
    ///
    /// ```
    /// use std::error::Error as _;
    ///
    /// use octoevents::{DispatchError, Dispatcher, Envelope, EventMeta, Verifier, WebhookReceiverBuilder, WebhookSecret};
    /// # #[derive(Debug, thiserror::Error)]
    /// # enum AppError {
    /// #     #[error("database is down")]
    /// #     Database,
    /// # }
    ///
    /// let dispatcher = Dispatcher::builder()
    ///     .always(|_: Envelope| async { Err::<(), _>(AppError::Database) })
    ///     .build();
    ///
    /// let receiver = WebhookReceiverBuilder::new(Verifier::new(WebhookSecret::new("current secret")))
    ///     .on_error(|_: &EventMeta, error: &DispatchError| {
    ///         if let Some(AppError::Database) = error.source.downcast_ref::<AppError>() {
    ///             // page the on-call
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

    /// Builds a receiver around one caller-owned handler.
    ///
    /// The handler is any [`Handler`] over the [`Envelope`]: an `async fn`
    /// taking the envelope, a struct with dependencies, a closure, or a
    /// `Dispatcher`. It does not need to be `Clone`.
    ///
    /// Its error is anything that converts into [`BoxError`]: an `Error`
    /// type, `BoxError` itself, `anyhow::Error`, a `String`. A handler error
    /// is answered with a bare 500, the response being GitHub's delivery
    /// record and not a log; the error goes to the
    /// [`on_error`](Self::on_error) observer as the handler returned it and,
    /// with the `tracing` feature, boxed onto the failed-delivery event.
    ///
    /// [`BoxError`]: crate::BoxError
    #[must_use]
    pub fn build<H>(self, handler: H) -> WebhookReceiver<H>
    where
        H: Handler<Envelope, Error = E> + MaybeSend + MaybeSync + 'static,
        E: Into<BoxError>,
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
/// Two entry points over one policy. [`receive`] (`http-body` feature, on by
/// default) takes an `http::Request` whose body is an `http_body::Body` and
/// answers with an `http::Response`; enabling the `tower` feature
/// additionally implements `tower_service::Service` over it, for routers
/// that want that. [`WebhookReceiver::receive_bytes`], in the core under
/// every feature set, takes the request's `http::HeaderMap` and its body as
/// [`Bytes`] already read, and answers with the `http::StatusCode`, for a
/// transport with no `http_body::Body`, which is what every surveyed
/// serverless runtime hands over.
///
/// The caller's router remains responsible for paths and methods. Responses
/// intentionally have empty bodies: handler details belong in logs, not in the
/// delivery record GitHub stores, and the builder's `on_error` observer is
/// where they are handed over.
///
// `receive` exists only under `http-body`; its link is an intra-doc path when
// it is compiled in and its docs.rs URL when it is not, as the front page does.
#[cfg_attr(feature = "http-body", doc = "[`receive`]: WebhookReceiver::receive")]
#[cfg_attr(
    not(feature = "http-body"),
    doc = "[`receive`]: https://docs.rs/octoevents/latest/octoevents/struct.WebhookReceiver.html#method.receive"
)]
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
    H::Error: Into<BoxError>,
{
    /// Authenticates, bounds, and dispatches one request.
    ///
    /// The request is any `http::Request` whose body yields [`Bytes`]:
    /// axum's, a Cloudflare Worker's, or a `String` in a test, which drives
    /// the receiver with a signed synthetic request and no server.
    /// [`Verifier::sign`] gives the [`Signature`](crate::Signature) GitHub
    /// would send for the body, which goes on the request as its header
    /// value, and a request needs the four headers [`header`](crate::header)
    /// names:
    ///
    /// ```
    /// use octoevents::{Dispatcher, Verifier, WebhookReceiverBuilder, WebhookSecret, header};
    ///
    /// # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
    /// let dispatcher = Dispatcher::builder().build();
    /// let verifier = Verifier::new(WebhookSecret::new("test-secret"));
    /// let webhook = WebhookReceiverBuilder::new(verifier.clone()).build(dispatcher);
    ///
    /// let body = r#"{"action":"opened","sender":{"id":1,"login":"octocat"}}"#;
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
    /// the concrete handler, and for a [`Dispatcher`], whose boxed error type
    /// the receiver's state names, that proof fails inside an `async move`
    /// block with "implementation of `Send` is not general enough".
    ///
    /// [`Dispatcher`]: crate::Dispatcher
    // Written as `fn -> impl Future` for the bound on the return type; the
    // body is the `async` block an `async fn` would desugar to.
    #[cfg(feature = "http-body")]
    #[expect(clippy::manual_async_fn)]
    pub fn receive<B>(
        &self,
        request: Request<B>,
    ) -> impl Future<Output = ReceiveResponse> + MaybeSend
    where
        B: Body<Data = Bytes> + MaybeSend,
        B::Error: fmt::Display,
    {
        async move { empty_response(self.inner.process_request(request).await) }
    }

    /// Authenticates, bounds, and dispatches one request whose body is
    /// already in hand, answering with the status.
    ///
    /// The same contract as [`receive`], for a transport with no
    /// `http_body::Body`: the request's `http::HeaderMap` and its body as
    /// [`Bytes`], which is the shape every surveyed Rust runtime hands over
    /// (`lambda_http`, `spin-sdk` and `wstd` give an `http::Request` with the
    /// body read, `worker` and `fastly` convert to one, `aws_lambda_events`
    /// carries a `HeaderMap` and a body string in its event structs), and
    /// the `http::StatusCode` to answer with, since a response type is the
    /// transport's. In order: a request whose signature header is absent
    /// (401) or not a signature (400) is refused from the headers; a body
    /// over the limit is 413; then [`Envelope::from_signed`] verifies and
    /// builds the envelope, a verified `ping` is 204 unless the builder was
    /// asked to `handle_ping`, and the handler runs, 204 when it succeeds
    /// and 500 when it fails, after the `on_error` observer and the
    /// failed-delivery event. Every span and field the `tracing` feature
    /// records on `receive` is recorded here.
    ///
    /// A transport that streams its body and wants to refuse unsigned
    /// traffic before buffering runs the header check itself, as the
    /// receiver does: `Signature::try_from` on the
    /// [`header::SIGNATURE`](crate::header::SIGNATURE) value, answered with
    /// [`ReceiveError::status`]; then this method repeats the check on the
    /// 71-byte header, which is cheaper than handing the parsed value across.
    ///
    /// ```
    /// use http::HeaderMap;
    /// use octoevents::{Bytes, Dispatcher, Verifier, WebhookReceiverBuilder, WebhookSecret, header};
    ///
    /// # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
    /// let dispatcher = Dispatcher::builder().build();
    /// let verifier = Verifier::new(WebhookSecret::new("test-secret"));
    /// let webhook = WebhookReceiverBuilder::new(verifier.clone()).build(dispatcher);
    ///
    /// // What a serverless runtime hands over: the headers and the body, read.
    /// let body = Bytes::from_static(br#"{"action":"opened","sender":{"id":1,"login":"octocat"}}"#);
    /// let mut headers = HeaderMap::new();
    /// headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    /// headers.insert(header::DELIVERY_ID, "delivery-1".parse().unwrap());
    /// headers.insert(header::EVENT_NAME, "issues".parse().unwrap());
    /// headers.insert(header::SIGNATURE, verifier.sign(&body).into());
    ///
    /// let status = webhook.receive_bytes(&headers, body).await;
    ///
    /// assert_eq!(status, 204);
    /// # });
    /// ```
    ///
    /// The future is `Send` on native targets whenever the handler is, as
    /// [`receive`]'s is, and for the same reason.
    ///
    #[cfg_attr(feature = "http-body", doc = "[`receive`]: WebhookReceiver::receive")]
    #[cfg_attr(
        not(feature = "http-body"),
        doc = "[`receive`]: https://docs.rs/octoevents/latest/octoevents/struct.WebhookReceiver.html#method.receive"
    )]
    // Written as `fn -> impl Future` for the bound on the return type; the
    // body is the `async` block an `async fn` would desugar to.
    #[expect(clippy::manual_async_fn)]
    pub fn receive_bytes(
        &self,
        headers: &HeaderMap,
        body: Bytes,
    ) -> impl Future<Output = StatusCode> + MaybeSend {
        async move { self.inner.process_bytes(headers, body).await }
    }
}

impl<H> Inner<H>
where
    H: Handler<Envelope>,
    H::Error: Into<BoxError>,
{
    /// The receiving path over an `http::Request`: the body is read from the
    /// transport, within the limit, once the headers have passed.
    #[cfg(feature = "http-body")]
    async fn process_request<B>(&self, request: Request<B>) -> StatusCode
    where
        B: Body<Data = Bytes>,
        B::Error: fmt::Display,
    {
        let (parts, body) = request.into_parts();
        let limit = self.config.body_limit;
        self.process(&parts.headers, read_body(body, limit)).await
    }

    /// The receiving path over a body already read: the limit is checked on
    /// its length, once the headers have passed.
    async fn process_bytes(&self, headers: &HeaderMap, body: Bytes) -> StatusCode {
        let limit = self.config.body_limit;
        self.process(headers, async move {
            if body.len() > limit {
                Err(ReceiveError::BodyTooLarge { limit })
            } else {
                Ok(body)
            }
        })
        .await
    }

    /// The receiving contract, inside the receive span, with the body as a
    /// future so the two paths differ only in how it is produced: the
    /// header-only refusal runs first, and `body` is awaited only for a
    /// request that passed it. On the request path that is what keeps
    /// unsigned traffic from occupying `body_limit` bytes of memory; on the
    /// bytes path the caller holds them already, and the refusal spares the
    /// verification.
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
    async fn process(
        &self,
        headers: &HeaderMap,
        body: impl Future<Output = Result<Bytes, ReceiveError>>,
    ) -> StatusCode {
        record_headers(headers);

        // A request whose signature header is absent or not a signature is
        // refused on the headers alone. `Envelope::from_signed` repeats the
        // check for transports that construct envelopes directly, and parses
        // the header again for the verifier; the header is 71 bytes, so the
        // second parse is cheaper than handing the first one across.
        if let Err(error) = header::signature(headers) {
            return refuse(&error.into());
        }

        let Config {
            verifier,
            handle_ping,
            observer,
            ..
        } = &self.config;

        let bytes = match body.await {
            Ok(bytes) => bytes,
            Err(error) => return refuse(&error),
        };

        let envelope = match Envelope::from_signed(verifier, headers, bytes) {
            Ok(envelope) => envelope,
            Err(error) => return refuse(&error),
        };

        if !handle_ping && matches!(envelope.meta.kind, EventKind::Ping) {
            return record_outcome("ok", StatusCode::NO_CONTENT);
        }

        // The handler takes the envelope by value, so the meta a failure is
        // reported with, to the observer and to the tracing event, is cloned
        // beforehand, and only when there is something to report to: with
        // the `tracing` feature the failed-delivery event always is.
        let reporting = observer.is_some() || cfg!(feature = "tracing");
        let meta = reporting.then(|| envelope.meta.clone());
        match self.handler.handle(envelope).await {
            Ok(()) => record_outcome("ok", StatusCode::NO_CONTENT),
            Err(error) => {
                // The outcome goes on the span first, so the observer and the
                // event run inside a span that already says how the delivery
                // ended. The observer sees the error as the handler returned
                // it, so it runs before the conversion the event needs.
                let status = record_outcome("handler_error", StatusCode::INTERNAL_SERVER_ERROR);
                if let Some(meta) = &meta {
                    if let Some(observer) = observer {
                        observer(meta, &error);
                    }
                    handler_failed(meta, error);
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
    H::Error: Into<BoxError>,
    B: Body<Data = Bytes> + MaybeSend + 'static,
    B::Error: fmt::Display,
{
    type Response = ReceiveResponse;
    type Error = Infallible;
    type Future = BoxFuture<'static, Result<ReceiveResponse, Infallible>>;

    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<B>) -> Self::Future {
        // The boxed future must be `'static`, so it owns a reference-counted
        // handle to the receiver state rather than borrowing `self`.
        let inner = Arc::clone(&self.inner);

        Box::pin(async move { Ok(empty_response(inner.process_request(request).await)) })
    }
}

#[cfg(feature = "http-body")]
fn empty_response(status: StatusCode) -> ReceiveResponse {
    Response::builder()
        .status(status)
        .body(Empty::new())
        .expect("an empty response with a fixed status always builds")
}

/// Reads `body` into memory, within `limit` bytes.
///
/// The one place the receiver touches the transport, so every way a body can
/// fail to arrive has its `ReceiveError` here. A body whose size hint is
/// already over the limit is refused before the first poll, one that crosses
/// it mid-stream at the frame that crossed, both as
/// [`ReceiveError::BodyTooLarge`]; a frame the transport could not produce is
/// [`ReceiveError::BodyRead`], with the transport's error as text. Trailers
/// are passed over and do not count toward the limit. The limit bounds the
/// accumulator, not a frame: a frame is the transport's allocation and has
/// arrived before its length can be read, so the crossing frame is held for
/// the length of the check and dropped with the error.
#[cfg(feature = "http-body")]
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

/// Records `delivery_id` and `event` on the receive span as soon as the
/// headers are read, before verification, so a refused request is still
/// identifiable. Nothing else off the headers is recorded, the signature
/// least of all.
fn record_headers(headers: &HeaderMap) {
    if let Some(delivery_id) = header::read(headers, &header::DELIVERY_ID) {
        trace::record("delivery_id", delivery_id);
    }
    if let Some(event) = header::read(headers, &header::EVENT_NAME) {
        trace::record("event", event);
    }
}

/// Records how the delivery ended on the receive span: `outcome`, the label
/// the front page's vocabulary gives that ending, and `status`, the HTTP code
/// it is answered with. The two are chosen together at the site that knows
/// why the delivery ended, and neither is derived from the other: the label
/// says the reason, the code what the reason is answered with.
fn record_outcome(outcome: &'static str, status: StatusCode) -> StatusCode {
    trace::record("outcome", outcome);
    trace::record("status", status.as_u16());
    status
}

/// Answers a request refused before any handler ran: the status the contract
/// maps `error` to, recorded as the span's outcome under the refusal's
/// label, and the error's text as the span's `error`, so the span says which
/// refusal it was where `outcome` says only its class. Every pre-handler
/// failure is an error value and goes through here, so none selects a status
/// on its own; the label and the status are read off the one `Refusal` the
/// error partitions into, so a dashboard's `outcome` and the code cannot
/// drift apart.
fn refuse(error: &ReceiveError) -> StatusCode {
    let refusal = error.refusal();
    let status = record_outcome(refusal_label(refusal), refusal.status());
    record_refusal(error);
    status
}

/// The `outcome` the receive span records for a refusal: the front page's
/// vocabulary, which a dashboard filters on verbatim, so each is a literal
/// here and not derived from the status. One label per class, over the
/// exhaustive `Refusal`, so a class this crate adds fails to compile here
/// until it has a label.
const fn refusal_label(refusal: Refusal) -> &'static str {
    match refusal {
        Refusal::Unauthorized => "unauthorized",
        Refusal::BadRequest => "bad_request",
        Refusal::PayloadTooLarge => "payload_too_large",
    }
}

/// Records the refusal's text on the receive span as `error`, through
/// `tracing::field::display`, the form the failed-delivery event fixed for
/// that name, so `error` is one field to a subscriber wherever it appears.
///
/// The text alone, and unconditionally: every `ReceiveError` message is the
/// crate's own fixed wording, so it can carry nothing from the request. Its
/// source is not recorded. Beneath `BodyRead` that is the transport's text,
/// which is the transport's to write and could quote the request, signature
/// included; it stays on the error value. The failed-delivery event records
/// its `error` as an error value instead, chain included: a handler's error
/// is the application's own text, and the chain is what an operator reads it
/// for.
#[cfg(feature = "tracing")]
fn record_refusal(error: &ReceiveError) {
    trace::record("error", tracing::field::display(error));
}

/// Records nothing: the `tracing` feature is disabled.
#[cfg(not(feature = "tracing"))]
fn record_refusal(_error: &ReceiveError) {}

/// The one `tracing::error!` for a failed delivery, so the event's fields
/// are declared in one place.
///
/// Takes the handler's error by value and boxes it here, so the conversion
/// happens only with the feature, after the observer has seen the error as
/// the handler returned it. `error` is recorded as an error value: the
/// subscriber renders its text and walks its `source()` chain itself (the
/// `fmt` subscriber prints `error=<text> error.sources=[<cause>, ..]`), so
/// with a dispatcher the text says where the delivery failed and the chain
/// beneath says why. The identifying fields it shares with the spans
/// (`delivery_id`, `event`, `action`, `installation_id`) are recorded in the
/// forms `trace` fixes for them.
///
/// No `status`: a handler failure is always answered 500, so the field
/// would say what the event's name already does, and the code is on the
/// receive span the event is emitted inside, beside `outcome`.
#[cfg(feature = "tracing")]
fn handler_failed<E: Into<BoxError>>(meta: &EventMeta, error: E) {
    let error: BoxError = error.into();
    tracing::error!(
        delivery_id = meta.delivery_id.as_str(),
        event = meta.kind.as_str(),
        action = meta.action.as_ref().map(Action::as_str),
        installation_id = meta.installation_id,
        error = &*error as &(dyn std::error::Error + 'static),
        "handler failed"
    );
}

/// Emits nothing: the `tracing` feature is disabled, and the error is
/// dropped unboxed.
#[cfg(not(feature = "tracing"))]
fn handler_failed<E: Into<BoxError>>(_meta: &EventMeta, _error: E) {}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
