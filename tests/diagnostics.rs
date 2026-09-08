//! The crate's compile-time diagnostics, held to their text.
//!
//! Five traits carry a `#[diagnostic::on_unimplemented]` so that the wrong
//! shape at a registration is answered in the crate's words: `Handler`,
//! `FromEnvelope`, `Payload`, `IntoMatcher` and `TracedError`. The rustdoc's
//! `compile_fail` blocks hold each to its error code, which does not fail
//! when the attribute stops producing the message it promises.
//! `TracedError` exists for its message alone, so its case is held here to
//! the rendered output: each file under `tests/ui/` fails to compile, and the
//! `.stderr` beside it is what rustc must say. trybuild renders the paths
//! into the crate as `src/..` with no line number and no numbered snippet,
//! so a doc edit above the bound does not stale the snapshot; the message,
//! the note, the signature and rustc's own wording do. A change to any of
//! them is re-blessed with `TRYBUILD=overwrite cargo test --all-features
//! --test diagnostics`, and the diff is reviewed as the message is.
//!
//! The fixtures use the receiver, so the harness exists with `http` and
//! `tracing` together.

#![cfg(all(feature = "http", feature = "tracing", not(target_arch = "wasm32")))]

#[test]
fn diagnostics_say_what_the_crate_promises() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/*.rs");
}
