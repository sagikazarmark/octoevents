//! Observe refusals as span-close records and handler failures as ERROR events.
//!
//! Run `cargo run --example observability --features tracing`. No server or
//! GitHub account is needed. See `docs/observability.md` for the configuration.

use http::{HeaderMap, StatusCode};
use octoevents::{
    BoxError, Bytes, Dispatcher, Envelope, EventKind, Verifier, WebhookReceiverBuilder,
    WebhookSecret, header,
};
use tracing_subscriber::fmt::format::FmtSpan;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing_subscriber::filter::LevelFilter::INFO)
        .with_span_events(FmtSpan::CLOSE)
        .init();

    assert_eq!(exercise().await, [401, 400, 413, 500]);
}

/// Drive each diagnostic path without changing the receiver or subscriber.
async fn exercise() -> [u16; 4] {
    async fn fail(_: Envelope) -> Result<(), BoxError> {
        Err(std::io::Error::other("example dependency unavailable").into())
    }

    let verifier = Verifier::new(WebhookSecret::new("test-secret"));
    let dispatcher = Dispatcher::builder().on(EventKind::Issues, fail).build();
    let receiver = WebhookReceiverBuilder::new(verifier.clone())
        .body_limit(32)
        .build(dispatcher);
    let body = Bytes::from_static(b"{}");
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    headers.insert(header::DELIVERY_ID, "diagnostic-1".parse().unwrap());
    headers.insert(header::EVENT_NAME, "issues".parse().unwrap());

    let missing = receiver.receive_bytes(&headers, body.clone()).await;
    headers.insert(header::SIGNATURE, "malformed".parse().unwrap());
    let malformed = receiver.receive_bytes(&headers, body.clone()).await;
    headers.insert(header::SIGNATURE, verifier.sign(&body).into());
    let oversized = receiver
        .receive_bytes(&headers, Bytes::from(vec![b' '; 33]))
        .await;
    let failed = receiver.receive_bytes(&headers, body).await;

    [missing, malformed, oversized, failed].map(|status: StatusCode| status.as_u16())
}

#[cfg(test)]
mod tests {
    use std::{
        io::{self, Write},
        sync::{Arc, Mutex},
    };

    use super::*;

    #[derive(Clone)]
    struct Output(Arc<Mutex<Vec<u8>>>);

    impl Write for Output {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn span_close_exposes_refusals_and_errors_include_the_source() {
        let output = Output(Arc::default());
        let writer = output.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing_subscriber::filter::LevelFilter::INFO)
            .with_span_events(FmtSpan::CLOSE)
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        // A current-thread runtime keeps the scoped subscriber active across
        // awaits. A multi-threaded server installs its subscriber globally.
        let _guard = tracing::subscriber::set_default(subscriber);
        assert_eq!(exercise().await, [401, 400, 413, 500]);

        let text = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
        for (outcome, status, error) in [
            (
                "unauthorized",
                401,
                Some("missing X-Hub-Signature-256 header"),
            ),
            (
                "bad_request",
                400,
                Some("malformed X-Hub-Signature-256 header"),
            ),
            ("payload_too_large", 413, Some("32-byte limit")),
            ("handler_error", 500, None),
        ] {
            assert!(
                text.lines().any(|line| {
                    line.contains("octoevents.receive")
                        && line.contains("close")
                        && line.contains(&format!("outcome=\"{outcome}\""))
                        && line.contains(&format!("status={status}"))
                        && error
                            .is_none_or(|message| line.contains("error=") && line.contains(message))
                }),
                "missing {outcome} receive close: {text}"
            );
        }
        assert_eq!(text.matches("handler failed").count(), 1, "{text}");
        assert!(text.contains("example dependency unavailable"), "{text}");
        assert!(text.contains("error.sources"), "{text}");
        assert!(!text.contains("test-secret"), "{text}");
        assert!(!text.contains("sha256="), "{text}");
    }
}
