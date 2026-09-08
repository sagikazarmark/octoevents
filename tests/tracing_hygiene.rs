//! The `tracing` feature must never record secret-derived values.
//!
//! This is a standing invariant rather than a best effort: delivery ID, event
//! name, outcome and status are recordable; signature header values, computed
//! MACs and secrets are not. The `on_error` observer is under the same rule: it
//! sees the event meta and the handler's error, nothing the receiver derived
//! from the secret; so is the ERROR event a failed delivery emits, with or
//! without the error `trace_errors` puts on it.

#![cfg(all(feature = "tracing", feature = "http", not(target_arch = "wasm32")))]

mod common;

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http::Request;
use http_body_util::Full;
use octoevents::{EventMeta, Secret, Verifier, WebhookReceiverBuilder};

const SECRET: &str = "It's a Secret to Everybody";
const BODY: &[u8] = br#"{"action":"opened","installation":{"id":42}}"#;

#[test]
fn spans_record_routing_metadata_but_never_the_signature_or_secret() {
    let (signature, request) = signed_request();

    let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new(SECRET)))
        .build(|_| async { Ok::<_, ()>(()) });

    let (logged, response) = common::traced(receiver.receive(request));
    assert_eq!(response.status(), 204);

    // The useful fields are present...
    assert!(logged.contains("d34db33f-delivery"), "logged: {logged}");
    assert!(logged.contains("pull_request"), "logged: {logged}");
    assert!(logged.contains("204"), "logged: {logged}");

    // ...and nothing secret-derived is.
    assert_nothing_secret_derived(&logged, &signature);
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

    let (logged, response) = common::traced(receiver.receive(request));
    assert_eq!(response.status(), 500);

    assert!(logged.contains("d34db33f-delivery"), "logged: {logged}");
    assert!(logged.contains("500"), "logged: {logged}");
    assert_nothing_secret_derived(&logged, &signature);

    let observed = observed.lock().unwrap();
    assert!(
        observed.contains("d34db33f-delivery"),
        "observed: {observed}"
    );
    assert!(observed.contains("handler failed"), "observed: {observed}");
    assert_nothing_secret_derived(&observed, &signature);
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

    let (logged, response) = common::traced(receiver.receive(request));
    assert_eq!(response.status(), 500);

    assert!(logged.contains("d34db33f-delivery"), "logged: {logged}");
    assert!(logged.contains("database is down"), "logged: {logged}");
    assert_nothing_secret_derived(&logged, &signature);
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

fn assert_nothing_secret_derived(text: &str, signature: &str) {
    assert!(!text.contains(SECRET), "secret leaked: {text}");
    assert!(!text.contains(signature), "signature leaked: {text}");
    let hex = signature.trim_start_matches("sha256=");
    assert!(!text.contains(hex), "MAC leaked: {text}");
}
