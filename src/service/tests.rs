//! The receiver's tests, beside the production code so they see the
//! module's private items and share the crate's fixtures. Grouped by
//! concern: receiving a request, the error observer, the Tower service
//! impl, the `Debug` output, and the response contract.
//!
//! Every request here is signed by [`verifier`], the verifier each receiver
//! is built with, so a test that wants a refusal says which header it
//! tampers with.

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
use http::Request;
use http_body::Frame;
use http_body_util::Full;

use crate::{DecodeError, Envelope, EventKind, Handler, Payload, Verifier, WebhookSecret};

/// A production-shaped handler: dependencies as fields, borrowed through
/// `&self`, and deliberately not `Clone`.
struct Recorder {
    calls: Arc<AtomicUsize>,
}

impl Handler<Envelope> for Recorder {
    type Error = std::convert::Infallible;

    // A real handler awaits its dependencies; this one only counts.
    #[expect(clippy::unused_async_trait_impl)]
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

impl Payload for IssueView {
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

    #[expect(clippy::unused_async_trait_impl)]
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

/// A body that cannot be moved once pinned: `PhantomPinned` makes it
/// `!Unpin`, and the frames sit behind a `Mutex` so `poll_frame` reads
/// them through the pin without projecting. A request over it compiles
/// only against a receiver that asks no `Unpin` of the body.
struct Pinned {
    frames: std::sync::Mutex<VecDeque<Frame<Bytes>>>,
    _pinned: std::marker::PhantomPinned,
}

impl Pinned {
    /// One data frame holding `payload`.
    fn data(payload: &'static [u8]) -> Self {
        Self {
            frames: std::sync::Mutex::new(VecDeque::from([Frame::data(Bytes::from_static(
                payload,
            ))])),
            _pinned: std::marker::PhantomPinned,
        }
    }
}

impl http_body::Body for Pinned {
    type Data = Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        Poll::Ready(self.frames.lock().unwrap().pop_front().map(Ok))
    }
}

/// A body whose transport fails on the first poll with the given text,
/// as a dropped connection does: the receiver sees no data frame, only
/// the error. The error type is a bare `&str`, `Display` and nothing
/// more, which is all the receiver asks of a body's error.
struct FailingBody(&'static str);

impl http_body::Body for FailingBody {
    type Data = Bytes;
    type Error = &'static str;

    fn poll_frame(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        Poll::Ready(Some(Err(self.0)))
    }
}

/// The request the receivers here accept: `body` under `event`, signed by
/// [`verifier`], with the headers a well-formed delivery carries.
fn request(body: &'static [u8], event: &str) -> Request<Full<Bytes>> {
    request_over(
        Full::new(Bytes::from_static(body)),
        event,
        &verifier().sign(body).to_string(),
    )
}

/// A request over a body the test shapes itself, carrying `signature` as
/// its signature header: `verifier().sign(..)` over the bytes the body
/// yields, rendered, authenticates, anything else does not.
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
    Verifier::new(WebhookSecret::new("secret"))
}

/// Receiving a request: the handler forms `build` accepts, what the handler
/// is handed, the ping short circuit, the body limit and where it stands
/// against authentication, and the bare status a failure answers with.
mod receive {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use bytes::Bytes;
    use http::{HeaderMap, Request, StatusCode};
    use http_body::{Body as _, Frame};
    use http_body_util::Full;

    use super::{
        FailingBody, Frames, IssueRecorder, Pinned, Recorder, WRONG_SIGNATURE, request,
        request_over, verifier, with_header, without_header,
    };
    use crate::{
        BodyError, Dispatcher, Envelope, EventKind, ReceiveError, WebhookReceiverBuilder,
        service::read_body, test_support::AppError,
    };

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
            .body(FailingBody("body must not be read"))
            .unwrap()
        };

        assert_eq!(
            receiver().receive(request(None)).await.status(),
            StatusCode::UNAUTHORIZED
        );
        // A signed request reaches the read loop and reports the body
        // failure with the status `ReceiveError::BodyRead` maps to. That the
        // loop produces that variant is `read_body`'s own test below; that
        // the receiver records its fixed text on the receive span as `error`
        // while omitting `source` is covered by `tests/tracing_outcome.rs`,
        // where another 400 for a malformed header reads differently.
        let signed = verifier().sign(b"{}").to_string();
        assert_eq!(
            receiver().receive(request(Some(&signed))).await.status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn refuses_a_malformed_signature_header_without_reading_the_body() {
        // A malformed header is 400, as a body the transport cannot read is,
        // so the status alone cannot say whether the body was reached; the
        // body counts its polls instead, and none means the headers were
        // decisive alone. The header is parsed before the body is read, so
        // a value that is not `sha256=` and 64 hex digits never costs the
        // receiver `body_limit` bytes of memory.
        let receiver =
            WebhookReceiverBuilder::new(verifier()).build(|_: Envelope| async { Ok::<_, ()>(()) });
        let body = Frames::data(&[b"{}"]);
        let polls = body.polls();

        let response = receiver
            .receive(request_over(body, "push", "sha256=not-hex"))
            .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(polls.load(Ordering::Relaxed), 0, "the body was polled");
    }

    #[tokio::test]
    async fn a_frame_the_transport_cannot_produce_is_a_body_read_error() {
        // The frame is an error, not data: the read stops there and the
        // failure has a value, the variant every pre-handler failure has,
        // carrying the transport's text as its source rather than a bare
        // status. Under a limit the empty body is within, so only the frame
        // error can refuse it.
        let result = read_body(FailingBody("connection reset by peer"), 64).await;

        assert_eq!(
            result,
            Err(ReceiveError::BodyRead(BodyError::new(
                "connection reset by peer"
            )))
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
        let signed = verifier().sign(PAYLOAD).to_string();
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
    async fn receives_a_body_that_is_not_unpin() {
        // A transport's body type is whatever it is; the receiver pins it
        // where it polls it and asks nothing of the caller. The test compiles
        // only while `receive` places no `Unpin` bound on `B`.
        let receiver =
            WebhookReceiverBuilder::new(verifier()).build(|_: Envelope| async { Ok::<_, ()>(()) });

        let response = receiver
            .receive(request_over(
                Pinned::data(b"{}"),
                "push",
                &verifier().sign(b"{}").to_string(),
            ))
            .await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
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
            .receive(request_over(
                body,
                "push",
                &verifier().sign(b"").to_string(),
            ))
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
}

/// The error observer: what it receives, the error types it accepts, and
/// that it runs only for a handler's failure.
mod observer {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use http::StatusCode;
    use http_body::Body as _;

    use super::{WRONG_SIGNATURE, request, verifier, with_header, without_header};
    use crate::{
        Action, DispatchError, Dispatcher, Envelope, EventKind, EventMeta, WebhookReceiverBuilder,
        test_support::AppError,
    };

    #[tokio::test]
    async fn the_error_observer_receives_the_event_meta_and_a_dispatchers_error_before_the_500() {
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
}

/// The Tower `Service` impl: the same policy as `receive`, for the same
/// handler forms.
#[cfg(feature = "tower")]
mod tower {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use ::tower::ServiceExt as _;
    use http::StatusCode;

    use super::{Pinned, Recorder, request, request_over, verifier};
    use crate::{Envelope, WebhookReceiverBuilder};

    #[tokio::test]
    async fn the_tower_service_impl_applies_the_same_policy() {
        let receiver =
            WebhookReceiverBuilder::new(verifier()).build(|_: Envelope| async { Ok::<_, ()>(()) });

        let response = receiver.oneshot(request(b"{}", "push")).await.unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

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
    async fn the_tower_service_impl_accepts_a_body_that_is_not_unpin() {
        // The `Service` impl states its own bound on `B`, apart from
        // `receive`'s, so it is held to the same test: this compiles only
        // while that bound asks no `Unpin` either.
        let receiver =
            WebhookReceiverBuilder::new(verifier()).build(|_: Envelope| async { Ok::<_, ()>(()) });

        let response = receiver
            .oneshot(request_over(
                Pinned::data(b"{}"),
                "push",
                &verifier().sign(b"{}").to_string(),
            ))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
}

/// The `Debug` output of the builder and the receiver: what it says, what
/// it redacts, and that it constrains neither the handler nor its error.
mod debug {
    use std::sync::{Arc, atomic::AtomicUsize};

    use super::Recorder;
    #[cfg(feature = "tracing")]
    use super::verifier;
    use crate::{Envelope, EventMeta, Verifier, WebhookReceiverBuilder, WebhookSecret};

    #[test]
    fn debug_and_clone_do_not_constrain_the_handler_or_its_error() {
        // Neither the handler nor its error type reaches either impl: error
        // types are routinely not `Clone`, and the handler is not required
        // to be, either. The builder holds an observer over the error type
        // and is under the same rule.
        struct NotCloneOrDebug;

        let builder =
            WebhookReceiverBuilder::new(Verifier::new(WebhookSecret::new("super-secret")))
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

        let receiver = WebhookReceiverBuilder::new(Verifier::new(WebhookSecret::new(
            "super-secret",
        )))
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
}

/// The response contract: an empty body, the HTTP status each
/// `ResponseStatus` converts to, and the outcome label each is recorded
/// under on the receive span.
mod respond {
    use http::StatusCode;
    use http_body::Body as _;

    use crate::{
        ResponseStatus,
        service::{empty_response, outcome_label},
    };

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
