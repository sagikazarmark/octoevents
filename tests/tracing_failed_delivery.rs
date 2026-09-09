//! A failed delivery is one event at ERROR, with or without the error's text.
//!
//! When a handler fails and the receiver answers 500, it emits one `tracing`
//! event at ERROR carrying the event meta's identifying fields (`delivery_id`,
//! `event`, and `action` and `installation_id` when the delivery has them);
//! the 500 is the receive span's `status`, not a field of the event, since a
//! handler failure is answered nothing else. Nothing else emits it: a
//! successful delivery, a request refused before any handler ran, and a
//! short-circuited `ping` are span fields only. By default the event carries no text of the error, so it
//! places no bound on the handler's error type; `trace_errors` and
//! `trace_boxed_errors` on the receiver builder put the error's text and its
//! source chain on the same event, never a second one.
//!
//! The tests read the event's fields as the recording layer in `common`
//! stored them: `error` is the text `tracing::field::display` recorded, and
//! `source` is an error value, its text and the chain of sources beneath it,
//! which a subscriber renders however it likes.

#![cfg(all(
    feature = "tracing",
    feature = "http-body",
    not(target_arch = "wasm32")
))]

mod common;

use bytes::Bytes;
use common::{Fields, Recording, Value};
use http::Request;
use http_body_util::Full;
use octoevents::{
    DecodeError, Dispatcher, Envelope, Verifier, WebhookReceiver, WebhookReceiverBuilder,
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
fn a_failed_delivery_emits_one_error_event_with_its_event_meta() {
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

/// A handler error nothing can be done with: no `Debug`, `Display` or
/// `Error`. The event must not need any of them.
struct Opaque;

#[test]
fn the_event_places_no_bound_on_the_handlers_error_type() {
    let receiver = receiver(|_: Envelope| async { Err::<(), _>(Opaque) });

    let (recording, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 500);
    let fields = failed_delivery_event(&recording);
    assert_eq!(fields.get("error"), None, "{fields:?}");
    assert_eq!(fields.get("source"), None, "{fields:?}");
}

#[test]
fn a_successful_delivery_emits_no_error_event() {
    let receiver = receiver(|_: Envelope| async { Ok::<_, ()>(()) });

    let (recording, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 204);
    assert!(recording.events_at(Level::ERROR).is_empty(), "{recording}");
}

#[test]
fn a_request_refused_before_any_handler_ran_emits_no_error_event() {
    // The handler would fail, but the signature was made under another
    // secret, so it never runs: the refusal is a status and a span field.
    let receiver = WebhookReceiverBuilder::new(Verifier::new(WebhookSecret::new("another secret")))
        .build(|_: Envelope| async { Err::<(), _>("handler failed") });

    let (recording, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 401);
    assert!(recording.events_at(Level::ERROR).is_empty(), "{recording}");
}

#[test]
fn a_short_circuited_ping_emits_no_error_event() {
    let receiver = receiver(|_: Envelope| async { Err::<(), _>("handler failed") });

    let (recording, response) = common::traced(receiver.receive(request("ping")));
    assert_eq!(response.status(), 204);
    assert!(recording.events_at(Level::ERROR).is_empty(), "{recording}");
}

#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error("database is down")]
    Database,
}

/// A three-deep chain, so a test can tell the error's text from its source
/// and its source from the chain beneath, which travels with the source as
/// one error value.
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
fn trace_errors_puts_the_errors_text_and_source_chain_on_the_one_event() {
    let dispatcher = Dispatcher::<AppError>::builder()
        .always(|_: Envelope| async { Err::<(), _>(AppError::Database) })
        .build();
    let receiver = WebhookReceiverBuilder::new(verifier())
        .trace_errors()
        .build(dispatcher);

    let (recording, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 500);

    // Still one event: the identifying fields, and the error beside them.
    // The dispatch error's `Display` says where; its source says why.
    let fields = failed_delivery_event(&recording);
    assert_identifies_the_delivery(fields);
    let error = fields.debug("error").expect("the error's text");
    assert!(
        error.starts_with("delivery delivery (pull_request.opened) failed in the always tier"),
        "{error}"
    );
    let source = fields.error("source").expect("the error's source");
    assert_eq!(source.text, "database is down");
    assert!(
        source.sources.is_empty(),
        "an error without a source has no chain beneath it: {source:?}"
    );
}

#[test]
fn trace_errors_records_the_source_as_an_error_value_with_the_chain_beneath_it() {
    let receiver = WebhookReceiverBuilder::new(verifier())
        .trace_errors()
        .build(|_: Envelope| async { Err::<(), _>(database_is_down()) });

    let (recording, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 500);

    // Without a dispatcher the handler's error is the text and its own source
    // the `source` field, an error value: the chain beneath it travels with
    // it, for the subscriber to render as it sees fit.
    let fields = failed_delivery_event(&recording);
    assert_eq!(
        fields.debug("error"),
        Some("database is down"),
        "{fields:?}"
    );
    let source = fields.error("source").expect("the error's source");
    assert_eq!(source.text, "connection refused");
    assert_eq!(source.sources, ["timed out"]);
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[test]
fn trace_boxed_errors_traces_a_dispatch_error_over_a_boxed_error() {
    // The front page's shape: a dispatcher over `Box<dyn Error + Send + Sync>`,
    // whose `DispatchError` is no `Error` and so out of `trace_errors`' reach.
    let dispatcher = Dispatcher::<BoxError>::builder()
        .always(|_: Envelope| async { Err::<(), BoxError>(Box::new(database_is_down())) })
        .build();
    let receiver = WebhookReceiverBuilder::new(verifier())
        .trace_boxed_errors()
        .build(dispatcher);

    let (recording, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 500);

    // The same one event, the same two fields: where, then why with its chain.
    let fields = failed_delivery_event(&recording);
    assert_identifies_the_delivery(fields);
    let error = fields.debug("error").expect("the error's text");
    assert!(
        error.starts_with("delivery delivery (pull_request.opened) failed in the always tier"),
        "{error}"
    );
    let source = fields.error("source").expect("the error's source");
    assert_eq!(source.text, "database is down");
    assert_eq!(source.sources, ["connection refused", "timed out"]);
}

#[test]
fn trace_boxed_errors_traces_a_boxed_error_as_the_handlers_own() {
    let receiver = WebhookReceiverBuilder::new(verifier())
        .trace_boxed_errors()
        .build(|_: Envelope| async { Err::<(), BoxError>(Box::new(database_is_down())) });

    let (recording, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 500);

    // Without a dispatcher the boxed error is the text and its source the
    // `source` field, as an unboxed one is under `trace_errors`.
    let fields = failed_delivery_event(&recording);
    assert_eq!(
        fields.debug("error"),
        Some("database is down"),
        "{fields:?}"
    );
    let source = fields.error("source").expect("the error's source");
    assert_eq!(source.text, "connection refused");
    assert_eq!(source.sources, ["timed out"]);
}

#[test]
fn trace_boxed_errors_traces_an_anyhow_error_through_its_as_ref() {
    // `anyhow::Error` is neither an `Error` nor a `Box`; it is a
    // `BoxedError` through its `AsRef<dyn Error + Send + Sync>`.
    let dispatcher = Dispatcher::<anyhow::Error>::builder()
        .always(|_: Envelope| async { Err::<(), anyhow::Error>(database_is_down().into()) })
        .build();
    let receiver = WebhookReceiverBuilder::new(verifier())
        .trace_boxed_errors()
        .build(dispatcher);

    let (recording, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 500);

    let fields = failed_delivery_event(&recording);
    let error = fields.debug("error").expect("the error's text");
    assert!(error.contains("failed in the always tier"), "{error}");
    let source = fields.error("source").expect("the error's source");
    assert_eq!(source.text, "database is down");
    assert_eq!(source.sources, ["connection refused", "timed out"]);
}

#[test]
fn trace_errors_and_an_error_observer_run_side_by_side_for_one_event() {
    use std::sync::{Arc, Mutex};

    // The observer is for what tracing does not do (a metric, a dead letter,
    // a line on stderr); the event is the receiver's. Registering both costs
    // no second event.
    let observed = Arc::new(Mutex::new(Vec::new()));
    let observer_seen = Arc::clone(&observed);
    let receiver = WebhookReceiverBuilder::new(verifier())
        .trace_errors()
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
    assert_eq!(
        fields.debug("error"),
        Some("database is down"),
        "{fields:?}"
    );
    assert_eq!(
        observed.lock().unwrap().as_slice(),
        ["delivery database is down"]
    );
}
