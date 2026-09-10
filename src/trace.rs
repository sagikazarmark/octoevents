//! Span-field recording behind the `tracing` feature.
//!
//! The contract an operator relies on (the three spans, their levels and
//! fields, the `outcome` vocabularies, the ERROR event) is documented once,
//! in the `# Tracing` section of the crate front page; this module implements
//! the part every span shares and explains only the choices the
//! implementation makes. What only the receiver emits, the failed-delivery
//! event and the `error` a receive span records for a refusal, lives with the
//! receiver in `receiver`.
//!
//! The spans this crate opens (`octoevents.verify`, `octoevents.receive`,
//! `octoevents.dispatch`) declare their late-bound fields empty and fill them
//! through their explicit [`Span`] handles on the way out. A disabled span
//! discards recordings, even when an enabled caller declares the same fields.
//! Never record through the current span: filtering can leave an ancestor
//! current instead. Async operations instrument their futures with a clone
//! of the handle; only synchronous verification holds an entered-span guard.
//! The handle and recording functions are no-ops without `tracing`.
//!
//! A field recorded on more than one span is the same field to a subscriber
//! only if every span records it in the same form, so the shared fields have
//! one type each: `delivery_id` and `event` are `&str` on the receive and
//! dispatch spans (the header values on one, the envelope's on the other),
//! and `outcome` is a `&'static str` label on all three, with the receive
//! span's HTTP code in its own `status` field. A `Display` value recorded as
//! one of those string fields (the dispatch span's `registration_site`) goes
//! through [`record_display`], which formats it to a `&str` first, for the
//! same reason.
//!
//! `error` is the one name recorded in two forms, neither of them a `&str`,
//! and to every subscriber it is the error's text in both: the receive span
//! records a refusal's text alone, as a display value, its source
//! deliberately withheld (`receiver::record_refusal` says why, and why it
//! bypasses `record_display`), and the failed-delivery event records the
//! handler's error as an error value, so a subscriber that walks sources
//! renders the chain beneath it as `error.sources`, a field of its own beside
//! the text.
//!
//! Nothing secret-derived may pass through here: signature header values,
//! computed MACs, and secrets are never recorded. `tests/tracing_hygiene.rs`
//! holds that invariant; `tests/tracing_outcome.rs` holds the one above, and
//! `tests/tracing_failed_delivery.rs` the ERROR event's.

/// An operation's explicit span handle.
#[cfg(feature = "tracing")]
pub(crate) use tracing::Span;

/// A no-op operation span when the `tracing` feature is disabled.
#[cfg(not(feature = "tracing"))]
pub(crate) struct Span;

#[cfg(not(feature = "tracing"))]
impl Span {
    // Keep the same recording interface as tracing::Span across feature sets.
    #[expect(clippy::unused_self)]
    pub(crate) fn record<V>(&self, _field: &str, _value: V) {}
}

/// Records `value`'s [`Display`](std::fmt::Display) form into the named field
/// of the given span, as a string.
///
/// Wrapped in `tracing::field::display` instead, the value would reach the
/// subscriber debug-formatted, a different field type from the `&str` the
/// other string fields use; formatting first keeps every string field one
/// type. The allocation happens only with the feature enabled.
#[cfg(feature = "tracing")]
pub(crate) fn record_display(span: &Span, field: &str, value: impl std::fmt::Display) {
    span.record(field, value.to_string().as_str());
}

/// Records nothing: the `tracing` feature is disabled.
#[cfg(not(feature = "tracing"))]
pub(crate) fn record_display<V>(_span: &Span, _field: &str, _value: V) {}
