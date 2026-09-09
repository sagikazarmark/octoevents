//! The bound `WebhookReceiverBuilder::trace_boxed_errors` places on the
//! handler's error: an error behind a pointer, or a `DispatchError` over one.
//!
//! The receiver is its only consumer, and the bound exists with `tracing`,
//! as the setting that asks it does. It sits under `trace` rather than in the
//! receiver because the impl for
//! [`DispatchError`] names the dispatcher's error type, and the receiver
//! imports nothing from `dispatch`: it knows a dispatcher only as a
//! `Handler<Envelope>`, which is what makes the policy seam true.

use std::error::Error;

use crate::DispatchError;

/// An error behind a pointer, or a [`DispatchError`] over one: what
/// `WebhookReceiverBuilder::trace_boxed_errors` puts on the failed-delivery
/// event.
///
/// `Box<dyn Error + Send + Sync>` is not itself an [`Error`], since std
/// implements `Error` for `Box<E>` only for a sized `E`, so neither is a
/// [`DispatchError`] over it, and `trace_errors` refuses both. This is the
/// bound that admits them: anything that is
/// `AsRef<dyn Error + Send + Sync + 'static>`, which `Box<dyn Error + Send +
/// Sync>` and `Arc<dyn Error + Send + Sync>` are and `anyhow::Error`
/// implements, and a `DispatchError` over any of them. The crate reads the
/// error through it: its text, and its source for the subscriber to render
/// with the chain beneath. The event records those two fields rather than
/// the error as one value because a subscriber's error value must be an
/// `Error + 'static`, and a `DispatchError` over a box is not one.
///
/// Sealed: the two impls are the whole contract, and a consumer's own error
/// type that is an `Error` goes through `trace_errors` instead.
pub trait BoxedError: sealed::Sealed {}

impl<E> BoxedError for E where E: AsRef<dyn Error + Send + Sync + 'static> {}

impl<E> BoxedError for DispatchError<E> where E: AsRef<dyn Error + Send + Sync + 'static> {}

mod sealed {
    use std::{error::Error, fmt::Display};

    use crate::DispatchError;

    /// How the crate reads a [`BoxedError`](super::BoxedError): the two
    /// values the failed-delivery event records of it.
    ///
    /// The private half of the sealed trait, so the methods are the crate's
    /// and no other impl can exist. For an error behind a pointer, the text
    /// and the source are the pointed-to error's; for a dispatch error over
    /// one, the text is the dispatch error's (where) and the source is the
    /// boxed error itself (why), as [`Error::source`] would return for a
    /// `DispatchError` over an `Error`.
    pub trait Sealed {
        fn text(&self) -> &dyn Display;
        fn source(&self) -> Option<&(dyn Error + 'static)>;
    }

    impl<E> Sealed for E
    where
        E: AsRef<dyn Error + Send + Sync + 'static>,
    {
        fn text(&self) -> &dyn Display {
            self.as_ref()
        }

        fn source(&self) -> Option<&(dyn Error + 'static)> {
            self.as_ref().source()
        }
    }

    impl<E> Sealed for DispatchError<E>
    where
        E: AsRef<dyn Error + Send + Sync + 'static>,
    {
        fn text(&self) -> &dyn Display {
            self
        }

        fn source(&self) -> Option<&(dyn Error + 'static)> {
            Some(self.source.as_ref())
        }
    }
}
