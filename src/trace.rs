//! Span-field recording behind the `tracing` feature.
//!
//! The contract an operator relies on (the three spans, their levels and
//! fields, the `outcome` vocabularies, the ERROR event) is documented once,
//! in the `# Tracing` section of the crate front page; this module implements
//! the part every span shares and explains only the choices the
//! implementation makes. What only the receiver emits, the failed-delivery
//! event, the setting that decides which fields of the error it carries, and
//! the `error` a receive span records for a refusal, lives with the receiver
//! in `service`.
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
//! [`record_display`] for the same reason, and the `error` the receive span
//! records for a refusal takes the form the failed-delivery event fixed for
//! that name.
//!
//! Nothing secret-derived may pass through here: signature header values,
//! computed MACs, and secrets are never recorded. `tests/tracing_hygiene.rs`
//! holds that invariant; `tests/tracing_outcome.rs` holds the one above, and
//! `tests/tracing_failed_delivery.rs` the ERROR event's.

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

/// Records nothing: the `tracing` feature is disabled.
#[cfg(not(feature = "tracing"))]
pub(crate) fn record<V>(_field: &str, _value: V) {}

/// Records nothing: the `tracing` feature is disabled.
#[cfg(not(feature = "tracing"))]
pub(crate) fn record_display<V>(_field: &str, _value: V) {}
