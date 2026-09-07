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

#![cfg(all(feature = "tracing", not(target_arch = "wasm32")))]

mod common;

use octoevents::{
    Action, DecodeError, DispatchError, Dispatcher, Envelope, EventKind, Handler as _, Match,
    Outcome,
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

/// A view any `pull_request` payload satisfies.
#[derive(serde::Deserialize, octoevents::Payload)]
#[payload(EventKind::PullRequest)]
struct AnyPullRequest {}

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

/// The fields one span carried at one of its events, as the `fmt` subscriber
/// rendered them: `name="text"` for a string, `name=42` for a number.
///
/// Keeping the rendered form lets a test assert the value type as well as
/// the value: a quoted string and a bare number are different fields to a
/// dashboard, even when they read alike.
#[derive(Debug)]
struct SpanFields(String);

impl SpanFields {
    /// The rendered value of `name`, quotes included for a string, or `None`
    /// when the span had not recorded it.
    fn rendered(&self, name: &str) -> Option<&str> {
        self.0.split_whitespace().find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == name).then_some(value)
        })
    }

    /// The text of a string field with its quotes removed.
    fn text(&self, name: &str) -> Option<&str> {
        self.rendered(name).map(|value| value.trim_matches('"'))
    }
}

/// The fields the named span carried at its `new` or `close` event.
///
/// An event line reads `<ancestors>:<span>{<fields>}: <target>: <event> ...`,
/// each ancestor with its own fields, so the span the line is about is the
/// last one in the scope prefix: a line on which the named span is only an
/// ancestor shows what it had recorded so far, not what the event saw. The
/// span's name anchors the search for its fields, since a field value can
/// hold braces of its own: a closure's handler name ends in `{{closure}}`.
fn span_fields(log: &str, span: &str, event: &str) -> SpanFields {
    let opening = format!("{span}{{");
    let fields = log
        .lines()
        .filter(|line| line.contains(&format!(": {event}")))
        .find_map(|line| {
            let (scope, _) = line.rsplit_once("}: ")?;
            let start = scope.rfind(&opening)?;
            let at_name_boundary = scope[..start].ends_with([' ', ':']);
            let fields = &scope[start + opening.len()..];
            // A span that is only an ancestor on this line is followed by a
            // child span's name and fields. The child's name is in the needle
            // because a bare `}:` also occurs inside a handler name of nested
            // closures (`{{closure}}::{{closure}}`).
            let is_last = !fields.contains("}:octoevents.");
            (at_name_boundary && is_last).then(|| fields.to_owned())
        })
        .unwrap_or_else(|| panic!("no {span} span {event} event: {log}"));
    SpanFields(fields)
}

/// Runs `dispatch` under a fresh subscriber and returns the fields the
/// `octoevents.dispatch` span closed with alongside what the call returned.
fn traced<F, T>(dispatch: F) -> (SpanFields, T)
where
    F: Future<Output = T>,
{
    let (log, returned) = common::traced(dispatch);
    (span_fields(&log, "octoevents.dispatch", "close"), returned)
}

fn dispatcher() -> Dispatcher<AppError> {
    Dispatcher::<AppError>::builder()
        .on_payload_action([Action::Opened], |_: AnyPullRequest| async {
            Ok::<_, AppError>(())
        })
        .on_payload_action([Action::Closed], |_: AnyPullRequest| async {
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
    assert_eq!(fields.text("outcome"), Some("ok"));
    assert_eq!(
        outcome,
        Outcome {
            matched: Match::Matched,
            result: Ok(())
        }
    );

    let (fields, outcome) =
        traced(dispatcher.dispatch(envelope(EventKind::PullRequest, Some(Action::Closed))));
    assert_eq!(fields.text("outcome"), Some("handler_error"));
    assert_eq!(
        unwrapped_outcome(outcome),
        (Match::Matched, Err(AppError::Handler("routed")))
    );

    // Unmatched with the kind known and unknown both read as unmatched: the
    // label says whether the delivery was matched, not how the miss came about.
    let (fields, outcome) =
        traced(dispatcher.dispatch(envelope(EventKind::PullRequest, Some(Action::Reopened))));
    assert_eq!(fields.text("outcome"), Some("unmatched_ok"));
    assert_eq!(outcome.matched, Match::UnmatchedAction);

    let (fields, outcome) =
        traced(dispatcher.dispatch(envelope(EventKind::CheckRun, Some(Action::Completed))));
    assert_eq!(fields.text("outcome"), Some("unmatched_ok"));
    assert_eq!(outcome.matched, Match::UnmatchedKind);

    let (fields, outcome) =
        traced(dispatcher.dispatch(envelope(EventKind::Installation, Some(Action::Created))));
    assert_eq!(fields.text("outcome"), Some("unmatched_error"));
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
    let builder = Dispatcher::<AppError>::builder()
        .on_payload(|_: AnyPullRequest| async { Ok::<_, AppError>(()) });
    let registration_line = line!() + 1;
    let dispatcher = builder.always(fail_audit).build();

    // The always tier fails both deliveries before any route or fallback
    // runs. The label follows the match, not the tier that failed: the
    // unmatched one reads as `unmatched_error`, which is true whichever tier
    // failed it; the tier itself is a field of its own.
    let (fields, outcome) =
        traced(dispatcher.dispatch(envelope(EventKind::PullRequest, Some(Action::Opened))));
    assert_eq!(fields.text("outcome"), Some("handler_error"));
    assert_eq!(fields.text("tier"), Some("always"));
    assert_eq!(
        unwrapped_outcome(outcome),
        (Match::Matched, Err(AppError::Handler("audit")))
    );

    let (fields, outcome) =
        traced(dispatcher.dispatch(envelope(EventKind::CheckRun, Some(Action::Completed))));
    assert_eq!(fields.text("outcome"), Some("unmatched_error"));
    assert_eq!(fields.text("tier"), Some("always"));
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
    assert_eq!(fields.text("outcome"), Some("handler_error"));
    assert_eq!(
        result.map_err(DispatchError::into_source),
        Err(AppError::Handler("routed"))
    );

    let (fields, result) =
        traced(dispatcher.handle(envelope(EventKind::CheckRun, Some(Action::Completed))));
    assert_eq!(fields.text("outcome"), Some("unmatched_ok"));
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
    assert_eq!(fields.text("outcome"), Some("unmatched_error"));
    assert_eq!(fields.text("tier"), Some("fallback"));
    assert_registered_on(&fields, registration_line);
    // The handler is named as the error names it: `type_name` of the
    // registered handler, here the `async fn` item's path.
    assert_eq!(
        fields.text("handler"),
        Some(std::any::type_name_of_val(&fail_check_run))
    );
    let error = outcome.result.unwrap_err();
    assert_eq!(
        fields.text("handler"),
        Some(error.handler),
        "the span and the error name the same handler"
    );
    assert_eq!(
        fields.text("registration_site"),
        Some(error.registration_site.to_string().as_str()),
        "the span and the error name the same registration site"
    );

    // A delivery that succeeds has no failing handler to name.
    let (fields, _) =
        traced(dispatcher.dispatch(envelope(EventKind::PullRequest, Some(Action::Opened))));
    assert_eq!(fields.text("outcome"), Some("unmatched_ok"));
    assert_eq!(fields.rendered("tier"), None);
    assert_eq!(fields.rendered("handler"), None);
    assert_eq!(fields.rendered("registration_site"), None);
}

/// Asserts the span's `registration_site` points at `line` of this file.
fn assert_registered_on(fields: &SpanFields, line: u32) {
    let site = fields
        .text("registration_site")
        .expect("registration site recorded");
    assert!(
        site.starts_with(&format!("{}:{line}:", file!())),
        "registration site {site} is not on line {line} of this file"
    );
}

#[test]
fn the_span_opens_with_the_delivery_id_and_event_and_the_action_and_installation_id_it_has() {
    let dispatcher = dispatcher();

    // The fields are read from the span's `new` event, so they were there
    // before any handler ran, not recorded on the way out.
    let with_both = Envelope::new(
        "delivery",
        EventKind::PullRequest,
        payload(Some(Action::Opened), Some(42)),
    );
    let (log, _) = common::traced(dispatcher.dispatch(with_both));
    let fields = span_fields(&log, "octoevents.dispatch", "new");
    assert_eq!(fields.rendered("delivery_id"), Some("\"delivery\""));
    assert_eq!(fields.rendered("event"), Some("\"pull_request\""));
    assert_eq!(fields.rendered("action"), Some("\"opened\""));
    assert_eq!(fields.rendered("installation_id"), Some("42"));

    // A `ping` has neither: the fields are absent rather than empty or `None`,
    // at open and at close alike.
    let (log, _) = common::traced(dispatcher.dispatch(envelope(EventKind::Ping, None)));
    for event in ["new", "close"] {
        let fields = span_fields(&log, "octoevents.dispatch", event);
        assert_eq!(fields.text("event"), Some("ping"));
        assert_eq!(fields.rendered("action"), None, "{event}: {fields:?}");
        assert_eq!(
            fields.rendered("installation_id"),
            None,
            "{event}: {fields:?}"
        );
    }
}

/// The receiver under test on the `http` paths: the dispatcher above behind
/// one secret, and a signed `pull_request.opened` request for installation
/// 42 that it accepts.
#[cfg(feature = "http")]
mod receiving {
    use bytes::Bytes;
    use octoevents::{Dispatcher, Secret, Verifier, WebhookReceiver, WebhookReceiverBuilder};

    use super::{AppError, common};

    const SECRET: &str = "It's a Secret to Everybody";
    const BODY: &[u8] = br#"{"action":"opened","installation":{"id":42}}"#;

    pub(super) fn receiver(
        dispatcher: Dispatcher<AppError>,
    ) -> WebhookReceiver<Dispatcher<AppError>> {
        WebhookReceiverBuilder::new(Verifier::new(Secret::new(SECRET))).build(dispatcher)
    }

    pub(super) fn signed_request() -> http::Request<http_body_util::Full<Bytes>> {
        http::Request::builder()
            .header("content-type", "application/json")
            .header("x-github-delivery", "delivery")
            .header("x-github-event", "pull_request")
            .header(
                "x-hub-signature-256",
                common::signature(SECRET.as_bytes(), BODY),
            )
            .body(http_body_util::Full::new(Bytes::from_static(BODY)))
            .unwrap()
    }
}

/// A field recorded on more than one span is the same field to a dashboard
/// only if every span records it in the same form: the receive span learns
/// the delivery ID and event from the headers, the dispatch span from the
/// envelope, and both must render `delivery_id="..."`, not one quoted and one
/// bare. `outcome` is a label on every span, with the HTTP status a field of
/// its own on the receive span.
#[cfg(feature = "http")]
#[test]
fn the_receive_and_dispatch_spans_record_their_shared_fields_in_the_same_form() {
    let receiver = receiving::receiver(dispatcher());
    let (log, response) = common::traced(receiver.receive(receiving::signed_request()));
    assert_eq!(response.status(), 204);

    let receive = span_fields(&log, "octoevents.receive", "close");
    let dispatch = span_fields(&log, "octoevents.dispatch", "close");

    for shared in ["delivery_id", "event"] {
        assert_eq!(
            receive.rendered(shared),
            dispatch.rendered(shared),
            "{shared} differs between the receive and dispatch spans:\n{receive:?}\n{dispatch:?}"
        );
    }
    assert_eq!(receive.rendered("delivery_id"), Some("\"delivery\""));
    assert_eq!(receive.rendered("event"), Some("\"pull_request\""));

    assert_eq!(receive.rendered("outcome"), Some("\"ok\""));
    assert_eq!(dispatch.rendered("outcome"), Some("\"ok\""));
    assert_eq!(receive.rendered("status"), Some("204"));
}

/// The receive and dispatch spans are one per delivery and carry the fields
/// an operator filters on, so they open at INFO. The verify span is the
/// detail behind the receive span's `unauthorized` and `bad_request`
/// outcomes, one more span per delivery at scale, so it opens at DEBUG: a
/// subscriber at INFO never sees it, and one at DEBUG sees all three.
#[cfg(feature = "http")]
#[test]
fn the_verify_span_opens_at_debug_and_the_receive_and_dispatch_spans_at_info() {
    let receiver = receiving::receiver(dispatcher());

    let (log, _) = common::traced_at(
        tracing::Level::INFO,
        receiver.receive(receiving::signed_request()),
    );
    assert!(log.contains("octoevents.receive"), "{log}");
    assert!(log.contains("octoevents.dispatch"), "{log}");
    assert!(!log.contains("octoevents.verify"), "{log}");

    let (log, _) = common::traced_at(
        tracing::Level::DEBUG,
        receiver.receive(receiving::signed_request()),
    );
    let verify_lines: Vec<&str> = log
        .lines()
        .filter(|line| line.contains(": octoevents::verify: "))
        .collect();
    assert!(!verify_lines.is_empty(), "{log}");
    for line in verify_lines {
        assert!(line.contains(" DEBUG "), "{line}");
    }
}
