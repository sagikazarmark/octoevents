//! A failed delivery is one event at ERROR, with or without the error's text.
//!
//! When a handler fails and the receiver answers 500, it emits one `tracing`
//! event at ERROR carrying the event meta's identifying fields (`delivery_id`,
//! `event`, and `action` and `installation_id` when the delivery has them)
//! and the `status` answered. Nothing else emits it: a successful delivery, a
//! request refused before any handler ran, and a short-circuited `ping` are
//! span fields only. By default the event carries no text of the error, so it
//! places no bound on the handler's error type; `trace_errors` and
//! `trace_boxed_errors` on the receiver builder put the error's text and its
//! source chain on the same event, never a second one.

#![cfg(all(feature = "tracing", feature = "http", not(target_arch = "wasm32")))]

mod common;

use bytes::Bytes;
use http::Request;
use http_body_util::Full;
use octoevents::{
    DecodeError, Dispatcher, Envelope, Secret, Verifier, WebhookReceiver, WebhookReceiverBuilder,
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

/// The fields an event line carries after its message, as rendered:
/// `name="text"` for a string, `name=42` for a number, and an error's text
/// unquoted, so `error` and `source` run to the next field.
fn rendered_fields(line: &str) -> &str {
    line.rsplit_once("}: ").map_or(line, |(_, tail)| tail)
}

/// One `name=value` pair off an event line.
fn rendered_field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    rendered_fields(line).split_whitespace().find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then_some(value)
    })
}

/// The `error` and `source` fields off an event line: `error` is the
/// error's `Display`, `source` its first source followed by the rest of the
/// chain as `source.sources=[..]`, both unquoted, so each is everything after
/// its key up to the next key or the end of the line.
fn rendered_error_and_source(line: &str) -> (Option<&str>, Option<&str>) {
    let fields = rendered_fields(line);
    let error = fields.split_once(" error=").map(|(_, rest)| {
        rest.split_once(" source=")
            .map_or(rest, |(error, _)| error)
            .trim_end()
    });
    let source = fields
        .split_once(" source=")
        .map(|(_, source)| source.trim_end());
    (error, source)
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

/// A three-deep chain, so a test can tell the error's text from its source
/// and its source from the chain beneath, which is the subscriber's to
/// render.
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
    let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new(SECRET)))
        .trace_errors()
        .build(dispatcher);

    let (log, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 500);

    // Still one event: the identifying fields, and the error beside them.
    // The dispatch error's `Display` says where; its source says why.
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
    let (error, source) = rendered_error_and_source(event);
    let error = error.expect("the error's text");
    assert!(
        error.starts_with("delivery delivery (pull_request.opened) failed in the always tier"),
        "{error}"
    );
    assert_eq!(source, Some("database is down"), "{event}");
}

#[test]
fn trace_errors_renders_a_source_chain_through_the_subscriber() {
    let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new(SECRET)))
        .trace_errors()
        .build(|_: Envelope| async { Err::<(), _>(database_is_down()) });

    let (log, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 500);

    // Without a dispatcher the handler's error is the text and its own source
    // the `source` field; the chain beneath that is the subscriber's to
    // render, which the `fmt` subscriber does as `source.sources=[..]`.
    let events = error_events(&log);
    assert_eq!(events.len(), 1, "{log}");
    let (error, source) = rendered_error_and_source(events[0]);
    assert_eq!(error, Some("database is down"), "{}", events[0]);
    assert_eq!(
        source,
        Some("connection refused source.sources=[timed out]"),
        "{}",
        events[0]
    );
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[test]
fn trace_boxed_errors_traces_a_dispatch_error_over_a_boxed_error() {
    // The front page's shape: a dispatcher over `Box<dyn Error + Send + Sync>`,
    // whose `DispatchError` is no `Error` and so out of `trace_errors`' reach.
    let dispatcher = Dispatcher::<BoxError>::builder()
        .always(|_: Envelope| async { Err::<(), BoxError>(Box::new(database_is_down())) })
        .build();
    let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new(SECRET)))
        .trace_boxed_errors()
        .build(dispatcher);

    let (log, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 500);

    // The same one event, the same two fields: where, then why with its chain.
    let events = error_events(&log);
    assert_eq!(
        events.len(),
        1,
        "one ERROR event per failed delivery:\n{log}"
    );
    let event = events[0];
    assert_eq!(rendered_field(event, "delivery_id"), Some("\"delivery\""));
    assert_eq!(rendered_field(event, "status"), Some("500"));
    let (error, source) = rendered_error_and_source(event);
    let error = error.expect("the error's text");
    assert!(
        error.starts_with("delivery delivery (pull_request.opened) failed in the always tier"),
        "{error}"
    );
    assert_eq!(
        source,
        Some("database is down source.sources=[connection refused, timed out]"),
        "{event}"
    );
}

#[test]
fn trace_boxed_errors_traces_a_boxed_error_as_the_handlers_own() {
    let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new(SECRET)))
        .trace_boxed_errors()
        .build(|_: Envelope| async { Err::<(), BoxError>(Box::new(database_is_down())) });

    let (log, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 500);

    // Without a dispatcher the boxed error is the text and its source the
    // `source` field, as an unboxed one is under `trace_errors`.
    let events = error_events(&log);
    assert_eq!(events.len(), 1, "{log}");
    let (error, source) = rendered_error_and_source(events[0]);
    assert_eq!(error, Some("database is down"), "{}", events[0]);
    assert_eq!(
        source,
        Some("connection refused source.sources=[timed out]"),
        "{}",
        events[0]
    );
}

#[test]
fn trace_boxed_errors_traces_an_anyhow_error_through_its_as_ref() {
    // `anyhow::Error` is neither an `Error` nor a `Box`; it is a
    // `BoxedError` through its `AsRef<dyn Error + Send + Sync>`.
    let dispatcher = Dispatcher::<anyhow::Error>::builder()
        .always(|_: Envelope| async { Err::<(), anyhow::Error>(database_is_down().into()) })
        .build();
    let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new(SECRET)))
        .trace_boxed_errors()
        .build(dispatcher);

    let (log, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 500);

    let events = error_events(&log);
    assert_eq!(events.len(), 1, "{log}");
    let (error, source) = rendered_error_and_source(events[0]);
    assert!(
        error.is_some_and(|error| error.contains("failed in the always tier")),
        "{}",
        events[0]
    );
    assert_eq!(
        source,
        Some("database is down source.sources=[connection refused, timed out]"),
        "{}",
        events[0]
    );
}

#[test]
fn trace_errors_and_an_error_observer_run_side_by_side_for_one_event() {
    use std::sync::{Arc, Mutex};

    // The observer is for what tracing does not do (a metric, a dead letter,
    // a line on stderr); the event is the receiver's. Registering both costs
    // no second event.
    let observed = Arc::new(Mutex::new(Vec::new()));
    let observer_seen = Arc::clone(&observed);
    let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new(SECRET)))
        .trace_errors()
        .on_error(move |meta: &octoevents::EventMeta, error: &Database| {
            observer_seen
                .lock()
                .unwrap()
                .push(format!("{} {error}", meta.delivery_id));
        })
        .build(|_: Envelope| async { Err::<(), _>(database_is_down()) });

    let (log, response) = common::traced(receiver.receive(request("pull_request")));
    assert_eq!(response.status(), 500);

    let events = error_events(&log);
    assert_eq!(events.len(), 1, "{log}");
    let (error, _) = rendered_error_and_source(events[0]);
    assert_eq!(error, Some("database is down"), "{}", events[0]);
    assert_eq!(
        observed.lock().unwrap().as_slice(),
        ["delivery database is down"]
    );
}
