//! A failed delivery is one event at ERROR, carrying the error.
//!
//! When a handler fails and the receiver answers 500, it emits one `tracing`
//! event at ERROR carrying the event meta's identifying fields (`delivery_id`,
//! `event`, and `action` and `installation_id` when the delivery has them)
//! and the handler's error, boxed, as `error`, an error value whose source
//! chain the subscriber renders; the 500 is the receive span's `status`, not
//! a field of the event, since a handler failure is answered nothing else.
//! Nothing else emits it: a successful delivery, a request refused before any
//! handler ran, and a short-circuited `ping` are span fields only. An
//! `on_error` observer runs beside the event and changes nothing about it.
//!
//! The tests read the event's fields as the recording layer in `common`
//! stored them: `error` is an error value, its text and the chain of sources
//! beneath it, which a subscriber renders however it likes.

#![cfg(all(
    feature = "tracing",
    feature = "http-body",
    not(target_arch = "wasm32")
))]

mod common;

use std::convert::Infallible;

use bytes::Bytes;
use common::{Fields, Recording, Value};
use http::Request;
use http_body_util::Full;
use octoevents::{
    BoxError, Dispatcher, Envelope, Verifier, WebhookReceiver, WebhookReceiverBuilder,
    WebhookSecret,
};
use tracing::Level;

const BODY: &[u8] = br#"{"action":"opened","installation":{"id":42}}"#;

/// The verifier every receiver here is built with; it signs the requests
/// they accept.
fn verifier() -> Verifier {
    Verifier::new(WebhookSecret::new("It's a Secret to Everybody"))
}

/// A signed request for `event` carrying [`BODY`]: an action and an
/// installation.
fn request(event: &str) -> Request<Full<Bytes>> {
    request_with(event, BODY)
}

/// A signed request for `event` carrying `body`.
fn request_with(event: &str, body: &'static [u8]) -> Request<Full<Bytes>> {
    Request::builder()
        .header("content-type", "application/json")
        .header("x-github-delivery", "delivery")
        .header("x-github-event", event)
        .header("x-hub-signature-256", verifier().sign(body))
        .body(Full::new(Bytes::from_static(body)))
        .unwrap()
}

fn receiver<H>(handler: H) -> WebhookReceiver<H>
where
    H: octoevents::Handler<Envelope> + Send + Sync + 'static,
    H::Error: Into<BoxError>,
{
    WebhookReceiverBuilder::new(verifier()).build(handler)
}

/// The fields of the one failed-delivery event `recording` holds: a delivery
/// emits one event at ERROR, and spans are not events, whatever their level.
#[track_caller]
fn failed_delivery_event(recording: &Recording) -> &Fields {
    &recording.event_at(Level::ERROR).fields
}

/// Asserts the event identifies the delivery [`request`] sends: the meta's
/// fields in the form every span records them, strings and integers, and no
/// `status`, which is the receive span's.
#[track_caller]
fn assert_identifies_the_delivery(fields: &Fields) {
    assert_eq!(
        fields.get("delivery_id"),
        Some(&Value::Str("delivery".into()))
    );
    assert_eq!(
        fields.get("event"),
        Some(&Value::Str("pull_request".into()))
    );
    assert_eq!(fields.get("action"), Some(&Value::Str("opened".into())));
    assert_eq!(fields.get("installation_id"), Some(&Value::U64(42)));
    assert_eq!(fields.get("status"), None, "{fields:?}");
}

#[test]
fn a_failed_delivery_emits_one_failed_delivery_event_with_its_event_meta() {
    let receiver = receiver(|_: Envelope| async { Err::<(), _>("handler failed") });

    let (recording, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 500);

    let fields = failed_delivery_event(&recording);
    assert_eq!(fields.debug("message"), Some("handler failed"));
    assert_identifies_the_delivery(fields);
}

#[test]
fn the_event_omits_the_action_and_installation_id_a_delivery_does_not_have() {
    let receiver = receiver(|_: Envelope| async { Err::<(), _>("handler failed") });

    // A `push` carries neither: the fields are absent rather than empty or
    // `None`, as they are on the dispatch span.
    let (recording, response) = common::traced(receiver.receive(request_with("push", b"{}")));
    assert_eq!(response.status(), 500);

    let fields = failed_delivery_event(&recording);
    assert_eq!(fields.str("delivery_id"), Some("delivery"));
    assert_eq!(fields.str("event"), Some("push"));
    assert_eq!(fields.get("action"), None, "{fields:?}");
    assert_eq!(fields.get("installation_id"), None, "{fields:?}");
}

#[test]
fn a_successful_delivery_emits_no_failed_delivery_event() {
    let receiver = receiver(|_: Envelope| async { Ok::<_, Infallible>(()) });

    let (recording, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 204);
    assert!(recording.events_at(Level::ERROR).is_empty(), "{recording}");
}

#[test]
fn a_request_refused_before_any_handler_ran_emits_no_failed_delivery_event() {
    // The handler would fail, but the signature was made under another
    // secret, so it never runs: the refusal is a status and a span field.
    let receiver = WebhookReceiverBuilder::new(Verifier::new(WebhookSecret::new("another secret")))
        .build(|_: Envelope| async { Err::<(), _>("handler failed") });

    let (recording, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 401);
    assert!(recording.events_at(Level::ERROR).is_empty(), "{recording}");
}

#[test]
fn a_short_circuited_ping_emits_no_failed_delivery_event() {
    let receiver = receiver(|_: Envelope| async { Err::<(), _>("handler failed") });

    let (recording, response) = common::traced(receiver.receive(request("ping")));
    assert_eq!(response.status(), 204);
    assert!(recording.events_at(Level::ERROR).is_empty(), "{recording}");
}

/// A three-deep chain, so a test can tell the error's text from the chain
/// beneath it, which travels with the error as one value.
#[derive(Debug, thiserror::Error)]
#[error("timed out")]
struct TimedOut;

#[derive(Debug, thiserror::Error)]
#[error("connection refused")]
struct Refused(#[source] TimedOut);

#[derive(Debug, thiserror::Error)]
#[error("database is down")]
struct Database(#[source] Refused);

fn database_is_down() -> Database {
    Database(Refused(TimedOut))
}

#[test]
fn the_event_records_the_error_as_a_value_with_the_chain_beneath_it() {
    let receiver = receiver(|_: Envelope| async { Err::<(), _>(database_is_down()) });

    let (recording, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 500);

    // Without a dispatcher the handler's error is the value: its text, and
    // its sources beneath it, for the subscriber to render as it sees fit.
    let fields = failed_delivery_event(&recording);
    assert_identifies_the_delivery(fields);
    let error = fields.error("error").expect("the error");
    assert_eq!(error.text, "database is down");
    assert_eq!(error.sources, ["connection refused", "timed out"]);
    assert_eq!(fields.get("source"), None, "one field, not two: {fields:?}");
}

#[test]
fn with_a_dispatcher_the_text_says_where_and_the_chain_says_why() {
    let dispatcher = Dispatcher::builder()
        .always(|_: Envelope| async { Err::<(), _>(database_is_down()) })
        .build();
    let receiver = WebhookReceiverBuilder::new(verifier()).build(dispatcher);

    let (recording, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 500);

    // Still one event: the identifying fields, and the error beside them.
    // The dispatch error's `Display` says where; the chain beneath says why.
    let fields = failed_delivery_event(&recording);
    assert_identifies_the_delivery(fields);
    let error = fields.error("error").expect("the error");
    assert!(
        error
            .text
            .starts_with("delivery delivery (pull_request.opened) failed in the always tier"),
        "{}",
        error.text
    );
    assert_eq!(
        error.sources,
        ["database is down", "connection refused", "timed out"]
    );
}

#[test]
fn a_boxed_error_is_traced_as_the_handlers_own() {
    // The front page's shape: a handler returning `BoxError`. The box is the
    // handler's error, so its text is the pointed-to error's and the chain
    // beneath is that error's own.
    let receiver =
        receiver(|_: Envelope| async { Err::<(), BoxError>(Box::new(database_is_down())) });

    let (recording, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 500);

    let fields = failed_delivery_event(&recording);
    let error = fields.error("error").expect("the error");
    assert_eq!(error.text, "database is down");
    assert_eq!(error.sources, ["connection refused", "timed out"]);
}

#[test]
fn an_anyhow_error_is_traced_through_its_conversion_into_the_box() {
    // `anyhow::Error` is not an `Error`, but it converts into the box, which
    // is all the receiver asks; the chain beneath is anyhow's.
    let dispatcher = Dispatcher::builder()
        .always(|_: Envelope| async { Err::<(), anyhow::Error>(database_is_down().into()) })
        .build();
    let receiver = WebhookReceiverBuilder::new(verifier()).build(dispatcher);

    let (recording, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 500);

    let fields = failed_delivery_event(&recording);
    let error = fields.error("error").expect("the error");
    assert!(
        error.text.contains("failed in the always tier"),
        "{}",
        error.text
    );
    assert_eq!(
        error.sources,
        ["database is down", "connection refused", "timed out"]
    );
}

#[test]
fn a_string_error_is_traced_by_its_text() {
    // A `String` or a `&str` converts into the box too, with no chain.
    let receiver = receiver(|_: Envelope| async { Err::<(), _>(String::from("out of cheese")) });

    let (recording, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 500);

    let fields = failed_delivery_event(&recording);
    let error = fields.error("error").expect("the error");
    assert_eq!(error.text, "out of cheese");
    assert!(error.sources.is_empty(), "{error:?}");
}

#[test]
fn an_error_observer_runs_beside_the_one_event_and_changes_nothing_about_it() {
    use std::sync::{Arc, Mutex};

    // The observer is for what tracing does not do (a metric, a dead letter,
    // a line on stderr); the event is the receiver's. Registering one costs
    // no second event and takes nothing off the first. The observer sees the
    // error as the handler returned it, before the receiver boxes it.
    let observed = Arc::new(Mutex::new(Vec::new()));
    let observer_seen = Arc::clone(&observed);
    let receiver = WebhookReceiverBuilder::new(verifier())
        .on_error(move |meta: &octoevents::EventMeta, error: &Database| {
            observer_seen
                .lock()
                .unwrap()
                .push(format!("{} {error}", meta.delivery_id));
        })
        .build(|_: Envelope| async { Err::<(), _>(database_is_down()) });

    let (recording, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 500);

    let fields = failed_delivery_event(&recording);
    let error = fields.error("error").expect("the error");
    assert_eq!(error.text, "database is down");
    assert_eq!(
        observed.lock().unwrap().as_slice(),
        ["delivery database is down"]
    );
    assert_eq!(recording.events_at(Level::ERROR).len(), 1, "{recording}");
}
