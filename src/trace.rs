//! Span-field recording and the failed-delivery event behind the `tracing`
//! feature.
//!
//! The contract an operator relies on (the three spans, their levels and
//! fields, the `outcome` vocabularies, the ERROR event) is documented once,
//! in the `# Tracing` section of the crate front page; this module implements
//! it and explains only the choices the implementation makes.
//!
//! The spans this crate opens (`octoevents.verify`, `octoevents.receive`,
//! `octoevents.dispatch`) declare their late-bound fields empty and fill them
//! through [`record`] on the way out. Without the feature the functions are
//! no-ops, so call sites carry no `cfg`; the only conditional code left at
//! them is the `#[instrument]` attribute that opens the span.
//!
//! A field recorded on more than one span is the same field to a subscriber
//! only if every span records it in the same form, so the shared fields have
//! one type each: `delivery_id` and `event` are `&str` on the receive and
//! dispatch spans (the header values on one, the envelope's on the other),
//! and `outcome` is a `&'static str` label on all three, with the receive
//! span's HTTP code in its own `status` field. A `Display` value goes through
//! [`record_display`] for the same reason. The failed-delivery event
//! (`handler_failed`) records the fields it shares with the spans in the
//! same forms, and which fields of the error it carries is the receiver's
//! setting, `ErrorFields`.
//!
//! Nothing secret-derived may pass through here: signature header values,
//! computed MACs, and secrets are never recorded. `tests/tracing_hygiene.rs`
//! holds that invariant; `tests/tracing_outcome.rs` holds the one above, and
//! `tests/tracing_failed_delivery.rs` the ERROR event's.

#[cfg(feature = "tracing")]
use std::error::Error;
#[cfg(all(feature = "http", feature = "tracing"))]
use std::fmt::Display;
#[cfg(feature = "http")]
use std::marker::PhantomData;

#[cfg(all(feature = "http", feature = "tracing"))]
use crate::Action;
#[cfg(feature = "http")]
use crate::EventMeta;

/// Records `value` into the named field of the current span.
#[cfg(feature = "tracing")]
pub(crate) fn record(field: &str, value: impl tracing::Value) {
    tracing::Span::current().record(field, value);
}

/// Records `value`'s [`Display`](std::fmt::Display) form into the named field
/// of the current span, as a string.
///
/// Wrapped in `tracing::field::display` instead, the value would reach the
/// subscriber debug-formatted, a different field type from the `&str` the
/// other string fields use; formatting first keeps every string field one
/// type. The allocation happens only with the feature enabled.
#[cfg(feature = "tracing")]
pub(crate) fn record_display(field: &str, value: impl std::fmt::Display) {
    record(field, value.to_string().as_str());
}

/// Whether the feature is on, so the receiver clones the event meta a
/// failure is reported with only when something will report it.
#[cfg(feature = "http")]
pub(crate) const ENABLED: bool = cfg!(feature = "tracing");

/// Which fields of the handler's error the failed-delivery event carries:
/// the receiver's setting, made on the builder.
///
/// The event always carries the event meta's identifying fields and the
/// status. Recording the error's text and source as well needs a bound on
/// the handler's error type, which the receiver itself does not place, so
/// the default, [`none`](Self::none), records neither, and `trace_errors` or
/// `trace_boxed_errors` swaps in a function that reads the error through the
/// bound it asked for. A function pointer rather than a trait object: the
/// three are known, capture nothing, and one is picked at build time.
///
/// It is one event either way, never a second one for the text: the
/// receiver knows which it emits, where an `on_error` observer emitting the
/// text could not tell the receiver to stay quiet.
///
/// The receiver is the only user, so like the receive span's `outcome`
/// label this exists with the `http` feature.
#[cfg(feature = "http")]
pub(crate) struct ErrorFields<E> {
    #[cfg(feature = "tracing")]
    emit: Option<fn(&EventMeta, &E, u16)>,
    // `fn(&E)` rather than `E`: the receiver's `Send` and `Sync` must not
    // depend on the error type, and neither must this type's.
    error: PhantomData<fn(&E)>,
}

// Hand-written for the same reason `Config`'s is: a derive would ask
// `E: Clone` and `E: Debug`, and the type holds no `E`.
#[cfg(feature = "http")]
impl<E> Clone for ErrorFields<E> {
    fn clone(&self) -> Self {
        *self
    }
}

#[cfg(feature = "http")]
impl<E> Copy for ErrorFields<E> {}

#[cfg(feature = "http")]
impl<E> ErrorFields<E> {
    /// The default: the identifying fields and the status, nothing of the
    /// error.
    pub(crate) const fn none() -> Self {
        Self {
            #[cfg(feature = "tracing")]
            emit: None,
            error: PhantomData,
        }
    }
}

#[cfg(all(feature = "http", feature = "tracing"))]
impl<E> ErrorFields<E> {
    /// Emits the event for a failed delivery, with the fields this setting
    /// asks for.
    pub(crate) fn handler_failed(self, meta: &EventMeta, error: &E, status: u16) {
        match self.emit {
            Some(emit) => emit(meta, error, status),
            None => handler_failed(meta, status, None, None),
        }
    }

    /// Whether the event carries the error, for the receiver's `Debug`.
    pub(crate) fn is_some(self) -> bool {
        self.emit.is_some()
    }

    /// The error's [`Display`] as `error` and its [`source`](Error::source)
    /// as `source`.
    pub(crate) const fn of_error() -> Self
    where
        E: Error,
    {
        Self {
            emit: Some(|meta, error, status| {
                handler_failed(meta, status, Some(error), error.source());
            }),
            error: PhantomData,
        }
    }

    /// A [`BoxedError`]'s text as `error` and its source as `source`: for an
    /// error behind a pointer, that error's own; for a `DispatchError` over
    /// one, the dispatch error's text and the boxed error.
    pub(crate) const fn of_boxed_error() -> Self
    where
        E: BoxedError,
    {
        Self {
            emit: Some(|meta, error, status| {
                handler_failed(meta, status, Some(error.text()), error.source());
            }),
            error: PhantomData,
        }
    }
}

/// The one `tracing::error!` for a failed delivery, so the event's fields
/// are declared in one place whatever the receiver was asked to record.
///
/// `error` is recorded through its `Display` and `source` as an error value
/// the subscriber walks itself. Two fields rather than the error alone as
/// one value, because a subscriber's error value must be `Error + 'static`,
/// and the error `trace_boxed_errors` traces, a
/// [`DispatchError`](crate::DispatchError) over a boxed error, is no `Error`:
/// its text and its source are all it can offer, so every shape offers the
/// same two.
#[cfg(all(feature = "http", feature = "tracing"))]
fn handler_failed(
    meta: &EventMeta,
    status: u16,
    error: Option<&dyn Display>,
    source: Option<&(dyn Error + 'static)>,
) {
    tracing::error!(
        delivery_id = meta.delivery_id.as_str(),
        event = meta.kind.as_str(),
        action = meta.action.as_ref().map(Action::as_str),
        installation_id = meta.installation_id,
        status,
        error = error.map(tracing::field::display),
        source,
        "handler failed"
    );
}

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
///
/// [`DispatchError`]: crate::DispatchError
#[cfg(feature = "tracing")]
pub trait BoxedError: sealed::Sealed {}

#[cfg(feature = "tracing")]
impl<E> BoxedError for E where E: AsRef<dyn Error + Send + Sync + 'static> {}

#[cfg(feature = "tracing")]
impl<E> BoxedError for crate::DispatchError<E> where E: AsRef<dyn Error + Send + Sync + 'static> {}

#[cfg(feature = "tracing")]
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

/// Records nothing: the `tracing` feature is disabled.
#[cfg(not(feature = "tracing"))]
pub(crate) fn record<V>(_field: &str, _value: V) {}

/// Records nothing: the `tracing` feature is disabled.
#[cfg(not(feature = "tracing"))]
pub(crate) fn record_display<V>(_field: &str, _value: V) {}

// The stubs are methods, so the receiver's call sites carry no `cfg`; the
// setting they would read does not exist without the feature.
#[cfg(all(feature = "http", not(feature = "tracing")))]
#[allow(clippy::unused_self)]
impl<E> ErrorFields<E> {
    /// Emits nothing: the `tracing` feature is disabled.
    pub(crate) fn handler_failed(self, _meta: &EventMeta, _error: &E, _status: u16) {}

    /// Never: the `tracing` feature is disabled.
    pub(crate) fn is_some(self) -> bool {
        false
    }
}
