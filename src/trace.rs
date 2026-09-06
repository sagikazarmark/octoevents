//! Span-field recording and the failed-delivery events behind the `tracing`
//! feature.
//!
//! The contract an operator relies on (the three spans, their levels and
//! fields, the `outcome` vocabularies, the ERROR events) is documented once,
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
//! ([`handler_failed`]) records the fields it shares with the spans in the
//! same forms.
//!
//! Nothing secret-derived may pass through here: signature header values,
//! computed MACs, and secrets are never recorded. `tests/tracing_hygiene.rs`
//! holds that invariant; `tests/tracing_outcome.rs` holds the one above, and
//! `tests/tracing_failed_delivery.rs` the ERROR events'.

#[cfg(all(feature = "http", feature = "tracing"))]
use crate::Action;
#[cfg(any(feature = "http", feature = "tracing"))]
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

/// Emits the one ERROR event a failed delivery produces: the handler
/// returned an error and the receiver answers `status`.
///
/// The event carries the event meta's identifying fields and the status,
/// never the error: recording its text would need a `Display` bound on the
/// handler's error type, which the receiver does not place. An operator who
/// wants the text registers [`trace_error`] with `on_error`.
///
/// The receiver is the only caller, so like the receive span's `outcome`
/// label this exists with the `http` feature.
#[cfg(all(feature = "http", feature = "tracing"))]
pub(crate) fn handler_failed(meta: &EventMeta, status: u16) {
    tracing::error!(
        delivery_id = meta.delivery_id.as_str(),
        event = meta.kind.as_str(),
        action = meta.action.as_ref().map(Action::as_str),
        installation_id = meta.installation_id,
        status,
        "handler failed"
    );
}

/// Whether the feature is on, so the receiver clones the event meta a
/// failure is reported with only when something will report it.
#[cfg(feature = "http")]
pub(crate) const ENABLED: bool = cfg!(feature = "tracing");

/// An `on_error` observer that emits the handler's error as a `tracing`
/// event at ERROR, text and source chain included.
///
/// The receiver's own event for a failed delivery carries the event meta's
/// identifying fields and the status but not the error, since the receiver
/// places no bound on the error type; this observer is the one-line way to
/// get the text. It emits a second event, in the same `octoevents.receive`
/// span, with `delivery_id` and the error recorded as an `error` field of
/// type `&dyn Error`, so the subscriber renders the `Display` and walks
/// [`source`](std::error::Error::source) itself (the `fmt` subscriber prints
/// `error=<text> error.sources=[<cause>, ..]`). With a
/// [`Dispatcher`](crate::Dispatcher) as the handler the text says where (the
/// tier, the handler and the registration site) and the first source is the
/// application error, which says why.
///
/// `E` must implement [`Error`](std::error::Error): the source chain is
/// read through it. A `thiserror` enum qualifies, and so does a
/// [`DispatchError`](crate::DispatchError) over one. A boxed
/// `dyn Error + Send + Sync` does not, since std implements `Error` for
/// `Box<E>` only for a sized `E`; a closure that formats the error, as the
/// crate front page's example does, observes one.
///
/// ```
/// use octoevents::{DecodeError, Dispatcher, Envelope, Secret, Verifier, WebhookReceiverBuilder};
/// # #[derive(Debug, thiserror::Error)]
/// # enum AppError {
/// #     #[error(transparent)]
/// #     Decode(#[from] DecodeError),
/// #     #[error("database is down")]
/// #     Database,
/// # }
///
/// let dispatcher = Dispatcher::<AppError>::builder()
///     .always(|_: Envelope| async { Err::<(), _>(AppError::Database) })
///     .build();
///
/// let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new("current secret")))
///     .on_error(octoevents::trace_error)
///     .build(dispatcher);
/// # let _ = receiver;
/// ```
#[cfg(feature = "tracing")]
pub fn trace_error<E>(meta: &EventMeta, error: &E)
where
    E: std::error::Error + 'static,
{
    let error: &(dyn std::error::Error + 'static) = error;
    tracing::error!(
        delivery_id = meta.delivery_id.as_str(),
        error,
        "handler error"
    );
}

/// Records nothing: the `tracing` feature is disabled.
#[cfg(not(feature = "tracing"))]
pub(crate) fn record<V>(_field: &str, _value: V) {}

/// Records nothing: the `tracing` feature is disabled.
#[cfg(not(feature = "tracing"))]
pub(crate) fn record_display<V>(_field: &str, _value: V) {}

/// Emits nothing: the `tracing` feature is disabled.
#[cfg(all(feature = "http", not(feature = "tracing")))]
pub(crate) fn handler_failed(_meta: &EventMeta, _status: u16) {}
