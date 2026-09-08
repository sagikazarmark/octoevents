//! The `tracing` feature must never record secret-derived values.
//!
//! This is a standing invariant rather than a best effort: delivery ID, event
//! name, outcome and status are recordable; signature header values, computed
//! MACs and secrets are not. The `on_error` observer is under the same rule: it
//! sees the event meta and the handler's error, nothing the receiver derived
//! from the secret; so is the ERROR event a failed delivery emits, with or
//! without the error `trace_errors` puts on it.
//!
//! The proof walks every field of every span and event the recording layer
//! in `common` saw, down to TRACE so the verify span, the one nearest the
//! secret, is among them, and checks the text each value carries.

#![cfg(all(feature = "tracing", feature = "http", not(target_arch = "wasm32")))]

mod common;

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use common::{Recording, Value};
use http::Request;
use http_body_util::Full;
use octoevents::{EventMeta, Secret, Verifier, WebhookReceiverBuilder};
use tracing::Level;

const SECRET: &str = "It's a Secret to Everybody";
const BODY: &[u8] = br#"{"action":"opened","installation":{"id":42}}"#;

#[test]
fn spans_record_routing_metadata_but_never_the_signature_or_secret() {
    let (signature, request) = signed_request();

    let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new(SECRET)))
        .build(|_| async { Ok::<_, ()>(()) });

    let (recording, response) = common::traced(receiver.receive(request));
    assert_eq!(response.status(), 204);

    // The useful fields are present, on the span nearest the secret too...
    let receive = &recording.span("octoevents.receive").at_close;
    assert_eq!(receive.str("delivery_id"), Some("d34db33f-delivery"));
    assert_eq!(receive.str("event"), Some("pull_request"));
    assert_eq!(receive.get("status"), Some(&Value::U64(204)));
    assert_eq!(
        recording.span("octoevents.verify").at_close.str("outcome"),
        Some("verified")
    );

    // ...and nothing secret-derived is, in any field of any span or event.
    assert_no_field_secret_derived(&recording, &signature);
}

#[test]
fn the_error_observer_and_the_spans_see_nothing_secret_derived_on_a_failed_delivery() {
    let (signature, request) = signed_request();

    let observed = Arc::new(Mutex::new(String::new()));
    let observer_record = Arc::clone(&observed);
    let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new(SECRET)))
        .on_error(move |meta: &EventMeta, error: &&str| {
            *observer_record.lock().unwrap() = format!("{meta:?} {error}");
        })
        .build(|_| async { Err::<(), _>("handler failed") });

    let (recording, response) = common::traced(receiver.receive(request));
    assert_eq!(response.status(), 500);

    let receive = &recording.span("octoevents.receive").at_close;
    assert_eq!(receive.str("delivery_id"), Some("d34db33f-delivery"));
    assert_eq!(receive.get("status"), Some(&Value::U64(500)));
    assert_eq!(
        recording.event_at(Level::ERROR).fields.get("status"),
        Some(&Value::U64(500))
    );
    assert_no_field_secret_derived(&recording, &signature);

    let observed = observed.lock().unwrap();
    assert!(
        observed.contains("d34db33f-delivery"),
        "observed: {observed}"
    );
    assert!(observed.contains("handler failed"), "observed: {observed}");
    assert_nothing_secret_derived("the observer's text", &observed, &signature);
}

#[derive(Debug, thiserror::Error)]
#[error("database is down")]
struct Database;

#[test]
fn the_event_with_the_error_on_it_carries_nothing_secret_derived() {
    let (signature, request) = signed_request();

    let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new(SECRET)))
        .trace_errors()
        .build(|_| async { Err::<(), _>(Database) });

    let (recording, response) = common::traced(receiver.receive(request));
    assert_eq!(response.status(), 500);

    let fields = &recording.event_at(Level::ERROR).fields;
    assert_eq!(fields.str("delivery_id"), Some("d34db33f-delivery"));
    assert_eq!(fields.debug("error"), Some("database is down"));
    assert_no_field_secret_derived(&recording, &signature);
}

fn signed_request() -> (String, Request<Full<Bytes>>) {
    let signature = common::signature(SECRET.as_bytes(), BODY);
    let request = Request::builder()
        .header("content-type", "application/json")
        .header("x-github-delivery", "d34db33f-delivery")
        .header("x-github-event", "pull_request")
        .header("x-hub-signature-256", &signature)
        .body(Full::new(Bytes::from_static(BODY)))
        .unwrap();
    (signature, request)
}

/// Every field of every span and event in `recording` carries nothing
/// secret-derived, whatever its type: the text each value carries is checked.
#[track_caller]
fn assert_no_field_secret_derived(recording: &Recording, signature: &str) {
    let mut walked = 0;
    for (name, value) in recording.fields() {
        assert_nothing_secret_derived(&format!("field {name}"), &value.to_string(), signature);
        walked += 1;
    }
    assert!(walked > 0, "no fields recorded:\n{recording}");
}

#[track_caller]
fn assert_nothing_secret_derived(what: &str, text: &str, signature: &str) {
    assert!(!text.contains(SECRET), "secret leaked into {what}: {text}");
    assert!(
        !text.contains(signature),
        "signature leaked into {what}: {text}"
    );
    let hex = signature.trim_start_matches("sha256=");
    assert!(!text.contains(hex), "MAC leaked into {what}: {text}");
}
