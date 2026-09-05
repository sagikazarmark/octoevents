//! The `tracing` feature must never record secret-derived values.
//!
//! This is a standing invariant rather than a best effort: delivery ID, event
//! name and outcome are recordable; signature header values, computed MACs
//! and secrets are not. The `on_error` observer is under the same rule: it
//! sees the event meta and the handler's error, nothing the receiver derived
//! from the secret.

#![cfg(all(feature = "tracing", feature = "tower", not(target_arch = "wasm32")))]

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use hmac::{Hmac, KeyInit, Mac};
use http::Request;
use http_body_util::Full;
use octoevents::{EventMeta, Secret, Verifier, WebhookReceiverBuilder};
use sha2::Sha256;
use tower::ServiceExt as _;
use tracing_subscriber::fmt::MakeWriter;

const SECRET: &str = "It's a Secret to Everybody";
const BODY: &[u8] = br#"{"action":"opened","installation":{"id":42}}"#;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    fn contents(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl std::io::Write for Capture {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[test]
fn spans_record_routing_metadata_but_never_the_signature_or_secret() {
    let capture = Capture::default();
    let (signature, request) = signed_request();

    let service = WebhookReceiverBuilder::new(Verifier::new(Secret::new(SECRET)))
        .build(|_| async { Ok::<_, ()>(()) });

    let response = capture.run(|| service.oneshot(request));
    assert_eq!(response.status(), 204);

    let logged = capture.contents();

    // The useful fields are present...
    assert!(logged.contains("d34db33f-delivery"), "logged: {logged}");
    assert!(logged.contains("pull_request"), "logged: {logged}");
    assert!(logged.contains("204"), "logged: {logged}");

    // ...and nothing secret-derived is.
    assert_nothing_secret_derived(&logged, &signature);
}

#[test]
fn the_error_observer_and_the_spans_see_nothing_secret_derived_on_a_failed_delivery() {
    let capture = Capture::default();
    let (signature, request) = signed_request();

    let observed = Arc::new(Mutex::new(String::new()));
    let observer_record = Arc::clone(&observed);
    let service = WebhookReceiverBuilder::new(Verifier::new(Secret::new(SECRET)))
        .on_error(move |meta: &EventMeta, error: &&str| {
            *observer_record.lock().unwrap() = format!("{meta:?} {error}");
        })
        .build(|_| async { Err::<(), _>("handler failed") });

    let response = capture.run(|| service.oneshot(request));
    assert_eq!(response.status(), 500);

    let logged = capture.contents();
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

impl Capture {
    /// Runs `call` with a subscriber writing into this capture.
    fn run<F, Fut>(&self, call: F) -> http::Response<http_body_util::Empty<Bytes>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<
            Output = Result<http::Response<http_body_util::Empty<Bytes>>, std::convert::Infallible>,
        >,
    {
        let subscriber = tracing_subscriber::fmt()
            .with_writer(self.clone())
            .with_ansi(false)
            .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
            .with_max_level(tracing::Level::TRACE)
            .finish();

        // A current-thread runtime keeps the whole call on the thread that
        // holds the subscriber default, which `with_default` does not carry
        // across awaits.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        tracing::subscriber::with_default(subscriber, || runtime.block_on(call())).unwrap()
    }
}

fn signed_request() -> (String, Request<Full<Bytes>>) {
    let signature = signature(SECRET.as_bytes(), BODY);
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

fn signature(secret: &[u8], body: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
    mac.update(body);
    let tag = mac.finalize().into_bytes();
    let mut out = String::from("sha256=");
    for byte in tag {
        write!(out, "{byte:02x}").unwrap();
    }
    out
}
