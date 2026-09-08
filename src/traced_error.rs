//! The bound `WebhookReceiverBuilder::trace_errors` places on the handler's
//! error: any [`Error`], under a name the crate can attach a message to.
//!
//! Gated as `boxed_error` is, for the same reason: the receiver is its only
//! consumer, and the receiver exists with `http`.

use std::error::Error;

/// An error `WebhookReceiverBuilder::trace_errors` can record on the
/// failed-delivery event: any [`Error`].
///
/// The bound is a trait of the crate's rather than `E: Error` itself so that
/// the one error type that is not an `Error` and is met first, `Box<dyn
/// Error + Send + Sync>`, gets a message naming `trace_boxed_errors` instead
/// of rustc's report that the pointed-to error is unsized. Nothing else
/// changes: every `Error` implements this trait through the one blanket
/// impl, a `thiserror` enum and a `DispatchError` over one included, and the
/// crate reads the error through `Error` as before, its text as `error` and
/// its `source()` as `source`.
///
/// Sealed: the blanket impl is the whole contract. A consumer's error type
/// implements `Error` and has this for free; an error behind a pointer is a
/// [`BoxedError`](crate::BoxedError) and goes through `trace_boxed_errors`.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not an `Error`, so `trace_errors` cannot record it",
    label = "`trace_errors` asks an `Error`",
    note = "`Box<dyn Error + Send + Sync>`, `anyhow::Error` and a `DispatchError` over them are traced with `trace_boxed_errors` instead"
)]
pub trait TracedError: sealed::Sealed + Error {}

// The blanket is the impl rustc would otherwise name in its report ("the
// trait `Error` is not implemented for `dyn Error`, required for `Box<dyn
// Error>` to implement `TracedError`"); hidden so the message above is what
// the reader sees.
#[diagnostic::do_not_recommend]
impl<E: Error> TracedError for E {}

mod sealed {
    pub trait Sealed {}

    impl<E: std::error::Error> Sealed for E {}
}
