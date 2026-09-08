//! The `octoevents.dispatch` span records what happened to one delivery: an
//! `outcome` label derived from the [`Outcome`] the dispatcher returns, the
//! tier, handler name and registration site of the handler that failed it,
//! and the `EventMeta` fields it ran with (`delivery_id`, `event`, `action`,
//! `installation_id`).
//!
//! The four labels are a contract dashboards filter on: `ok` and
//! `handler_error` for a matched delivery, `unmatched_ok` and
//! `unmatched_error` for an unmatched one, whichever tier failed it. The same
//! span wraps `Handler::handle`, so the receiver's path records the value
//! too, and the fields it shares with the `octoevents.receive` span are
//! recorded in the same form on both.
//!
//! The tests read what the crate recorded, through the recording layer in
//! `common`, not how a subscriber renders it: a string field is a
//! [`Value::Str`], an integer a [`Value::U64`], whatever they would look like
//! on a line.

#![cfg(all(feature = "tracing", not(target_arch = "wasm32")))]

mod common;

use common::{Fields, Value};
use octoevents::{
    Action, AnyAction, DecodeError, DispatchError, Dispatcher, Envelope, EventKind, Handler as _,
    Match, Outcome,
};

#[derive(Debug, PartialEq)]
enum AppError {
    Decode,
    Handler(&'static str),
}

impl From<DecodeError> for AppError {
    fn from(_: DecodeError) -> Self {
        Self::Decode
    }
}

impl From<&'static str> for AppError {
    fn from(message: &'static str) -> Self {
        Self::Handler(message)
    }
}

/// The match and the handlers' result with the dispatch error unwrapped to
/// its source: these tests check the span's label against what was returned,
/// not where the failing handler was registered.
fn unwrapped_outcome<E>(outcome: Outcome<E>) -> (Match, Result<(), E>) {
    (
        outcome.matched,
        outcome.result.map_err(DispatchError::into_source),
    )
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

fn dispatcher() -> Dispatcher<AppError> {
    Dispatcher::<AppError>::builder()
        .on([Action::Opened], |_: AnyPullRequest| async {
            Ok::<_, AppError>(())
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
    assert_eq!(
        outcome,
        Outcome {
            matched: Match::Matched,
            result: Ok(())
        }
    );

    let (fields, outcome) =
        traced(dispatcher.dispatch(envelope(EventKind::PullRequest, Some(Action::Closed))));
    assert_eq!(fields.str("outcome"), Some("handler_error"));
    assert_eq!(
        unwrapped_outcome(outcome),
        (Match::Matched, Err(AppError::Handler("routed")))
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
        (Match::UnmatchedKind, Err(AppError::Handler("unmatched")))
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
    let builder = Dispatcher::<AppError>::builder().on(AnyAction, |_: AnyPullRequest| async {
        Ok::<_, AppError>(())
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
        (Match::Matched, Err(AppError::Handler("audit")))
    );

    let (fields, outcome) =
        traced(dispatcher.dispatch(envelope(EventKind::CheckRun, Some(Action::Completed))));
    assert_eq!(fields.str("outcome"), Some("unmatched_error"));
    assert_eq!(fields.str("tier"), Some("always"));
    assert_registered_on(&fields, registration_line);
    assert_eq!(
        unwrapped_outcome(outcome),
        (Match::UnmatchedKind, Err(AppError::Handler("audit")))
    );
}

#[test]
fn the_handle_path_records_the_same_outcome() {
    let dispatcher = dispatcher();

    let (fields, result) =
        traced(dispatcher.handle(envelope(EventKind::PullRequest, Some(Action::Closed))));
    assert_eq!(fields.str("outcome"), Some("handler_error"));
    assert_eq!(
        result.map_err(DispatchError::into_source),
        Err(AppError::Handler("routed"))
    );

    let (fields, result) =
        traced(dispatcher.handle(envelope(EventKind::CheckRun, Some(Action::Completed))));
    assert_eq!(fields.str("outcome"), Some("unmatched_ok"));
    assert_eq!(result.map_err(DispatchError::into_source), Ok(()));
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
    let builder = Dispatcher::<AppError>::builder();
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
    let dispatcher = Dispatcher::<AppError>::builder()
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

/// The receiver under test on the `http` paths: the dispatcher above behind
/// one secret, and a signed `pull_request.opened` request for installation
/// 42 that it accepts.
#[cfg(feature = "http")]
mod receiving {
    use bytes::Bytes;
    use octoevents::{Dispatcher, Secret, Verifier, WebhookReceiver, WebhookReceiverBuilder};

    use super::AppError;

    const BODY: &[u8] = br#"{"action":"opened","installation":{"id":42}}"#;

    /// The verifier the receiver is built with; it signs the request it
    /// accepts.
    fn verifier() -> Verifier {
        Verifier::new(Secret::new("It's a Secret to Everybody"))
    }

    pub(super) fn receiver(
        dispatcher: Dispatcher<AppError>,
    ) -> WebhookReceiver<Dispatcher<AppError>> {
        WebhookReceiverBuilder::new(verifier()).build(dispatcher)
    }

    pub(super) fn signed_request() -> http::Request<http_body_util::Full<Bytes>> {
        http::Request::builder()
            .header("content-type", "application/json")
            .header("x-github-delivery", "delivery")
            .header("x-github-event", "pull_request")
            .header("x-hub-signature-256", verifier().sign(BODY))
            .body(http_body_util::Full::new(Bytes::from_static(BODY)))
            .unwrap()
    }
}

/// A field recorded on more than one span is the same field to a dashboard
/// only if every span records it in the same form: the receive span learns
/// the delivery ID and event from the headers, the dispatch span from the
/// envelope, and both must record `delivery_id` as a string, not one a string
/// and one an integer or a `Debug` rendering. `outcome` is a label on every
/// span, with the HTTP status an integer field of its own on the receive span.
#[cfg(feature = "http")]
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
/// detail behind the receive span's `unauthorized` and `bad_request`
/// outcomes, one more span per delivery at scale, so it opens at DEBUG: a
/// subscriber at INFO never sees it, and one at DEBUG sees all three.
#[cfg(feature = "http")]
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
    assert_eq!(verify.target, "octoevents::verify");
    assert_eq!(recording.span("octoevents.receive").level, Level::INFO);
    assert_eq!(recording.span("octoevents.dispatch").level, Level::INFO);
}
