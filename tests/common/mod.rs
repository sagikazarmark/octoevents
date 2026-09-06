//! Helpers the `tracing` integration tests share: a subscriber that writes
//! into memory, and a signer for requests the receiver will accept.
//!
//! Not a test crate of its own: each test file includes it with `mod common;`
//! and uses the part it needs.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use tracing_subscriber::fmt::{MakeWriter, format::FmtSpan};

/// An in-memory sink for the `fmt` subscriber's output.
#[derive(Clone, Default)]
pub struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    /// Everything the subscriber wrote so far.
    pub fn contents(&self) -> String {
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

/// Runs `call` under a fresh `fmt` subscriber that logs every span's `new`
/// and `close` events, and returns everything it wrote alongside what the
/// call returned.
///
/// Both events are logged so a test can tell what a span carried when it
/// opened from what it had recorded by the time it closed. Everything down
/// to TRACE is captured; [`traced_at`] is the same subscriber with a ceiling.
pub fn traced<F, T>(call: F) -> (String, T)
where
    F: Future<Output = T>,
{
    traced_at(tracing::Level::TRACE, call)
}

/// [`traced`] with the subscriber's maximum level set to `level`, so a test
/// can see what an operator filtering at that level would see.
pub fn traced_at<F, T>(level: tracing::Level, call: F) -> (String, T)
where
    F: Future<Output = T>,
{
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_ansi(false)
        .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
        .with_max_level(level)
        .finish();

    // A current-thread runtime keeps the whole call on the thread that holds
    // the subscriber default, which `with_default` does not carry across awaits.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let returned = tracing::subscriber::with_default(subscriber, || runtime.block_on(call));
    (capture.contents(), returned)
}

/// The `X-Hub-Signature-256` value GitHub would send for `body` under `secret`.
pub fn signature(secret: &[u8], body: &[u8]) -> String {
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
