#[cfg(feature = "tower")]
use std::{
    convert::Infallible,
    task::{Context, Poll},
};
use std::{fmt, sync::Arc};

use bytes::{Bytes, BytesMut};
use http::{Request, Response};
use http_body::Body;
use http_body_util::{BodyExt as _, Empty};
#[cfg(feature = "tower")]
use tower_service::Service;

#[cfg(feature = "tower")]
use crate::runtime::BoxFuture;
use crate::{
    DEFAULT_BODY_LIMIT, Envelope, EventKind, HeaderView, MaybeSend, MaybeSync, ReceiveError,
    ResponseStatus, Verifier, WebhookHandler, trace,
};

type ServiceResponse = Response<Empty<Bytes>>;

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

    /// Builds a receiver around one caller-owned handler.
    ///
    /// The handler is any [`WebhookHandler`]: a struct with dependencies, a
    /// closure, a `Dispatcher`, or a typed handler converted with its
    /// `into_webhook_handler()`. It does not need to be `Clone`.
    #[must_use]
    pub fn build<H>(self, handler: H) -> WebhookReceiver<H>
    where
        H: WebhookHandler + MaybeSend + MaybeSync + 'static,
    {
        WebhookReceiver {
            inner: Arc::new(Inner {
                verifier: self.verifier,
                body_limit: self.body_limit,
                handle_ping: self.handle_ping,
                handler,
            }),
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
pub struct WebhookReceiver<H> {
    // Shared rather than owned so the receiver is `Clone` for any handler:
    // Tower routers clone a service per connection and its future must own
    // its state, and a struct handler should not need `Clone` for that.
    inner: Arc<Inner<H>>,
}

struct Inner<H> {
    verifier: Verifier,
    body_limit: usize,
    handle_ping: bool,
    handler: H,
}

impl<H> fmt::Debug for WebhookReceiver<H> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The handler is elided rather than bounded: closures are never
        // `Debug`, and the configuration is what is worth printing.
        formatter
            .debug_struct("WebhookReceiver")
            .field("verifier", &self.inner.verifier)
            .field("body_limit", &self.inner.body_limit)
            .field("handle_ping", &self.inner.handle_ping)
            .finish_non_exhaustive()
    }
}

impl<H> Clone for WebhookReceiver<H> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<H> WebhookReceiver<H>
where
    H: WebhookHandler + MaybeSend + MaybeSync + 'static,
{
    /// Authenticates, bounds, and dispatches one request.
    ///
    /// This path never boxes and never crosses a Tower or native executor
    /// boundary, so on `wasm32` a Cloudflare Worker can hand an
    /// `http::Request<worker::Body>` straight in with a handler holding
    /// JavaScript values.
    pub async fn receive<B>(&self, request: Request<B>) -> ServiceResponse
    where
        B: Body<Data = Bytes> + Unpin,
    {
        empty_response(self.inner.process(request).await)
    }
}

impl<H> Inner<H>
where
    H: WebhookHandler,
{
    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(
            name = "octoevents.receive",
            skip_all,
            fields(delivery_id = tracing::field::Empty, event = tracing::field::Empty, outcome = tracing::field::Empty)
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

        if !self.handle_ping && matches!(envelope.meta.kind, EventKind::Ping) {
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
/// use octoevents::{Envelope, Secret, Verifier, WebhookHandler, WebhookReceiverBuilder};
///
/// struct Persist { /* database pool */ }
///
/// impl WebhookHandler for Persist {
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
    H: WebhookHandler + MaybeSend + MaybeSync + 'static,
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
    trace::record("outcome", status.as_u16());
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
        Envelope, EventKind, EventMeta, PayloadHandler, ResponseStatus, Secret, Verifier,
        WebhookHandler,
    };

    /// A production-shaped handler: dependencies as fields, borrowed through
    /// `&self`, and deliberately not `Clone`.
    struct Recorder {
        calls: Arc<AtomicUsize>,
    }

    impl WebhookHandler for Recorder {
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

    struct IssueRecorder {
        seen: Arc<std::sync::Mutex<Vec<(String, String, u64)>>>,
    }

    impl PayloadHandler<IssueView> for IssueRecorder {
        type Error = std::convert::Infallible;

        #[allow(clippy::unused_async_trait_impl)]
        async fn handle(&self, meta: EventMeta, payload: IssueView) -> Result<(), Self::Error> {
            self.seen.lock().unwrap().push((
                meta.delivery_id,
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
        let service = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
            .build(|_: Envelope| async { Ok::<_, ()>(()) });

        let response = service.receive(request(b"{}", "push")).await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn accepts_a_struct_handler_that_is_not_clone() {
        let calls = Arc::new(AtomicUsize::new(0));
        let service =
            WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(Recorder {
                calls: Arc::clone(&calls),
            });

        let response = service.receive(request(b"{}", "push")).await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[cfg(feature = "tower")]
    #[tokio::test]
    async fn the_tower_service_impl_applies_the_same_policy() {
        let service = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
            .build(|_: Envelope| async { Ok::<_, ()>(()) });

        let response = service.oneshot(request(b"{}", "push")).await.unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[cfg(feature = "tower")]
    #[tokio::test]
    async fn the_tower_service_impl_accepts_a_struct_handler() {
        let calls = Arc::new(AtomicUsize::new(0));
        let service =
            WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(Recorder {
                calls: Arc::clone(&calls),
            });

        let response = service.oneshot(request(b"{}", "push")).await.unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn a_payload_handler_receives_its_decoded_view_with_the_metadata() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let service = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(
            IssueRecorder {
                seen: Arc::clone(&seen),
            }
            .into_webhook_handler(),
        );

        let response = service
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
    async fn a_payload_handler_rejects_an_envelope_of_the_wrong_kind() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let service = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(
            IssueRecorder {
                seen: Arc::clone(&seen),
            }
            .into_webhook_handler(),
        );

        // The body would decode as an IssueView; only the kind is wrong.
        let response = service
            .receive(request(
                br#"{"action":"opened","issue":{"number":7}}"#,
                "pull_request",
            ))
            .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_payload_handler_fails_the_delivery_when_the_payload_does_not_decode() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let service = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(
            IssueRecorder {
                seen: Arc::clone(&seen),
            }
            .into_webhook_handler(),
        );

        let response = service
            .receive(request(br#"{"action":"opened"}"#, "issues"))
            .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[cfg(feature = "octocrab")]
    #[tokio::test]
    async fn accepts_a_dispatcher_as_its_handler() {
        use crate::{DecodeError, Dispatcher};

        #[derive(Debug)]
        struct AppError;
        impl From<DecodeError> for AppError {
            fn from(_: DecodeError) -> Self {
                Self
            }
        }
        impl From<std::convert::Infallible> for AppError {
            fn from(never: std::convert::Infallible) -> Self {
                match never {}
            }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let handler_calls = Arc::clone(&calls);
        let dispatcher = Dispatcher::<AppError>::builder()
            .on(
                EventKind::Ping,
                move |_: EventMeta, _: octocrab::models::webhook_events::WebhookEvent| {
                    handler_calls.fetch_add(1, Ordering::Relaxed);
                    async { Ok::<_, std::convert::Infallible>(()) }
                },
            )
            .build();
        let service = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
            .handle_ping(true)
            .build(dispatcher);

        let response = service
            .receive(request(
                include_bytes!("../tests/fixtures/ping.json"),
                "ping",
            ))
            .await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[cfg(feature = "octocrab")]
    #[tokio::test]
    async fn an_event_handler_receives_octocrabs_decoded_event_with_the_metadata() {
        use octocrab::models::webhook_events::{WebhookEvent, WebhookEventPayload};

        use crate::EventHandler;

        type Seen = Arc<std::sync::Mutex<Vec<(String, Option<u64>, u64)>>>;

        struct EventRecorder {
            seen: Seen,
        }

        impl EventHandler for EventRecorder {
            type Error = std::convert::Infallible;

            #[allow(clippy::unused_async_trait_impl)]
            async fn handle(
                &self,
                meta: EventMeta,
                event: WebhookEvent,
            ) -> Result<(), Self::Error> {
                let WebhookEventPayload::PullRequest(payload) = event.specific else {
                    panic!("expected a pull request payload");
                };
                self.seen.lock().unwrap().push((
                    meta.delivery_id,
                    meta.installation_id,
                    payload.number,
                ));
                Ok(())
            }
        }

        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let service = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(
            EventRecorder {
                seen: Arc::clone(&seen),
            }
            .into_webhook_handler(),
        );

        let response = service
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
    async fn a_payload_handler_receives_octocrabs_payload_for_its_kind() {
        use octocrab::models::webhook_events::payload::{
            PullRequestWebhookEventAction, PullRequestWebhookEventPayload,
        };

        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let handler_seen = Arc::clone(&seen);
        let service = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(
            (move |meta: EventMeta, payload: PullRequestWebhookEventPayload| {
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
            })
            .into_webhook_handler(),
        );

        let response = service
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
        let service = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret"))).build(
            move |_: Envelope| {
                handler_calls.fetch_add(1, Ordering::Relaxed);
                async { Ok::<_, ()>(()) }
            },
        );

        let response = service.receive(request(b"{}", "ping")).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(calls.load(Ordering::Relaxed), 0);

        let handler_calls = Arc::clone(&calls);
        let service = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
            .handle_ping(true)
            .build(move |_: Envelope| {
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
                .build(|_: Envelope| async { Ok::<_, ()>(()) })
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
            .build(|_: Envelope| async { Ok::<_, ()>(()) });

        let response = service.receive(request(b"{}", "push")).await;

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn handler_errors_return_bare_internal_server_errors() {
        let service = WebhookReceiverBuilder::new(Verifier::new(Secret::new("secret")))
            .build(|_: Envelope| async { Err::<(), _>("private error") });

        let response = service.receive(request(b"{}", "push")).await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(response.body().size_hint().exact(), Some(0));
    }

    #[test]
    fn debug_and_clone_do_not_constrain_the_handler_or_its_error() {
        // Neither the handler nor its error type reaches either impl: error
        // types are routinely not `Clone`, and the handler is not required
        // to be, either.
        struct NotCloneOrDebug;

        let service = WebhookReceiverBuilder::new(Verifier::new(Secret::new("super-secret")))
            .body_limit(64)
            .build(|_: Envelope| async { Err::<(), _>(NotCloneOrDebug) });

        let debug = format!("{:?}", service.clone());
        assert!(debug.contains("body_limit: 64"), "{debug}");
        assert!(debug.contains("[REDACTED]"), "{debug}");
        assert!(!debug.contains("super-secret"), "{debug}");

        let service = WebhookReceiverBuilder::new(Verifier::new(Secret::new("super-secret")))
            .build(Recorder {
                calls: Arc::new(AtomicUsize::new(0)),
            });
        let _ = format!("{:?}", service.clone());
    }

    #[test]
    fn response_contract_uses_empty_bodies() {
        let response = empty_response(ResponseStatus::BadRequest);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.body().size_hint().exact(), Some(0));
    }
}
