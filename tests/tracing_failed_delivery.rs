//! A failed delivery is visible at ERROR without an observer.
//!
//! When a handler fails and the receiver answers 500, it emits one `tracing`
//! event at ERROR carrying the event meta's identifying fields (`delivery_id`,
//! `event`, and `action` and `installation_id` when the delivery has them)
//! and the `status` answered. Nothing else emits it: a successful delivery, a
//! request refused before any handler ran, and a short-circuited `ping` are
//! span fields only. The event carries no text of the error, so it places no
//! bound on the handler's error type; the text is the opt-in
//! [`trace_error`](octoevents::trace_error) observer's.

#![cfg(all(feature = "tracing", feature = "http", not(target_arch = "wasm32")))]

mod common;

use bytes::Bytes;
use http::Request;
use http_body_util::Full;
use octoevents::{
    DecodeError, Dispatcher, Envelope, Secret, Verifier, WebhookReceiver, WebhookReceiverBuilder,
    trace_error,
};

const SECRET: &str = "It's a Secret to Everybody";
const BODY: &[u8] = br#"{"action":"opened","installation":{"id":42}}"#;

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
        .header(
            "x-hub-signature-256",
            common::signature(SECRET.as_bytes(), body),
        )
        .body(Full::new(Bytes::from_static(body)))
        .unwrap()
}

fn receiver<H>(handler: H) -> WebhookReceiver<H>
where
    H: octoevents::Handler<Envelope> + Send + Sync + 'static,
{
    WebhookReceiverBuilder::new(Verifier::new(Secret::new(SECRET))).build(handler)
}

/// The lines the subscriber rendered for events at ERROR: span `new` and
/// `close` lines are at the span's level, so none of them is one.
fn error_events(log: &str) -> Vec<&str> {
    log.lines()
        .filter(|line| line.contains(" ERROR "))
        .collect()
}

/// The `name=value` pairs an event line carries after its message, as
/// rendered: `name="text"` for a string, `name=42` for a number.
fn rendered_field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let (_, tail) = line.rsplit_once("}: ")?;
    tail.split_whitespace().find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then_some(value)
    })
}

#[test]
fn a_failed_delivery_emits_one_error_event_with_its_event_meta_and_status() {
    let receiver = receiver(|_: Envelope| async { Err::<(), _>("handler failed") });

    let (log, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 500);

    let events = error_events(&log);
    assert_eq!(
        events.len(),
        1,
        "one ERROR event per failed delivery:\n{log}"
    );
    let event = events[0];
    assert_eq!(rendered_field(event, "delivery_id"), Some("\"delivery\""));
    assert_eq!(rendered_field(event, "event"), Some("\"pull_request\""));
    assert_eq!(rendered_field(event, "action"), Some("\"opened\""));
    assert_eq!(rendered_field(event, "installation_id"), Some("42"));
    assert_eq!(rendered_field(event, "status"), Some("500"));
}

#[test]
fn the_event_omits_the_action_and_installation_id_a_delivery_does_not_have() {
    let receiver = receiver(|_: Envelope| async { Err::<(), _>("handler failed") });

    // A `push` carries neither: the fields are absent rather than empty or
    // `None`, as they are on the dispatch span.
    let (log, response) = common::traced(receiver.receive(request_with("push", b"{}")));
    assert_eq!(response.status(), 500);

    let events = error_events(&log);
    assert_eq!(events.len(), 1, "{log}");
    let event = events[0];
    assert_eq!(rendered_field(event, "delivery_id"), Some("\"delivery\""));
    assert_eq!(rendered_field(event, "event"), Some("\"push\""));
    assert_eq!(rendered_field(event, "action"), None, "{event}");
    assert_eq!(rendered_field(event, "installation_id"), None, "{event}");
    assert_eq!(rendered_field(event, "status"), Some("500"));
}

/// A handler error nothing can be done with: no `Debug`, `Display` or
/// `Error`. The event must not need any of them.
struct Opaque;

#[test]
fn the_event_places_no_bound_on_the_handlers_error_type() {
    let receiver = receiver(|_: Envelope| async { Err::<(), _>(Opaque) });

    let (log, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 500);
    assert_eq!(error_events(&log).len(), 1, "{log}");
}

#[test]
fn a_successful_delivery_emits_no_error_event() {
    let receiver = receiver(|_: Envelope| async { Ok::<_, ()>(()) });

    let (log, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 204);
    assert!(error_events(&log).is_empty(), "{log}");
}

#[test]
fn a_request_refused_before_any_handler_ran_emits_no_error_event() {
    // The handler would fail, but the signature was made under another
    // secret, so it never runs: the refusal is a status and a span field.
    let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new("another secret")))
        .build(|_: Envelope| async { Err::<(), _>("handler failed") });

    let (log, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 401);
    assert!(error_events(&log).is_empty(), "{log}");
}

#[test]
fn a_short_circuited_ping_emits_no_error_event() {
    let receiver = receiver(|_: Envelope| async { Err::<(), _>("handler failed") });

    let (log, response) = common::traced(receiver.receive(request("ping")));
    assert_eq!(response.status(), 204);
    assert!(error_events(&log).is_empty(), "{log}");
}

#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error("database is down")]
    Database,
}

#[test]
fn the_trace_error_observer_emits_the_errors_text_and_its_source_chain() {
    let dispatcher = Dispatcher::<AppError>::builder()
        .always(|_: Envelope| async { Err::<(), _>(AppError::Database) })
        .build();
    let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new(SECRET)))
        .on_error(trace_error)
        .build(dispatcher);

    let (log, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 500);

    // The receiver's own event first, then the observer's with the text: the
    // dispatch error's `Display` says where, its source says why.
    let events = error_events(&log);
    assert_eq!(
        events.len(),
        2,
        "the receiver's event and the observer's:\n{log}"
    );
    let observed = events[1];
    assert_eq!(
        rendered_field(observed, "delivery_id"),
        Some("\"delivery\"")
    );
    assert!(
        observed.contains("failed in the always tier"),
        "the error's text: {observed}"
    );
    assert!(
        observed.contains("database is down"),
        "the error's source: {observed}"
    );
}
