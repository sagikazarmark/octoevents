//! Receive and verify GitHub webhook events.
//!
//! `octoevents` is the receiving edge of a GitHub App: it turns an untrusted
//! HTTP request into a verified [`Envelope`] and hands it to your handlers.
//! The core is sans-I/O and wasm-safe; one receiver over `http` types serves
//! Axum, Cloudflare Workers, and anything else that can hand over a request.
//!
//! A receiver that thanks the author of every opened issue:
//!
//! ```
//! use octoevents::{Action, Dispatcher, Envelope, EventKind, Secret, Verifier};
//! # #[cfg(feature = "http")]
//! use octoevents::WebhookReceiverBuilder;
//!
//! // The error every handler returns. Any error converts into it with `?`.
//! type BoxError = Box<dyn std::error::Error + Send + Sync>;
//!
//! /// Runs for `issues.opened`. The envelope is the verified delivery: its
//! /// meta (delivery ID, kind, action, repository, sender, ...) and the raw
//! /// payload bytes.
//! async fn thank(envelope: Envelope) -> Result<(), BoxError> {
//!     let sender = envelope.meta.sender.unwrap_or_default();
//!     println!("Thank you for your contribution, @{sender}! :)");
//!     Ok(())
//! }
//!
//! // Routes each verified envelope by kind and action.
//! let dispatcher = Dispatcher::<BoxError>::builder()
//!     .on((EventKind::Issues, Action::Opened), thank)
//!     .build();
//!
//! # #[cfg(feature = "http")] {
//! // Verifies `X-Hub-Signature-256` against the body with the secret, refuses
//! // what does not verify, and hands everything else to the dispatcher.
//! let webhook = WebhookReceiverBuilder::new(Verifier::new(Secret::new("development-secret")))
//!     .build(dispatcher);
//! # let _ = webhook;
//! # }
//! ```
//!
//! `webhook` mounts on any router that hands over an `http` request: with the
//! `tower` feature as a `Service` (`post_service(webhook)` on Axum), or as a
//! plain handler calling [`WebhookReceiver::receive`]. It owns no paths or
//! methods. The response is a bare status: 204 when the handler succeeded,
//! 500 when it failed, and 401, 400 or 413 for a request that never reached
//! one.
//!
//! The [README] is the guide: a runnable quickstart, real deliveries with
//! `gh webhook forward`, handlers, the dispatcher, error handling, testing,
//! transports, security, and a Probot migration table. This page maps the
//! crate's concepts and states the contracts that belong to the API.
//!
//! [README]: https://github.com/sagikazarmark/octoevents#readme
//!
//! # Concepts
//!
//! - [`Envelope`]: the verified unit of receipt, an [`EventMeta`] beside the
//!   exact payload bytes. Produced by [`Envelope::from_signed`] on the
//!   receiving path and by [`Envelope::new`] in a test, never by a struct
//!   literal, so the meta and the bytes cannot disagree at birth. An envelope
//!   a trusted transport forwarded is read back through serde, meta as
//!   forwarded; only `from_signed` carries an authentication claim.
//! - [`EventMeta`]: the delivery ID, [`EventKind`], [`Action`], installation
//!   ID, repository, organization, sender and target, read from the headers
//!   and a best-effort probe of the payload.
//! - [`Handler<I>`](Handler): consumer code over one input `I`, any
//!   [`FromEnvelope`]: the `Envelope`, the `EventMeta`, a [`Payload`] view
//!   (a serde type declaring its kind with `#[derive(Payload)]`), or
//!   [`Event<P>`](Event) for the meta beside the payload. An `async fn`, a
//!   struct, or a closure.
//! - [`Dispatcher`]: a handler over the envelope that routes to other
//!   handlers by kind and action in three [tiers](Tier), always, route and
//!   fallback, and reports an [`Outcome`]. Built with [`DispatcherBuilder`],
//!   whose routes take an [`IntoMatcher`]: an [`EventMatcher`] shape that says
//!   its kinds, or, for a handler over a payload, actions alone or
//!   [`AnyAction`]. A failure is a [`DispatchError`]
//!   naming the tier, the handler and its registration site. The policy the
//!   tiers cannot express lives in a handler wrapping `dispatch`,
//!   [the policy seam](Dispatcher#the-policy-seam).
//! - [`WebhookReceiver`]: authenticates, bounds and dispatches one request.
//!   Built with [`WebhookReceiverBuilder`], which takes the [`Verifier`], the
//!   body limit, `ping` handling and the
//!   [`on_error`][WebhookReceiverBuilder::on_error] observer.
//! - [`Verifier`] and [`Secret`]: the configured secrets and the HMAC
//!   comparison; [`Verifier::also`] opens a rotation window, and
//!   [`Verifier::sign`] signs a test's synthetic request.
//! - [`HeaderView`], [`ResponseStatus`] and [`Envelope::from_signed`]: the
//!   sans-I/O path for a transport with no `http::Request`.
//!
//! Always pass the exact request bytes. Parsing, re-encoding, or normalizing
//! the body before verification invalidates GitHub's signature.
//!
//! # Features
//!
//! | Feature | Default | Provides |
//! | --- | --- | --- |
//! | `http` | yes | [`WebhookReceiver`] and [`WebhookReceiverBuilder`] over `http::Request`, [`HeaderView`] from an `http::HeaderMap`, [`ResponseStatus`] into `http::StatusCode` |
//! | `derive` | yes | `#[derive(Payload)]`, declaring a serde type's kind with `#[payload(EventKind::..)]`; without it, a payload is declared with a three-line `impl Payload` |
//! | `tower` | no | `tower_service::Service` for [`WebhookReceiver`] |
//! | `octocrab` | no | [`FromEnvelope`] for octocrab's `WebhookEvent`, [`Payload`] for its per-kind payload structs, `Envelope::decode_event`; see [Feature caveats](#feature-caveats) |
//! | `tracing` | no | The spans and the failed-delivery event under [Tracing](#tracing), and `trace_errors` / `trace_boxed_errors` on [`WebhookReceiverBuilder`] |
//!
//! The core (envelope, verification, the handler trait and its inputs, the
//! dispatcher) depends on none of them and builds for `wasm32-unknown-unknown`.
//!
//! # Tracing
//!
//! With the `tracing` feature, a delivery runs in three spans:
//!
//! - `octoevents.receive`, at INFO, around [`WebhookReceiver::receive`]. It
//!   records `delivery_id` and `event` from the headers as soon as they are
//!   read, before verification, and on the way out `outcome` and `status`,
//!   the HTTP code answered. `outcome` is one of `ok`, `bad_request`,
//!   `unauthorized`, `payload_too_large` and `handler_error`. A request
//!   refused before any handler ran also records the text of the
//!   [`ReceiveError`] that selected its status as `error`: `outcome` says the
//!   class of the answer, `error` which refusal it was. The text alone, the
//!   crate's own fixed wording for each refusal; the error's source, for a
//!   body that could not be read the transport's own error text, stays off
//!   the span, since a transport may quote the request in it.
//! - `octoevents.verify`, at DEBUG, around [`Verifier::verify`], inside the
//!   receive span. It records `secret_count` and `body_len` on open and
//!   `outcome` on the way out: `verified`, `malformed` or `mismatch`. It is
//!   the detail behind the receive span's `unauthorized` and `bad_request`
//!   outcomes, which is why it opens a level below them.
//! - `octoevents.dispatch`, at INFO, around [`Dispatcher::dispatch`], inside
//!   the receive span when the dispatcher is the receiver's handler. It
//!   records `delivery_id`, `event` and, when the delivery has them, `action`
//!   and `installation_id` on open; on the way out `outcome`, one of `ok`,
//!   `handler_error`, `unmatched_ok` and `unmatched_error`, and when a
//!   handler failed the [`Tier`] it ran in as `tier`, its name as `handler`
//!   and its registration site as `registration_site`.
//!
//! A field recorded in more than one place is recorded in one form
//! everywhere: `delivery_id`, `event` and `action` as strings,
//! `installation_id` and `status` as integers, `outcome` as a string label
//! with its own vocabulary per span, and `error`, on the receive span for a
//! refusal and on the failed-delivery event for a handler failure, as the
//! error's text. The one value two vocabularies share, `handler_error`,
//! partitions differently: on the receive span it is every delivery a handler
//! failed, since any handler error is a 500; on the dispatch span it is a
//! matched delivery a handler failed, and an unmatched delivery failed by its
//! `always` or `fallback` tier is `unmatched_error`. A receive
//! `handler_error` is a dispatch `handler_error` or `unmatched_error`.
//!
//! A failed delivery also emits one event at ERROR, `handler failed`, with
//! `delivery_id`, `event`, `status`, and `action` and `installation_id` when
//! the delivery has them, so a subscriber filtering at ERROR sees every
//! failed delivery without an observer. A successful delivery, a request
//! refused before any handler ran and a short-circuited `ping` emit no event.
//! By default the event carries no text of the error, since the receiver
//! places no bound on the handler's error type. The text is a setting on the
//! receiver builder, and it goes on the same event, never a second one:
//! `WebhookReceiverBuilder::trace_errors`, for a `TracedError` (any
//! `E: Error`), records the error's `Display` as `error` and its `source()`
//! as `source`, an error value the subscriber renders with the sources
//! beneath it (the `fmt` subscriber prints `error=<text> source=<cause>
//! source.sources=[<cause>, ..]`); `WebhookReceiverBuilder::trace_boxed_errors`
//! does the same for a `BoxedError`, an error behind a pointer (`Box<dyn
//! Error + Send + Sync>`, `anyhow::Error`) or a [`DispatchError`] over one,
//! which is no `Error` itself, and asking `trace_errors` of one is a compile
//! error that says so. With a dispatcher, `error` says where (the tier, the
//! handler and its registration site) and `source` why (the application
//! error). An `on_error` observer runs beside the event and changes nothing
//! about it.
//!
//! Nothing secret-derived is recorded anywhere: not the secret, the
//! signature header, nor a computed MAC.
//!
//! # Design
//!
//! What this crate declines on purpose, and why, is recorded in the
//! repository under [`docs/design/deliberately-left-out.md`][left-out].
//!
//! [left-out]: https://github.com/sagikazarmark/octoevents/blob/main/docs/design/deliberately-left-out.md
//!
//! # Feature caveats
//!
//! Enabling the `octocrab` feature makes octocrab's pre-1.0 version part of
//! this crate's public API: the `FromEnvelope` impl for its `WebhookEvent`,
//! the `Payload` impls for its per-kind payload structs, and
//! `Envelope::decode_event` expose octocrab's types, so an octocrab major
//! bump is a breaking change for handlers over them. octocrab goes in the
//! consumer's own `[dependencies]` too, to name those types; this crate
//! re-exports none of them. The core (envelope, verification, receiver, the
//! handler trait and its inputs, and the whole dispatcher) does not depend on
//! it.
//!
//! The trade-off of octocrab's types is whole-model decode: a field GitHub
//! changes fails the delivery with 500, where a view fails only on the fields
//! it names, and a test fixture is a complete payload, since the structs
//! decode no fragment. Some structs leave their main object untyped, as
//! `serde_json::Value` (`check_suite`, `workflow_run`, `workflow_job`,
//! `team`). Its per-kind payload structs omit the top-level
//! `installation`, `sender`, `repository` and `organization` objects, which
//! its `WebhookEvent` carries and [`EventMeta`] summarizes.
//!
// The receiver types exist only under `http`, and the front page names them
// under every feature set, so their link definitions are chosen by cfg: an
// intra-doc path when the item is compiled in, so a renamed or removed item
// still fails `cargo doc`, and its docs.rs URL when it is not, so the links
// resolve under `--no-default-features` rather than being dropped.
#![cfg_attr(
    feature = "http",
    doc = "[`WebhookReceiver`]: WebhookReceiver",
    doc = "[`WebhookReceiver::receive`]: WebhookReceiver::receive",
    doc = "[`WebhookReceiverBuilder`]: WebhookReceiverBuilder",
    doc = "[WebhookReceiverBuilder::on_error]: WebhookReceiverBuilder::on_error"
)]
#![cfg_attr(
    not(feature = "http"),
    doc = "[`WebhookReceiver`]: https://docs.rs/octoevents/latest/octoevents/struct.WebhookReceiver.html",
    doc = "[`WebhookReceiver::receive`]: https://docs.rs/octoevents/latest/octoevents/struct.WebhookReceiver.html#method.receive",
    doc = "[`WebhookReceiverBuilder`]: https://docs.rs/octoevents/latest/octoevents/struct.WebhookReceiverBuilder.html",
    doc = "[WebhookReceiverBuilder::on_error]: https://docs.rs/octoevents/latest/octoevents/struct.WebhookReceiverBuilder.html#method.on_error"
)]
// `doc_cfg` propagates each `#[cfg]` into the rendered docs on its own,
// including from a gated module to the items inside it, so gated items carry
// no separate `doc(cfg(...))`.
#![cfg_attr(docsrs, feature(doc_cfg))]

#[cfg(all(feature = "http", feature = "tracing"))]
mod boxed_error;
#[cfg(feature = "octocrab")]
mod decode;
mod dispatch;
mod envelope;
mod events;
mod handler;
pub mod header;
mod matcher;
mod payload;
mod respond;
mod runtime;
mod secret;
#[cfg(feature = "http")]
mod service;
#[cfg(test)]
mod test_support;
mod trace;
#[cfg(all(feature = "http", feature = "tracing"))]
mod traced_error;
mod verify;

#[cfg(all(feature = "http", feature = "tracing"))]
pub use boxed_error::BoxedError;
pub use dispatch::{DispatchError, Dispatcher, DispatcherBuilder, Match, Outcome, Tier};
pub use envelope::{
    BodyError, DecodeError, Envelope, EventMeta, HeaderView, ReceiveError, RepositoryRef,
};
pub use events::{Action, EventKind, TargetType};
pub use handler::Handler;
pub use matcher::{AnyAction, EventMatcher, IntoMatcher};
/// Derives [`Payload`] for a serde type, declaring its kind:
/// `#[derive(Payload)] #[payload(EventKind::..)]`. See the trait.
#[cfg(feature = "derive")]
pub use octoevents_derive::Payload;
pub use payload::{Event, FromEnvelope, Payload};
pub use respond::ResponseStatus;
pub use runtime::{MaybeSend, MaybeSync};
pub use secret::Secret;
#[cfg(feature = "http")]
pub use service::{WebhookReceiver, WebhookReceiverBuilder};
#[cfg(all(feature = "http", feature = "tracing"))]
pub use traced_error::TracedError;
pub use verify::{SecretError, Verifier, VerifyError};

/// The byte buffer type of [`Envelope::raw_payload`] and of the body
/// [`Envelope::from_signed`] takes, re-exported from the `bytes` crate.
///
/// A transport that never touches `bytes` otherwise builds the body from
/// here (`Bytes::from(String)`, `Bytes::from(Vec<u8>)`, or
/// `Bytes::from_static`) without adding the dependency for one type.
pub use bytes::Bytes;

/// GitHub's maximum delivered payload size: 25 MiB.
pub const DEFAULT_BODY_LIMIT: usize = 25 * 1024 * 1024;

// The README's Rust blocks compile as doctests, so its programs cannot drift
// from the API. Its quickstart mounts the receiver with `post_service`, which
// the `tower` feature provides, and its views declare their kind with
// `#[derive(Payload)]`, which the `derive` feature provides, so the blocks are
// checked under both. Blocks that continue a program rather than stand alone
// (the closure and observer fragments, and the tests) are marked `ignore` in
// the README itself; `tests/readme_testing.rs` compiles the tests.
#[cfg(all(doctest, feature = "tower", feature = "derive"))]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
