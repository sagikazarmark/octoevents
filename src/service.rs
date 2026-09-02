#[cfg(feature = "tower")]
use std::{
    convert::Infallible,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use std::{fmt, marker::PhantomData};

use bytes::{Bytes, BytesMut};
use http::{Request, Response};
use http_body::Body;
use http_body_util::{BodyExt as _, Empty};
#[cfg(feature = "tower")]
use tower_service::Service;

use crate::{
    DEFAULT_BODY_LIMIT, Envelope, EventKind, HeaderView, ReceiveError, ResponseStatus, Verifier,
    WebhookHandler,
};

type ServiceResponse = Response<Empty<Bytes>>;
#[cfg(feature = "tower")]
type ServiceFuture =
    Pin<Box<dyn Future<Output = Result<ServiceResponse, Infallible>> + Send + 'static>>;

/// Builds a [`WebhookReceiver`].
#[derive(Debug, Clone)]
pub struct WebhookReceiverBuilder {
    verifier: Verifier,
    body_limit: usize,
    handle_ping: bool,
}

impl WebhookReceiverBuilder {
    /// Creates a builder with GitHub's 25 MiB payload cap and ping short-circuiting.
    ///
    /// The verifier is required rather than configurable: GitHub webhooks
    /// without a secret are intentionally unsupported, so a receiver that
    /// cannot authenticate is not constructible.
    #[must_use]
    pub fn new(verifier: Verifier) -> Self {
        Self {
            verifier,
            body_limit: DEFAULT_BODY_LIMIT,
            handle_ping: false,
        }
    }

    /// Sets the maximum bytes read from an unauthenticated request.
    ///
    /// GitHub never sends payloads above [`DEFAULT_BODY_LIMIT`]. Lower values
    /// reduce memory exposure when an application's real events are smaller;
    /// raising the limit does not enable larger GitHub deliveries.
    #[must_use]
    pub const fn body_limit(mut self, limit: usize) -> Self {
        self.body_limit = limit;
        self
    }

    /// Controls whether verified `ping` events reach the handler.
    #[must_use]
    pub const fn handle_ping(mut self, handle: bool) -> Self {
        self.handle_ping = handle;
        self
    }

    /// Builds a service around one caller-owned handler.
    #[must_use]
    pub fn build<H, E>(self, handler: H) -> WebhookReceiver<H, E>
    where
        H: WebhookHandler<E>,
    {
        WebhookReceiver {
            verifier: self.verifier,
            body_limit: self.body_limit,
            handle_ping: self.handle_ping,
            handler,
            error: PhantomData,
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
/// delivery record GitHub stores.
pub struct WebhookReceiver<H, E> {
    verifier: Verifier,
    body_limit: usize,
    handle_ping: bool,
    handler: H,
    error: PhantomData<fn() -> E>,
}

// Hand-written rather than derived: a derive would bound both impls on `E`,
// which appears only in `PhantomData` and is routinely not `Clone`.
impl<H, E> fmt::Debug for WebhookReceiver<H, E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The handler is elided rather than bounded: closures are never
        // `Debug`, and the configuration is what is worth printing.
        formatter
            .debug_struct("WebhookReceiver")
            .field("verifier", &self.verifier)
            .field("body_limit", &self.body_limit)
            .field("handle_ping", &self.handle_ping)
            .finish_non_exhaustive()
    }
}

impl<H: Clone, E> Clone for WebhookReceiver<H, E> {
    fn clone(&self) -> Self {
        Self {
            verifier: self.verifier.clone(),
            body_limit: self.body_limit,
            handle_ping: self.handle_ping,
            handler: self.handler.clone(),
            error: PhantomData,
        }
    }
}

impl<H, E> WebhookReceiver<H, E> {
    /// Authenticates, bounds, and dispatches one request.
    ///
    /// Handler futures do not need to be `Send`: this path never boxes and
    /// never crosses a Tower or native executor boundary. That is what lets a
    /// Cloudflare Worker hand an `http::Request<worker::Body>` straight in.
    ///
    /// The receiver is cloned per delivery (an `Arc` bump for the verifier
    /// plus the handler's own `Clone`) so the future owns its state and is
    /// `Send` whenever the handler is, without demanding `H: Sync`.
    pub async fn receive<B>(&self, request: Request<B>) -> ServiceResponse
    where
        H: WebhookHandler<E> + Clone,
        B: Body<Data = Bytes> + Unpin,
    {
        empty_response(self.clone().process(request).await)
    }

    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(
            name = "octoevents.receive",
            skip_all,
            fields(delivery_id = tracing::field::Empty, event = tracing::field::Empty, outcome = tracing::field::Empty)
        )
    )]
    // By-value `self` on purpose: holding `&self` across an await would put
    // `&WebhookReceiver` in the future and demand `H: Sync` for it to be
    // `Send`. Owning a clone keeps the bound at `H: Send + Clone`.
    async fn process<B>(self, request: Request<B>) -> ResponseStatus
    where
        H: WebhookHandler<E>,
        B: Body<Data = Bytes> + Unpin,
    {
        let (parts, mut body) = request.into_parts();
        let headers = HeaderView::from(&parts.headers);
        record_headers(&headers);

        // A request whose signature header cannot authenticate is refused on
        // the headers alone, so unsigned traffic never occupies `body_limit`
        // bytes of memory. `Envelope::from_signed` repeats the check for
        // transports that construct envelopes directly.
        if let Some(error) = headers.signature_failure() {
            return record_outcome(ResponseStatus::for_receive_error(&error.into()));
        }

        // The comparison is in `u64` so a hint above `usize::MAX` (possible
        // on 32-bit targets, wasm included) still takes the fast path.
        if u64::try_from(self.body_limit).is_ok_and(|limit| body.size_hint().lower() > limit) {
            return record_outcome(body_too_large(self.body_limit));
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
                .is_none_or(|length| length > self.body_limit)
            {
                return record_outcome(body_too_large(self.body_limit));
            }
            bytes.extend_from_slice(&data);
        }

        let envelope = match Envelope::from_signed(&self.verifier, &headers, bytes.freeze()) {
            Ok(envelope) => envelope,
            Err(error) => return record_outcome(ResponseStatus::for_receive_error(&error)),
        };

        if !self.handle_ping && matches!(envelope.kind, EventKind::Ping) {
            return record_outcome(ResponseStatus::NoContent);
        }

        match self.handler.handle(envelope).await {
            Ok(()) => record_outcome(ResponseStatus::NoContent),
            Err(_) => record_outcome(ResponseStatus::InternalServerError),
        }
    }
}

/// Lets a Tower router own the path and method while the receiver owns the
/// policy.
///
/// ```
/// use axum::{Router, routing::post_service};
/// use octoevents::{Envelope, Secret, Verifier, WebhookReceiverBuilder};
///
/// let verifier = Verifier::new(Secret::new("current secret"))
///     .also(Secret::new("previous secret"));
///
/// let webhook = WebhookReceiverBuilder::new(verifier)
///     .body_limit(1024 * 1024)
///     .build(|envelope: Envelope| async move {
///         // Persist or forward before returning; process asynchronously.
///         println!("{} {}", envelope.delivery_id, envelope.kind);
///         Ok::<_, std::convert::Infallible>(())
///     });
///
/// let app: Router = Router::new().route("/webhook", post_service(webhook));
/// # let _ = app;
/// ```
#[cfg(feature = "tower")]
#[cfg_attr(docsrs, doc(cfg(feature = "tower")))]
impl<H, B, E> Service<Request<B>> for WebhookReceiver<H, E>
where
    H: WebhookHandler<E> + Clone + Send + 'static,
    H::Future: Send + 'static,
    B: Body<Data = Bytes> + Send + Unpin + 'static,
    E: 'static,
{
    type Response = ServiceResponse;
    type Error = Infallible;
    type Future = ServiceFuture;

    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<B>) -> Self::Future {
        // An owned clone rather than a borrow of `self`: the boxed future must
        // be `Send`, and holding `&self` across an await would demand
        // `H: Sync`, which this crate deliberately does not require.
        let receiver = self.clone();

        Box::pin(async move { Ok(empty_response(receiver.process(request).await)) })
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

#[cfg(feature = "tracing")]
fn record_headers(headers: &HeaderView<'_>) {
    let span = tracing::Span::current();
    if let Some(delivery_id) = headers.recorded_delivery_id() {
        span.record("delivery_id", delivery_id);
    }
    if let Some(event) = headers.recorded_event_name() {
        span.record("event", event);
    }
}

#[cfg(not(feature = "tracing"))]
fn record_headers(_headers: &HeaderView<'_>) {}

fn record_outcome(status: ResponseStatus) -> ResponseStatus {
    #[cfg(feature = "tracing")]
    tracing::Span::current().record("outcome", status.as_u16());
    status
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use bytes::Bytes;
    use hmac::{Hmac, Mac};
    use http::{Request, StatusCode};
    use http_body::Body as _;
    use http_body_util::Full;
    use sha2::Sha256;
    #[cfg(feature = "tower")]
    use tower::ServiceExt as _;

    use super::{WebhookReceiverBuilder, empty_response};
    use crate::{Dispatcher, EventKind, ResponseStatus, Secret, Verifier};

    fn request(body: &'static [u8], event: &str) -> Request<Full<Bytes>> {
        Request::builder()
            .header("content-type", "application/json")
            .header("x-github-delivery", "delivery")
            .header("x-github-event", event)
            .header("x-hub-signature-256", signature(b"secret", body))
            .body(Full::new(Bytes::from_static(body)))
            .unwrap()
    }

    fn signature(secret: &[u8], body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
        mac.update(body);
        format!("sha256={:x}", mac.finalize().into_bytes())
    }

    #[tokio::test]
    async fn returns_no_content_after_successful_dispatch() {
        let service = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
            .build(|_| async { Ok::<_, ()>(()) });

        let response = service.receive(request(b"{}", "push")).await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn accepts_a_dispatcher_as_its_handler() {
        let dispatcher = Dispatcher::<()>::builder()
            .on(EventKind::Push, |_| async { Ok(()) })
            .build();
        let service =
            WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(dispatcher);

        let response = service.receive(request(b"{}", "push")).await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[cfg(feature = "tower")]
    #[tokio::test]
    async fn the_tower_service_impl_applies_the_same_policy() {
        let service = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
            .build(|_| async { Ok::<_, ()>(()) });

        let response = service.oneshot(request(b"{}", "push")).await.unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn receive_accepts_a_non_send_handler() {
        use std::{cell::Cell, rc::Rc};

        // The Workers path relies on this: `receive` boxes nothing, so it never
        // imposes the `Send` bound the `Service` impl must.
        let calls = Rc::new(Cell::new(0));
        let handler_calls = Rc::clone(&calls);
        let service =
            WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(move |_| {
                let calls = Rc::clone(&handler_calls);
                async move {
                    calls.set(calls.get() + 1);
                    Ok::<_, ()>(())
                }
            });

        let response = service.receive(request(b"{}", "push")).await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(calls.get(), 1);
    }

    #[tokio::test]
    async fn short_circuits_ping_unless_enabled() {
        let calls = Arc::new(AtomicUsize::new(0));
        let handler_calls = Arc::clone(&calls);
        let service =
            WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(move |_| {
                handler_calls.fetch_add(1, Ordering::Relaxed);
                async { Ok::<_, ()>(()) }
            });

        let response = service.receive(request(b"{}", "ping")).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(calls.load(Ordering::Relaxed), 0);

        let handler_calls = Arc::clone(&calls);
        let service = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
            .handle_ping(true)
            .build(move |_| {
                handler_calls.fetch_add(1, Ordering::Relaxed);
                async { Ok::<_, ()>(()) }
            });
        service.receive(request(b"{}", "ping")).await;
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn maps_authentication_and_request_errors() {
        let service = || {
            WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
                .build(|_| async { Ok::<_, ()>(()) })
        };

        let mut mismatch = request(b"{}", "push");
        mismatch.headers_mut().insert(
            "x-hub-signature-256",
            "sha256=0000000000000000000000000000000000000000000000000000000000000000"
                .parse()
                .unwrap(),
        );
        assert_eq!(
            service().receive(mismatch).await.status(),
            StatusCode::UNAUTHORIZED
        );

        let mut sha1_only = request(b"{}", "push");
        sha1_only.headers_mut().remove("x-hub-signature-256");
        sha1_only
            .headers_mut()
            .insert("x-hub-signature", "sha1=legacy".parse().unwrap());
        assert_eq!(
            service().receive(sha1_only).await.status(),
            StatusCode::UNAUTHORIZED
        );

        let mut malformed = request(b"{}", "push");
        malformed
            .headers_mut()
            .insert("x-hub-signature-256", "invalid".parse().unwrap());
        assert_eq!(
            service().receive(malformed).await.status(),
            StatusCode::BAD_REQUEST
        );

        let mut form = request(b"{}", "push");
        form.headers_mut().insert(
            "content-type",
            "application/x-www-form-urlencoded".parse().unwrap(),
        );
        assert_eq!(
            service().receive(form).await.status(),
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

        let service = || {
            WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
                .build(|_| async { Ok::<_, ()>(()) })
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
            service().receive(request(None)).await.status(),
            StatusCode::UNAUTHORIZED
        );
        // A signed request reaches the read loop and reports the body failure.
        let signed = signature(b"secret", b"{}");
        assert_eq!(
            service().receive(request(Some(&signed))).await.status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn stops_at_the_body_limit_before_authentication() {
        let service = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
            .body_limit(1)
            .build(|_| async { Ok::<_, ()>(()) });

        let response = service.receive(request(b"{}", "push")).await;

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn handler_errors_return_bare_internal_server_errors() {
        let service = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
            .build(|_| async { Err::<(), _>("private error") });

        let response = service.receive(request(b"{}", "push")).await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(response.body().size_hint().exact(), Some(0));
    }

    #[test]
    fn debug_and_clone_do_not_constrain_the_error_type() {
        // `E` reaches neither impl: it lives only in `PhantomData`, and error
        // types are routinely not `Clone`.
        struct NotCloneOrDebug;

        let service = WebhookReceiverBuilder::new(Verifier::new(Secret::new("super-secret")))
            .body_limit(64)
            .build(|_| async { Err::<(), _>(NotCloneOrDebug) });

        let debug = format!("{:?}", service.clone());
        assert!(debug.contains("body_limit: 64"), "{debug}");
        assert!(debug.contains("[REDACTED]"), "{debug}");
        assert!(!debug.contains("super-secret"), "{debug}");
    }

    #[test]
    fn response_contract_uses_empty_bodies() {
        let response = empty_response(ResponseStatus::BadRequest);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.body().size_hint().exact(), Some(0));
    }
}
