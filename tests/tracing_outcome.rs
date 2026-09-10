//! Every span the crate opens closes with an `outcome` label, a contract
//! dashboards filter on, with one vocabulary per span:
//!
//! - `octoevents.dispatch` records what happened to one delivery: `ok` and
//!   `handler_error` for a matched delivery, `unmatched_ok` and
//!   `unmatched_error` for an unmatched one, whichever tier failed it, derived
//!   from the [`Outcome`] the dispatcher returns; beside it the tier, handler
//!   name and registration site of the handler that failed it, and the
//!   `EventMeta` fields it ran with (`delivery_id`, `event`, `action`,
//!   `installation_id`). The same span wraps `Handler::handle`, so the
//!   receiver's path records the value too.
//! - `octoevents.receive` records how the receiver answered: `ok`,
//!   `bad_request`, `unauthorized`, `payload_too_large` or `handler_error`,
//!   beside `status`, the HTTP code, and for a request refused before any
//!   handler ran, the text of the `ReceiveError` that selected the status as
//!   `error`.
//! - `octoevents.verify` records how verification ended: `verified` or
//!   `mismatch`, beside `secret_count` and `body_len`. A malformed header
//!   never reaches it: the header is parsed into a `Signature` before the
//!   verifier is asked, so the span is about the HMAC and nothing else.
//!
//! The fields the receive and dispatch spans share are recorded in the same
//! form on both.
//!
//! The tests read what the crate recorded, through the recording layer in
//! `common`, not how a subscriber renders it: a string field is a
//! [`Value::Str`], an integer a [`Value::U64`], whatever they would look like
//! on a line.

#![cfg(all(feature = "tracing", not(target_arch = "wasm32")))]

mod common;

use common::{Fields, SpanRecord, Value};
use octoevents::{
    Action, AnyAction, DispatchError, Dispatcher, Envelope, EventKind, Handler as _, Match,
    Outcome, Signature, SignatureError, Verifier, WebhookSecret,
};

/// The boxed source's text: the handlers here fail with a `&'static str`,
/// which the box displays verbatim.
fn source_text(error: DispatchError) -> String {
    error.into_source().to_string()
}

/// The match and the handlers' result with the dispatch error unwrapped to
/// its source's text: these tests check the span's label against what was
/// returned, not where the failing handler was registered.
fn unwrapped_outcome(outcome: Outcome) -> (Match, Result<(), String>) {
    (outcome.matched, outcome.result.map_err(source_text))
}

/// A view any `pull_request` payload satisfies. Written by hand rather than
/// derived so the file compiles without the `derive` feature.
#[derive(serde::Deserialize)]
struct AnyPullRequest {}
impl octoevents::Payload for AnyPullRequest {
    const KIND: EventKind = EventKind::PullRequest;
}

/// An envelope of `kind` whose payload carries `action`, or `{}` for none, so
/// the meta the span records is what the payload says.
fn envelope(kind: EventKind, action: Option<Action>) -> Envelope {
    Envelope::new("delivery", kind, payload(action, None))
}

/// The smallest payload carrying `action` and an installation ID, each when
/// given.
fn payload(action: Option<Action>, installation_id: Option<u64>) -> Vec<u8> {
    let mut document = serde_json::Map::new();
    if let Some(action) = action {
        document.insert("action".into(), action.as_str().into());
    }
    if let Some(id) = installation_id {
        document.insert("installation".into(), serde_json::json!({ "id": id }));
    }
    serde_json::to_vec(&document).unwrap()
}

/// Runs `dispatch` under a fresh recording subscriber and returns the fields
/// the `octoevents.dispatch` span closed with alongside what the call
/// returned.
#[track_caller]
fn traced<F, T>(dispatch: F) -> (Fields, T)
where
    F: Future<Output = T>,
{
    let (recording, returned) = common::traced(dispatch);
    (
        recording.span("octoevents.dispatch").at_close.clone(),
        returned,
    )
}

fn dispatcher() -> Dispatcher {
    Dispatcher::builder()
        .on([Action::Opened], |_: AnyPullRequest| async {
            Ok::<_, &'static str>(())
        })
        .on([Action::Closed], |_: AnyPullRequest| async {
            Err::<(), _>("routed")
        })
        .fallback(|envelope: Envelope| async move {
            if envelope.meta.kind == EventKind::Installation {
                Err("unmatched")
            } else {
                Ok(())
            }
        })
        .build()
}

#[test]
fn the_span_records_one_of_four_outcomes_derived_from_the_returned_outcome() {
    let dispatcher = dispatcher();

    let (fields, outcome) =
        traced(dispatcher.dispatch(envelope(EventKind::PullRequest, Some(Action::Opened))));
    assert_eq!(fields.str("outcome"), Some("ok"));
    assert_eq!(unwrapped_outcome(outcome), (Match::Matched, Ok(())));

    let (fields, outcome) =
        traced(dispatcher.dispatch(envelope(EventKind::PullRequest, Some(Action::Closed))));
    assert_eq!(fields.str("outcome"), Some("handler_error"));
    assert_eq!(
        unwrapped_outcome(outcome),
        (Match::Matched, Err("routed".to_owned()))
    );

    // Unmatched with the kind known and unknown both read as unmatched: the
    // label says whether the delivery was matched, not how the miss came about.
    let (fields, outcome) =
        traced(dispatcher.dispatch(envelope(EventKind::PullRequest, Some(Action::Reopened))));
    assert_eq!(fields.str("outcome"), Some("unmatched_ok"));
    assert_eq!(outcome.matched, Match::UnmatchedAction);

    let (fields, outcome) =
        traced(dispatcher.dispatch(envelope(EventKind::CheckRun, Some(Action::Completed))));
    assert_eq!(fields.str("outcome"), Some("unmatched_ok"));
    assert_eq!(outcome.matched, Match::UnmatchedKind);

    let (fields, outcome) =
        traced(dispatcher.dispatch(envelope(EventKind::Installation, Some(Action::Created))));
    assert_eq!(fields.str("outcome"), Some("unmatched_error"));
    assert_eq!(
        unwrapped_outcome(outcome),
        (Match::UnmatchedKind, Err("unmatched".to_owned()))
    );
}

/// An `always` handler that fails every delivery.
async fn fail_audit(_: Envelope) -> Result<(), &'static str> {
    Err("audit")
}

#[test]
fn a_failure_before_routing_is_labelled_by_the_match_the_route_table_decided() {
    // No fallback is registered: the label must not claim one ran. The
    // location is that of the registration method's name, so the failing
    // handler is registered on the line after `line!()`.
    let builder = Dispatcher::builder().on(AnyAction, |_: AnyPullRequest| async {
        Ok::<_, &'static str>(())
    });
    let registration_line = line!() + 1;
    let dispatcher = builder.always(fail_audit).build();

    // The always tier fails both deliveries before any route or fallback
    // runs. The label follows the match, not the tier that failed: the
    // unmatched one reads as `unmatched_error`, which is true whichever tier
    // failed it; the tier itself is a field of its own.
    let (fields, outcome) =
        traced(dispatcher.dispatch(envelope(EventKind::PullRequest, Some(Action::Opened))));
    assert_eq!(fields.str("outcome"), Some("handler_error"));
    assert_eq!(fields.str("tier"), Some("always"));
    assert_eq!(
        unwrapped_outcome(outcome),
        (Match::Matched, Err("audit".to_owned()))
    );

    let (fields, outcome) =
        traced(dispatcher.dispatch(envelope(EventKind::CheckRun, Some(Action::Completed))));
    assert_eq!(fields.str("outcome"), Some("unmatched_error"));
    assert_eq!(fields.str("tier"), Some("always"));
    assert_registered_on(&fields, registration_line);
    assert_eq!(
        unwrapped_outcome(outcome),
        (Match::UnmatchedKind, Err("audit".to_owned()))
    );
}

#[test]
fn the_handle_path_records_the_same_outcome() {
    let dispatcher = dispatcher();

    let (fields, result) =
        traced(dispatcher.handle(envelope(EventKind::PullRequest, Some(Action::Closed))));
    assert_eq!(fields.str("outcome"), Some("handler_error"));
    assert_eq!(result.map_err(source_text), Err("routed".to_owned()));

    let (fields, result) =
        traced(dispatcher.handle(envelope(EventKind::CheckRun, Some(Action::Completed))));
    assert_eq!(fields.str("outcome"), Some("unmatched_ok"));
    result.unwrap();
}

/// A fallback that fails `check_run` deliveries and passes every other kind.
async fn fail_check_run(envelope: Envelope) -> Result<(), &'static str> {
    if envelope.meta.kind == EventKind::CheckRun {
        Err("unmatched")
    } else {
        Ok(())
    }
}

#[test]
fn a_failure_records_the_tier_the_handler_and_the_registration_site_of_the_failing_handler() {
    // The location is that of the registration method's name, so the failing
    // handler is registered on the line after `line!()`.
    let builder = Dispatcher::builder();
    let registration_line = line!() + 1;
    let dispatcher = builder.fallback(fail_check_run).build();

    let (fields, outcome) =
        traced(dispatcher.dispatch(envelope(EventKind::CheckRun, Some(Action::Completed))));
    assert_eq!(fields.str("outcome"), Some("unmatched_error"));
    assert_eq!(fields.str("tier"), Some("fallback"));
    assert_registered_on(&fields, registration_line);
    // The handler is named as the error names it: `type_name` of the
    // registered handler, here the `async fn` item's path.
    assert_eq!(
        fields.str("handler"),
        Some(std::any::type_name_of_val(&fail_check_run))
    );
    let error = outcome.result.unwrap_err();
    assert_eq!(
        fields.str("handler"),
        Some(error.handler),
        "the span and the error name the same handler"
    );
    assert_eq!(
        fields.str("registration_site"),
        Some(error.registration_site.to_string().as_str()),
        "the span and the error name the same registration site"
    );

    // A delivery that succeeds has no failing handler to name.
    let (fields, _) =
        traced(dispatcher.dispatch(envelope(EventKind::PullRequest, Some(Action::Opened))));
    assert_eq!(fields.str("outcome"), Some("unmatched_ok"));
    assert_eq!(fields.get("tier"), None);
    assert_eq!(fields.get("handler"), None);
    assert_eq!(fields.get("registration_site"), None);
}

/// Asserts the span's `registration_site` points at `line` of this file.
#[track_caller]
fn assert_registered_on(fields: &Fields, line: u32) {
    let site = fields
        .str("registration_site")
        .expect("registration site recorded");
    assert!(
        site.starts_with(&format!("{}:{line}:", file!())),
        "registration site {site} is not on line {line} of this file"
    );
}

/// A handler that is a trait's associated fn, registered as the fn item
/// `<Audit as Fallible>::fail`: `type_name` of such an item reads
/// `<Type as Trait>::method`, with spaces in it.
struct Audit;

trait Fallible {
    fn fail(envelope: Envelope) -> std::future::Ready<Result<(), &'static str>>;
}

impl Fallible for Audit {
    fn fail(_: Envelope) -> std::future::Ready<Result<(), &'static str>> {
        std::future::ready(Err("audit"))
    }
}

#[test]
fn a_handler_name_with_spaces_in_it_is_recorded_whole() {
    let dispatcher = Dispatcher::builder()
        .always(<Audit as Fallible>::fail)
        .build();

    let (fields, outcome) =
        traced(dispatcher.dispatch(envelope(EventKind::PullRequest, Some(Action::Opened))));

    let name = std::any::type_name_of_val(&<Audit as Fallible>::fail);
    assert!(
        name.contains(' '),
        "the name under test has no space in it: {name}"
    );
    assert_eq!(fields.str("handler"), Some(name));
    assert_eq!(
        fields.str("handler"),
        Some(outcome.result.unwrap_err().handler),
        "the span and the error name the same handler"
    );
}

#[test]
fn the_span_opens_with_the_delivery_id_and_event_and_the_action_and_installation_id_it_has() {
    let dispatcher = dispatcher();

    // The fields are read from what the span opened with, so they were there
    // before any handler ran, not recorded on the way out. The value's type
    // is asserted with its text: `installation_id` is the integer 42, not the
    // string "42", and the others are strings, which is what a dashboard
    // groups and compares by.
    let with_both = Envelope::new(
        "delivery",
        EventKind::PullRequest,
        payload(Some(Action::Opened), Some(42)),
    );
    let (recording, _) = common::traced(dispatcher.dispatch(with_both));
    let opened = &recording.span("octoevents.dispatch").at_open;
    assert_eq!(
        opened.get("delivery_id"),
        Some(&Value::Str("delivery".into()))
    );
    assert_eq!(
        opened.get("event"),
        Some(&Value::Str("pull_request".into()))
    );
    assert_eq!(opened.get("action"), Some(&Value::Str("opened".into())));
    assert_eq!(opened.get("installation_id"), Some(&Value::U64(42)));

    // A `ping` has neither: the fields are absent rather than empty or `None`,
    // at open and at close alike.
    let (recording, _) = common::traced(dispatcher.dispatch(envelope(EventKind::Ping, None)));
    let span = recording.span("octoevents.dispatch");
    for (when, fields) in [("open", &span.at_open), ("close", &span.at_close)] {
        assert_eq!(fields.str("event"), Some("ping"));
        assert_eq!(fields.get("action"), None, "at {when}: {fields:?}");
        assert_eq!(fields.get("installation_id"), None, "at {when}: {fields:?}");
    }
}

/// The payload every request and verification below carries: a
/// `pull_request.opened` for installation 42, 44 bytes.
const BODY: &[u8] = br#"{"action":"opened","installation":{"id":42}}"#;

/// The secret the verifier under test holds.
const SECRET: &str = "It's a Secret to Everybody";

/// A header value that is not `sha256=` and 64 hex digits: GitHub's legacy
/// SHA-1 header value. Refused from the headers, before the verifier is
/// asked; the receiver answers 400 and no verify span opens.
#[cfg(feature = "http-body")]
const MALFORMED_SIGNATURE: &str = "sha1=757107ea0eb2509fc211221cce984b8a37570b6d";

/// The verifier under test, and the one the receiver is built with; it signs
/// what they accept.
fn verifier() -> Verifier {
    Verifier::new(WebhookSecret::new(SECRET))
}

/// A verifier over a secret [`verifier`] does not hold: what it signs, the
/// verifier under test rejects.
fn another_verifier() -> Verifier {
    Verifier::new(WebhookSecret::new("another secret"))
}

/// Runs `verify` over [`BODY`] under a fresh recording subscriber and returns
/// the `octoevents.verify` span it opened alongside what it returned.
#[track_caller]
fn traced_verify(
    verifier: &Verifier,
    signature: &Signature,
) -> (SpanRecord, Result<(), SignatureError>) {
    let (recording, returned) = common::traced(async { verifier.verify(signature, BODY) });
    (recording.span("octoevents.verify").clone(), returned)
}

#[test]
fn the_verify_span_records_the_secret_count_the_body_length_and_one_of_two_outcomes() {
    // The two outcomes are the two ways `verify` can end, each read as a
    // label from what the span closed with: the signature arrives parsed, so
    // a malformed header is refused before the span opens and is no outcome
    // of it. `secret_count` and `body_len` are read from what it opened
    // with, as integers: both are known before any comparison runs, and a
    // rotated verifier counts every secret it holds, whichever of them
    // verified the signature.
    let rotated =
        Verifier::new(WebhookSecret::new("previous secret")).also(WebhookSecret::new(SECRET));
    let cases = [
        (
            "a signature under the verifier's secret",
            verifier(),
            verifier().sign(BODY),
            1,
            "verified",
            Ok(()),
        ),
        (
            "a signature under another secret",
            verifier(),
            another_verifier().sign(BODY),
            1,
            "mismatch",
            Err(SignatureError::Mismatch),
        ),
        (
            "a rotated verifier, the signature under the secret it was rotated to",
            rotated,
            verifier().sign(BODY),
            2,
            "verified",
            Ok(()),
        ),
    ];

    for (case, verifier, signature, secret_count, outcome, returned) in cases {
        let (span, result) = traced_verify(&verifier, &signature);
        assert_eq!(result, returned, "{case}");
        assert_eq!(
            span.at_open.get("secret_count"),
            Some(&Value::U64(secret_count)),
            "{case}: {}",
            span.at_open
        );
        assert_eq!(
            span.at_open.get("body_len"),
            Some(&Value::U64(44)),
            "{case}: {}",
            span.at_open
        );
        assert_eq!(
            span.at_close.get("outcome"),
            Some(&Value::Str(outcome.into())),
            "{case}: {}",
            span.at_close
        );
    }
}

#[test]
fn info_filtered_verification_leaves_the_callers_outcome_unchanged() {
    for (signature, expected) in [
        (verifier().sign(BODY), Ok(())),
        (another_verifier().sign(BODY), Err(SignatureError::Mismatch)),
    ] {
        let (recording, result) = common::traced_at(tracing::Level::INFO, async {
            let caller = tracing::info_span!("application", outcome = "pending", status = 102_u64);
            caller.in_scope(|| verifier().verify(&signature, BODY))
        });
        assert_eq!(result, expected);
        assert!(!recording.has_span("octoevents.verify"), "{recording}");
        let caller = recording.span("application");
        assert_eq!(caller.at_close, caller.at_open, "{recording}");
    }
}

/// Declare every late-bound operation field, so any stray recording is visible.
fn caller_span() -> tracing::Span {
    tracing::info_span!(
        "application",
        outcome = "pending",
        status = 102_u64,
        delivery_id = "caller-delivery",
        event = "caller-event",
        error = "caller-error",
        tier = "caller-tier",
        handler = "caller-handler",
        registration_site = "caller-site",
    )
}

#[test]
fn selectively_filtered_dispatch_leaves_the_callers_fields_unchanged() {
    use tracing::Instrument as _;

    for action in [Action::Opened, Action::Closed, Action::Reopened] {
        let dispatcher = dispatcher();
        let (recording, outcome) = common::traced_with_filter(
            tracing_subscriber::filter::filter_fn(|meta| meta.name() != "octoevents.dispatch"),
            async {
                dispatcher
                    .dispatch(envelope(EventKind::PullRequest, Some(action.clone())))
                    .instrument(caller_span())
                    .await
            },
        );
        assert_eq!(outcome.result.is_err(), action == Action::Closed);
        assert!(!recording.has_span("octoevents.dispatch"), "{recording}");
        let caller = recording.span("application");
        assert_eq!(caller.at_close, caller.at_open, "{recording}");
    }
}

#[test]
fn selectively_filtered_receive_leaves_the_callers_fields_unchanged() {
    use octoevents::{Bytes, WebhookReceiverBuilder};
    use tracing::Instrument as _;

    // Both public entry points share the receive span. Keep the bytes path
    // covered even when the transport feature is disabled.
    let paths = ["bytes"];
    #[cfg(feature = "http-body")]
    let paths = [paths[0], "request"];

    for path in paths {
        for (event, signature, fail, expected) in [
            (
                "pull_request",
                verifier().sign(BODY).to_string(),
                false,
                204,
            ),
            ("pull_request", "malformed".into(), false, 400),
            (
                "pull_request",
                another_verifier().sign(BODY).to_string(),
                false,
                401,
            ),
            ("pull_request", verifier().sign(BODY).to_string(), true, 500),
            ("ping", verifier().sign(BODY).to_string(), false, 204),
        ] {
            let observed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let error_count = observed.clone();
            let receiver = WebhookReceiverBuilder::new(verifier())
                .on_error(move |_, _: &DispatchError| {
                    error_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                })
                .build(
                    Dispatcher::builder()
                        .always(move |_: Envelope| async move {
                            tokio::task::yield_now().await;
                            if fail { Err("handler failed") } else { Ok(()) }
                        })
                        .build(),
                );
            let request = http::Request::builder()
                .header("content-type", "application/json")
                .header("x-github-delivery", "delivery")
                .header("x-github-event", event)
                .header("x-hub-signature-256", signature)
                .body(Bytes::from_static(BODY))
                .unwrap();

            let (recording, status) = common::traced_with_filter(
                tracing_subscriber::filter::filter_fn(|meta| meta.name() != "octoevents.receive"),
                async {
                    let receive = async {
                        match path {
                            "bytes" => {
                                let (parts, body) = request.into_parts();
                                receiver.receive_bytes(&parts.headers, body).await
                            }
                            #[cfg(feature = "http-body")]
                            "request" => receiver
                                .receive(request.map(http_body_util::Full::new))
                                .await
                                .status(),
                            _ => unreachable!(),
                        }
                    };
                    receive.instrument(caller_span()).await
                },
            );
            assert_eq!(status, expected, "{path}");
            assert!(!recording.has_span("octoevents.receive"), "{recording}");
            let caller = recording.span("application");
            assert_eq!(caller.at_close, caller.at_open, "{path}: {recording}");
            assert_eq!(
                observed.load(std::sync::atomic::Ordering::SeqCst),
                usize::from(fail)
            );
            if fail {
                assert_eq!(
                    recording
                        .event_at(tracing::Level::ERROR)
                        .fields
                        .debug("message"),
                    Some("handler failed")
                );
                assert_eq!(
                    recording
                        .span("octoevents.dispatch")
                        .at_close
                        .str("outcome"),
                    Some("unmatched_error")
                );
            } else {
                assert!(recording.events_at(tracing::Level::ERROR).is_empty());
            }
        }
    }
}

/// The receiver under test on the `http-body` paths: a dispatcher behind
/// [`verifier`], and the `pull_request` request for [`BODY`] it accepts or
/// refuses, depending on the signature the request carries.
#[cfg(feature = "http-body")]
mod receiving {
    use std::{
        pin::Pin,
        task::{Context, Poll},
    };

    use bytes::Bytes;
    use http::Request;
    use http_body::Frame;
    use http_body_util::Full;
    use octoevents::{DispatchError, Dispatcher, WebhookReceiver, WebhookReceiverBuilder};

    use super::{BODY, verifier};

    /// The receiver's builder, for a test that changes a setting before it
    /// builds.
    pub(super) fn builder() -> WebhookReceiverBuilder<DispatchError> {
        WebhookReceiverBuilder::new(verifier())
    }

    pub(super) fn receiver(dispatcher: Dispatcher) -> WebhookReceiver<Dispatcher> {
        builder().build(dispatcher)
    }

    /// The request the receiver accepts: [`BODY`] signed by the verifier it
    /// was built with.
    pub(super) fn signed_request() -> Request<Full<Bytes>> {
        request_with_signature(&verifier().sign(BODY).to_string())
    }

    /// The same request carrying `signature` as its `X-Hub-Signature-256`:
    /// what [`signed_request`] carries authenticates, anything else does not.
    pub(super) fn request_with_signature(signature: &str) -> Request<Full<Bytes>> {
        request_over(Full::new(Bytes::from_static(BODY)), signature)
    }

    /// The same headers over a body the test shapes itself.
    pub(super) fn request_over<B>(body: B, signature: &str) -> Request<B> {
        Request::builder()
            .header("content-type", "application/json")
            .header("x-github-delivery", "delivery")
            .header("x-github-event", "pull_request")
            .header("x-hub-signature-256", signature)
            .body(body)
            .unwrap()
    }

    /// A body whose transport fails on the first poll with the given text,
    /// as a dropped connection does: the receiver sees no data frame, only
    /// the error.
    pub(super) struct FailingBody(pub(super) &'static str);

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
}

/// The receive span closes with `outcome`, a label, and `status`, the HTTP
/// code answered, however the delivery ended: one of five labels, each beside
/// its code. A dashboard filters on the label and a subscriber renders the
/// code as a number, so the one is asserted a string and the other an
/// integer.
#[cfg(feature = "http-body")]
#[test]
fn the_receive_span_records_one_of_five_outcomes_beside_the_status_answered() {
    // One case per label, in the front page's order. The refusals are a
    // malformed signature (400), one under another secret (401) and a body
    // one byte over the limit (413), each on a request the receiver would
    // otherwise accept; the handler failure is an always tier that fails
    // every delivery.
    let cases = [
        (
            "a delivery its handler accepts",
            receiving::receiver(dispatcher()),
            receiving::signed_request(),
            "ok",
            204,
        ),
        (
            "a malformed signature",
            receiving::receiver(dispatcher()),
            receiving::request_with_signature(MALFORMED_SIGNATURE),
            "bad_request",
            400,
        ),
        (
            "a signature under another secret",
            receiving::receiver(dispatcher()),
            receiving::request_with_signature(&another_verifier().sign(BODY).to_string()),
            "unauthorized",
            401,
        ),
        (
            "a body over the limit",
            receiving::builder()
                .body_limit(BODY.len() - 1)
                .build(dispatcher()),
            receiving::signed_request(),
            "payload_too_large",
            413,
        ),
        (
            "a delivery its handler fails",
            receiving::receiver(Dispatcher::builder().always(fail_audit).build()),
            receiving::signed_request(),
            "handler_error",
            500,
        ),
    ];

    for (case, receiver, request, outcome, status) in cases {
        let (recording, response) = common::traced(receiver.receive(request));
        assert_eq!(response.status().as_u16(), status, "{case}");

        let fields = &recording.span("octoevents.receive").at_close;
        assert_eq!(
            fields.get("outcome"),
            Some(&Value::Str(outcome.into())),
            "{case}: {fields}"
        );
        assert_eq!(
            fields.get("status"),
            Some(&Value::U64(u64::from(status))),
            "{case}: {fields}"
        );
    }
}

/// A request refused before any handler ran has an error value, the
/// `ReceiveError` that selected its status, and the receive span carries its
/// text as `error`: a display value, the text alone, where the
/// failed-delivery event records the same name as an error value with its
/// chain. `outcome` says the class of the answer; `error` says which refusal
/// it was. The error's source is not on the span: beneath `BodyRead` it is
/// the transport's own text, which `tests/tracing_hygiene.rs` holds off it.
#[cfg(feature = "http-body")]
#[test]
fn a_refusal_before_any_handler_ran_records_its_error_on_the_receive_span() {
    // Two 400s the outcome alone cannot tell apart: a malformed signature and
    // a body whose first frame is the transport's error. Then an accepted
    // delivery, which refuses nothing and records no `error`.
    let receiver = receiving::receiver(dispatcher());

    let malformed = receiving::request_with_signature(MALFORMED_SIGNATURE);
    let (recording, response) = common::traced(receiver.receive(malformed));
    assert_eq!(response.status(), 400);
    let fields = &recording.span("octoevents.receive").at_close;
    assert_eq!(
        fields.get("outcome"),
        Some(&Value::Str("bad_request".into()))
    );
    assert_eq!(
        fields.debug("error"),
        Some("malformed X-Hub-Signature-256 header"),
        "{fields}"
    );

    let unreadable = receiving::request_over(
        receiving::FailingBody("connection reset by peer"),
        &verifier().sign(BODY).to_string(),
    );
    let (recording, response) = common::traced(receiver.receive(unreadable));
    assert_eq!(response.status(), 400);
    let fields = &recording.span("octoevents.receive").at_close;
    assert_eq!(
        fields.get("outcome"),
        Some(&Value::Str("bad_request".into()))
    );
    assert_eq!(
        fields.debug("error"),
        Some("could not read the webhook body"),
        "{fields}"
    );
    assert_eq!(fields.get("source"), None, "{fields}");

    let (recording, response) = common::traced(receiver.receive(receiving::signed_request()));
    assert_eq!(response.status(), 204);
    let fields = &recording.span("octoevents.receive").at_close;
    assert_eq!(fields.get("error"), None, "{fields}");
}

/// The verify span is about the HMAC comparison and nothing else: a header
/// that is not a signature is refused from the headers, before the verifier
/// is asked, so the receive span records the refusal and no verify span opens.
/// The refusal is told apart from a mismatch by the receive span's `outcome`
/// and `error`, not by an outcome of a span that never ran.
#[cfg(feature = "http-body")]
#[test]
fn a_malformed_signature_header_is_refused_before_any_verify_span_opens() {
    let receiver = receiving::receiver(dispatcher());

    let (recording, response) =
        common::traced(receiver.receive(receiving::request_with_signature(MALFORMED_SIGNATURE)));

    assert_eq!(response.status(), 400);
    assert!(!recording.has_span("octoevents.verify"), "{recording}");
    let fields = &recording.span("octoevents.receive").at_close;
    assert_eq!(
        fields.get("outcome"),
        Some(&Value::Str("bad_request".into())),
        "{fields}"
    );
    assert_eq!(
        fields.debug("error"),
        Some("malformed X-Hub-Signature-256 header"),
        "{fields}"
    );
}

/// A field recorded on more than one span is the same field to a dashboard
/// only if every span records it in the same form: the receive span learns
/// the delivery ID and event from the headers, the dispatch span from the
/// envelope, and both must record `delivery_id` as a string, not one a string
/// and one an integer or a `Debug` rendering. `outcome` is a label on every
/// span, with the HTTP status an integer field of its own on the receive span.
#[cfg(feature = "http-body")]
#[test]
fn the_receive_and_dispatch_spans_record_their_shared_fields_in_the_same_form() {
    let receiver = receiving::receiver(dispatcher());
    let (recording, response) = common::traced(receiver.receive(receiving::signed_request()));
    assert_eq!(response.status(), 204);

    let receive = &recording.span("octoevents.receive").at_close;
    let dispatch = &recording.span("octoevents.dispatch").at_close;

    for shared in ["delivery_id", "event"] {
        assert_eq!(
            receive.get(shared),
            dispatch.get(shared),
            "{shared} differs between the receive and dispatch spans:\n{receive:?}\n{dispatch:?}"
        );
    }
    assert_eq!(
        receive.get("delivery_id"),
        Some(&Value::Str("delivery".into()))
    );
    assert_eq!(
        receive.get("event"),
        Some(&Value::Str("pull_request".into()))
    );

    assert_eq!(receive.get("outcome"), Some(&Value::Str("ok".into())));
    assert_eq!(dispatch.get("outcome"), Some(&Value::Str("ok".into())));
    assert_eq!(receive.get("status"), Some(&Value::U64(204)));
}

/// The receive and dispatch spans are one per delivery and carry the fields
/// an operator filters on, so they open at INFO. The verify span is the
/// detail behind the receive span's `unauthorized` outcome, one more span
/// per delivery at scale, so it opens at DEBUG: a subscriber at INFO never
/// sees it, and one at DEBUG sees all three.
#[cfg(feature = "http-body")]
#[test]
fn the_verify_span_opens_at_debug_and_the_receive_and_dispatch_spans_at_info() {
    use tracing::Level;

    let receiver = receiving::receiver(dispatcher());

    let (recording, _) =
        common::traced_at(Level::INFO, receiver.receive(receiving::signed_request()));
    assert!(recording.has_span("octoevents.receive"), "{recording}");
    assert!(recording.has_span("octoevents.dispatch"), "{recording}");
    assert!(!recording.has_span("octoevents.verify"), "{recording}");

    let (recording, _) =
        common::traced_at(Level::DEBUG, receiver.receive(receiving::signed_request()));
    let verify = recording.span("octoevents.verify");
    assert_eq!(verify.level, Level::DEBUG);
    assert_eq!(verify.target, "octoevents::signature");
    assert_eq!(recording.span("octoevents.receive").level, Level::INFO);
    assert_eq!(recording.span("octoevents.dispatch").level, Level::INFO);
}
