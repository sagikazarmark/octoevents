//! Helpers the integration tests share: a signer for requests the receiver
//! will accept, and, under the `tracing` feature, a subscriber that records
//! every span and event as typed values.
//!
//! Not a test crate of its own: each test file includes it with `mod common;`
//! and uses the part it needs. The recording layer and its helpers are gated
//! on the feature they observe, so a test file that only signs requests can
//! include the module under any feature set.

// Each test binary compiles its own copy and uses its own subset of the
// module, so what is unused in one binary is what another is for.
#![allow(dead_code, unused_imports)]

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

#[cfg(feature = "tracing")]
mod recording;

#[cfg(feature = "tracing")]
pub use recording::{
    ErrorValue, EventRecord, Fields, Recording, SpanRecord, Value, traced, traced_at,
};

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
