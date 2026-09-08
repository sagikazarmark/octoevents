//! The `tracing` feature must never record secret-derived values.
//!
//! This is a standing invariant rather than a best effort: delivery ID, event
//! name, outcome, status and the text of a refusal are recordable; signature
//! header values, computed MACs and secrets are not. The `on_error` observer
//! is under the same rule: it sees the event meta and the handler's error,
//! nothing the receiver derived from the secret; so is the ERROR event a
//! failed delivery emits, with or without the error `trace_errors` puts on it.
//!
//! The proof walks every field of every span and event the recording layer
//! in `common` saw, down to TRACE so the verify span, the one nearest the
//! secret, is among them, and checks the text each value carries.

#![cfg(all(feature = "tracing", feature = "http", not(target_arch = "wasm32")))]

mod common;

use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use bytes::Bytes;
use common::{Recording, Value};
use http::Request;
use http_body::Frame;
use http_body_util::Full;
use octoevents::{EventMeta, Verifier, WebhookReceiverBuilder, WebhookSecret};
use tracing::Level;

/// The secret, named so the assertions can look for it in the output.
const SECRET: &str = "It's a Secret to Everybody";
const BODY: &[u8] = br#"{"action":"opened","installation":{"id":42}}"#;

/// The verifier every receiver here is built with; it signs the request
/// they accept.
fn verifier() -> Verifier {
    Verifier::new(WebhookSecret::new(SECRET))
}

#[test]
fn spans_record_routing_metadata_but_never_the_signature_or_secret() {
    let (signature, request) = signed_request();

    let receiver = WebhookReceiverBuilder::new(verifier()).build(|_| async { Ok::<_, ()>(()) });

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
    let receiver = WebhookReceiverBuilder::new(verifier())
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

    let receiver = WebhookReceiverBuilder::new(verifier())
        .trace_errors()
        .build(|_| async { Err::<(), _>(Database) });

    let (recording, response) = common::traced(receiver.receive(request));
    assert_eq!(response.status(), 500);

    let fields = &recording.event_at(Level::ERROR).fields;
    assert_eq!(fields.str("delivery_id"), Some("d34db33f-delivery"));
    assert_eq!(fields.debug("error"), Some("database is down"));
    assert_no_field_secret_derived(&recording, &signature);
}

#[test]
fn a_refusal_records_its_error_on_the_receive_span_but_nothing_secret_derived() {
    // The receive span says which refusal it was, as `error`. The two
    // refusals nearest the signature: one under another secret, whose text
    // must not carry the MAC the receiver computed or the one the request
    // carried, and a malformed header, whose text names the header and must
    // not carry its value.
    let receiver = WebhookReceiverBuilder::new(verifier()).build(|_| async { Ok::<_, ()>(()) });
    let (signature, _) = signed_request();

    let another = Verifier::new(WebhookSecret::new("another secret")).sign(BODY);
    let (recording, response) = common::traced(receiver.receive(request_signed_with(&another)));
    assert_eq!(response.status(), 401);
    let receive = &recording.span("octoevents.receive").at_close;
    assert_eq!(receive.debug("error"), Some("webhook signature mismatch"));
    assert_no_field_secret_derived(&recording, &signature);
    assert_no_field_secret_derived(&recording, &another);

    let malformed = "sha256=not-hex";
    let (recording, response) = common::traced(receiver.receive(request_signed_with(malformed)));
    assert_eq!(response.status(), 400);
    let receive = &recording.span("octoevents.receive").at_close;
    assert_eq!(
        receive.debug("error"),
        Some("malformed X-Hub-Signature-256 header")
    );
    assert_no_field_secret_derived(&recording, &signature);
    assert_no_field_secret_derived(&recording, malformed);
}

#[test]
fn a_body_the_transport_cannot_read_records_the_refusal_but_not_the_transports_text() {
    // The transport's error text is the transport's to write, and a body
    // implementation could put the request into it, signature included. The
    // refusal goes on the span as the crate's own fixed wording; what the
    // transport said stays on the `ReceiveError` value and reaches no span.
    let (signature, _) = signed_request();
    let receiver = WebhookReceiverBuilder::new(verifier()).build(|_| async { Ok::<_, ()>(()) });
    let body = FailingBody(format!(
        "stream reset while reading a request signed {signature}"
    ));

    let (recording, response) = common::traced(receiver.receive(request_over(body, &signature)));

    assert_eq!(response.status(), 400);
    let receive = &recording.span("octoevents.receive").at_close;
    assert_eq!(
        receive.debug("error"),
        Some("could not read the webhook body")
    );
    assert_eq!(receive.get("source"), None, "{receive}");
    assert_no_field_secret_derived(&recording, &signature);
}

/// A body whose transport fails on the first poll with the given text.
struct FailingBody(String);

impl http_body::Body for FailingBody {
    type Data = Bytes;
    type Error = String;

    fn poll_frame(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        Poll::Ready(Some(Err(self.0.clone())))
    }
}

fn signed_request() -> (String, Request<Full<Bytes>>) {
    let signature = verifier().sign(BODY);
    (signature.clone(), request_signed_with(&signature))
}

/// The request for [`BODY`] carrying `signature` as its `X-Hub-Signature-256`.
fn request_signed_with(signature: &str) -> Request<Full<Bytes>> {
    request_over(Full::new(Bytes::from_static(BODY)), signature)
}

/// The same headers over a body the test shapes itself.
fn request_over<B>(body: B, signature: &str) -> Request<B> {
    Request::builder()
        .header("content-type", "application/json")
        .header("x-github-delivery", "d34db33f-delivery")
        .header("x-github-event", "pull_request")
        .header("x-hub-signature-256", signature)
        .body(body)
        .unwrap()
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
