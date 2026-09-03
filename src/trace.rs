//! Span-field recording behind the `tracing` feature.
//!
//! The spans this crate opens (`octoevents.verify`, `octoevents.receive`,
//! `octoevents.dispatch`) declare their late-bound fields empty and fill them
//! through [`record`] on the way out. Without the feature the function is a
//! no-op, so call sites carry no `cfg`; the only conditional code left at
//! them is the `#[instrument]` attribute that opens the span.
//!
//! Nothing secret-derived may pass through here: signature header values,
//! computed MACs, and secrets are never recorded. `tests/tracing_hygiene.rs`
//! holds that invariant.

/// Records `value` into the named field of the current span.
#[cfg(feature = "tracing")]
pub(crate) fn record(field: &str, value: impl tracing::Value) {
    tracing::Span::current().record(field, value);
}

/// Records nothing: the `tracing` feature is disabled.
#[cfg(not(feature = "tracing"))]
pub(crate) fn record<V>(_field: &str, _value: V) {}
