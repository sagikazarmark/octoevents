//! Helpers the tracing integration tests share: a subscriber that records
//! every span and event as typed values, and the runners that install it
//! around one call.
//!
//! Not a test crate of its own: each test file includes it with `mod common;`.
//! Everything here is gated on the `tracing` feature it observes; a request
//! the receiver will accept is signed with `Verifier::sign`, which needs no
//! helper.

// Each test binary compiles its own copy and uses its own subset of the
// module, so what is unused in one binary is what another is for.
#![allow(dead_code, unused_imports)]
#![cfg(feature = "tracing")]

mod recording;

pub use recording::{
    ErrorValue, EventRecord, Fields, Recording, SpanRecord, Value, traced, traced_at,
};
