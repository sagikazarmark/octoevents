//! The `tracing` feature must never record secret-derived values.
//!
//! Section 4.2 of the specification makes this a standing invariant rather than
//! a best effort: delivery ID, event name and outcome are recordable; signature
//! header values, computed MACs and secrets are not.

#![cfg(all(feature = "tracing", feature = "tower", not(target_arch = "wasm32")))]

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use hmac::{Hmac, Mac};
use http::Request;
use http_body_util::Full;
use octoevents::{Secret, Verifier, WebhookReceiverBuilder};
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
    let subscriber = tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_ansi(false)
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
        .with_max_level(tracing::Level::TRACE)
        .finish();

    let signature = signature(SECRET.as_bytes(), BODY);
    let request = Request::builder()
        .header("content-type", "application/json")
        .header("x-github-delivery", "d34db33f-delivery")
        .header("x-github-event", "pull_request")
        .header("x-hub-signature-256", &signature)
        .body(Full::new(Bytes::from_static(BODY)))
        .unwrap();

    let service = WebhookReceiverBuilder::new(Verifier::new(Secret::new(SECRET)))
        .build(|_| async { Ok::<_, ()>(()) });

    // A current-thread runtime keeps the whole call on the thread that holds
    // the subscriber default, which `with_default` does not carry across awaits.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let response = tracing::subscriber::with_default(subscriber, || {
        runtime.block_on(service.oneshot(request))
    });
    assert_eq!(response.unwrap().status(), 204);

    let logged = capture.contents();

    // The useful fields are present...
    assert!(logged.contains("d34db33f-delivery"), "logged: {logged}");
    assert!(logged.contains("pull_request"), "logged: {logged}");
    assert!(logged.contains("204"), "logged: {logged}");

    // ...and nothing secret-derived is.
    assert!(!logged.contains(SECRET), "secret leaked: {logged}");
    assert!(!logged.contains(&signature), "signature leaked: {logged}");
    let hex = signature.trim_start_matches("sha256=");
    assert!(!logged.contains(hex), "MAC leaked: {logged}");
}

fn signature(secret: &[u8], body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
    mac.update(body);
    format!("sha256={:x}", mac.finalize().into_bytes())
}
