//! Receive and verify GitHub webhook events.
//!
//! `octoevents` is the receiving edge of a GitHub App: it turns an untrusted
//! HTTP request into a verified [`Envelope`] and hands it to your handlers.
//! The core is wasm-safe: one receiver over `http` types serves
//! Axum, Cloudflare Workers, and anything else that can hand over a request.
//!
//! A receiver that thanks the author of every opened issue:
//!
//! ```
//! use octoevents::{
//!     Action, BoxError, Dispatcher, Envelope, EventKind, Verifier, WebhookReceiverBuilder,
//!     WebhookSecret,
//! };
//!
//! // Runs for `issues.opened`. The envelope is the verified unit of receipt:
//! // its meta (delivery ID, kind, action, repository, sender, ...) and the
//! // raw payload bytes. `BoxError` is the crate's erased error; any
//! // `Error + Send + Sync + 'static` converts into it with `?`, and a
//! // handler with an error type of its own keeps it.
//! async fn thank(envelope: Envelope) -> Result<(), BoxError> {
//!     let sender = envelope.meta.sender.map(|s| s.login).unwrap_or_default();
//!     println!("Thank you for your contribution, @{sender}! :)");
//!     Ok(())
//! }
//!
//! // Routes each verified envelope by kind and action.
//! let dispatcher = Dispatcher::builder()
//!     .on((EventKind::Issues, Action::Opened), thank)
//!     .build();
//!
//! // Verifies `X-Hub-Signature-256` against the body with the secret, refuses
//! // what does not verify, and hands everything else to the dispatcher.
//! let webhook = WebhookReceiverBuilder::new(Verifier::new(WebhookSecret::new("development-secret")))
//!     .build(dispatcher);
//! # let _ = webhook;
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
//! [README]: https://github.com/sagikazarmark/octoevents
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
//!   ID, repository, organization, sender and target. The first two and the
//!   target come from the headers; the rest come from the *probe*, a
//!   best-effort read of the payload that runs when the envelope is built,
//!   keeps five top-level values, skips the rest, and never fails. It runs
//!   for every envelope, whatever the handler's input will be, because the
//!   dispatcher routes by the action and the action is in the payload; it is
//!   the one read of the payload before a handler's input decodes it. Its
//!   cost is stated on `EventMeta`.
//! - [`Handler<I>`](Handler): consumer code over one input `I`, any
//!   [`FromEnvelope`]: the `Envelope`, the `EventMeta`, a [`Payload`] view
//!   (a serde type declaring its kind with `#[derive(Payload)]`), or
//!   [`Event<P>`](Event) for the meta beside the payload. An `async fn`, a
//!   struct, a closure, or an `Arc` of any of them, with any error that
//!   converts into [`BoxError`]: an `Error + Send + Sync + 'static` type of
//!   its own (any `Error + 'static` on `wasm32`), `BoxError` itself,
//!   `anyhow::Error`, a `String`.
//! - [`Dispatcher`]: a handler over the envelope that routes to other
//!   handlers by kind and action in three [tiers](Tier), always, route and
//!   fallback, and reports an [`Outcome`]. Built with [`DispatcherBuilder`],
//!   whose routes take an [`IntoMatcher`]: an [`EventMatcher`] shape that says
//!   its kinds, or, for a handler over a payload, actions alone or
//!   [`AnyAction`]. Each handler's error is boxed where it is registered,
//!   so handlers share no error enum. A failure is a [`DispatchError`]
//!   naming the tier, the handler and its registration site, the boxed
//!   error its source. The policy the tiers cannot express lives in a
//!   handler wrapping `dispatch`, [the policy seam](Dispatcher#the-policy-seam).
//! - [`WebhookReceiver`]: authenticates, bounds and dispatches one request,
//!   through [`WebhookReceiver::receive`] over an `http::Request` (`http-body`
//!   feature) or [`WebhookReceiver::receive_bytes`] over the `http::HeaderMap`
//!   and the body already read, answered as the `http::StatusCode`. Built
//!   with [`WebhookReceiverBuilder`], which takes the [`Verifier`], the body
//!   limit, `ping` handling and the
//!   [`on_error`][WebhookReceiverBuilder::on_error] observer.
//! - [`Verifier`] and [`WebhookSecret`]: the configured secrets and the HMAC
//!   comparison; [`Verifier::also`] opens a rotation window, and
//!   [`Verifier::sign`] signs a test's synthetic request. The header value
//!   parsed is a [`Signature`], which is where a malformed one is refused; a
//!   signature that does not authenticate is a [`SignatureError`].
//! - [`Envelope::from_signed`] and [`ReceiveError::status`]: the receiving
//!   path for a transport that wants the envelope and not the receiver's
//!   answer. `from_signed` takes the same `http::HeaderMap` and body bytes
//!   the receiver does and produces the envelope; a failure is a
//!   [`ReceiveError`], and `status` maps it to the `http::StatusCode` the
//!   receiver would answer. The header names are in [`header`].
//!
//! Always pass the exact request bytes. Parsing, re-encoding, or normalizing
//! the body before verification invalidates GitHub's signature.
//!
//! # Features
//!
//! | Feature | Default | Provides |
//! | --- | --- | --- |
//! | `http-body` | yes | [`WebhookReceiver::receive`] over an `http::Request` whose body is an `http_body::Body`, answering an `http::Response`; the receiver itself, its builder and [`WebhookReceiver::receive_bytes`] are in the core |
//! | `derive` | yes | `#[derive(Payload)]`, declaring a serde type's kind with `#[payload(EventKind::..)]`; without it, a payload is declared with a three-line `impl Payload` |
//! | `tower` | no | `tower_service::Service` for [`WebhookReceiver`] |
//! | `octocrab` | no | [`FromEnvelope`] for octocrab's `WebhookEvent` and [`Payload`] for its per-kind payload structs; see [Feature caveats](#feature-caveats) |
//! | `tracing` | no | The spans and the failed-delivery event under [Tracing](#tracing) |
//!
//! The core (envelope, verification, the handler trait and its inputs, the
//! dispatcher, the receiver over headers and bytes) depends on none of them
//! and builds for `wasm32-unknown-unknown`. [`Envelope::from_signed`] and
//! [`WebhookReceiver::receive_bytes`] over an `http::HeaderMap`, the
//! [`header`] constants and [`ReceiveError::status`] as an `http::StatusCode`
//! are part of it: the `http` crate is not optional, since every surveyed
//! Rust runtime hands over its types, and it adds one entry to the dependency
//! tree.
//!
//! # Tracing
//!
//! With the `tracing` feature, a delivery runs in three spans:
//!
//! - `octoevents.receive`, at INFO, around [`WebhookReceiver::receive`] and
//!   [`WebhookReceiver::receive_bytes`] alike, which share it. It
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
//!   `outcome` on the way out: `verified` or `mismatch`. It is the detail
//!   behind the receive span's `unauthorized` outcome, which is why it opens
//!   a level below it. A header that is absent or malformed is refused from
//!   the headers before the verifier is asked, so it opens no verify span:
//!   the receive span records that refusal as `bad_request` or
//!   `unauthorized` with the refusal's text as `error`.
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
//! `installation_id` as an integer, and `outcome` as a string label with its
//! own vocabulary per span. `error` is the error's text to every subscriber
//! wherever it appears; the failed-delivery event records it as an error
//! value, so a subscriber that walks sources renders the chain beneath it
//! too, where the receive span records a refusal's text alone. The one value
//! two vocabularies share, `handler_error`, partitions differently on the two
//! spans:
//!
//! - on the receive span it is every delivery a handler failed, since any
//!   handler error is a 500;
//! - on the dispatch span it is a *matched* delivery a handler failed; an
//!   unmatched delivery failed by its `always` or `fallback` tier is
//!   `unmatched_error`.
//!
//! So a receive `handler_error` is a dispatch `handler_error` or
//! `unmatched_error`.
//!
//! A failed delivery also emits one event at ERROR, `handler failed`, with
//! `delivery_id`, `event`, and `action` and `installation_id` when the
//! delivery has them, and the handler's error, boxed, as `error`: an error
//! value, so the subscriber renders its text and the chain of sources beneath
//! it (the `fmt` subscriber prints `error=<text> error.sources=[<cause>,
//! ..]`). With a dispatcher the text says where (the tier, the handler and
//! its registration site) and the chain why (the application error, and its
//! own sources). A subscriber filtering at ERROR sees every failed delivery
//! and why, with no observer and no setting; the 500 it is answered with is
//! the receive span's `status`, since a handler failure is answered nothing
//! else. A successful delivery, a request refused before any handler ran and
//! a short-circuited `ping` emit no event. An `on_error` observer runs beside
//! the event and changes nothing about it.
//!
//! Nothing secret-derived is recorded anywhere: not the secret, the
//! signature header, nor a computed MAC.
//!
//! # Feature caveats
//!
//! Enabling the `octocrab` feature makes octocrab's pre-1.0 version part of
//! this crate's public API: the `FromEnvelope` impl for its `WebhookEvent`
//! and the `Payload` impls for its per-kind payload structs expose octocrab's
//! types, so an octocrab major bump is a breaking change for handlers over
//! them. octocrab goes in the
//! consumer's own `[dependencies]` too, to name those types; this crate
//! re-exports none of them. The core (envelope, verification, receiver, the
//! handler trait and its inputs, and the whole dispatcher) does not depend on
//! it.
//!
//! The trade-off of octocrab's types is whole-model decode: the struct names
//! far more of the payload than a handler reads, and an incompatible change
//! to any field it names (removed, renamed, retyped, or made null) fails the
//! delivery with 500, where a view names the fields its handler reads and
//! fails only on those; a field GitHub adds fails neither. A test fixture for
//! a struct is a complete payload, since the structs decode no fragment.
//! Many of the structs leave their main object untyped, as
//! `serde_json::Value` (`check_run`, `check_suite`, `workflow_run`,
//! `workflow_job`, `release`, `deployment`, `discussion`, `label`,
//! `milestone` and `team` among them). The per-kind payload structs mostly
//! omit the top-level `installation`, `sender`, `repository` and
//! `organization` objects, which its `WebhookEvent` carries and
//! [`EventMeta`] summarizes.
//!
// `WebhookReceiver::receive` exists only under `http-body`, and the front page
// names it under every feature set, so its link definition is chosen by cfg:
// an intra-doc path when the method is compiled in, so a renamed or removed
// method still fails `cargo doc`, and its docs.rs URL when it is not, so the
// link resolves under `--no-default-features` rather than being dropped.
#![cfg_attr(
    feature = "http-body",
    doc = "[`WebhookReceiver::receive`]: WebhookReceiver::receive"
)]
#![cfg_attr(
    not(feature = "http-body"),
    doc = "[`WebhookReceiver::receive`]: https://docs.rs/octoevents/latest/octoevents/struct.WebhookReceiver.html#method.receive"
)]
// `doc_cfg` propagates each `#[cfg]` into the rendered docs on its own,
// including from a gated module to the items inside it, so gated items carry
// no separate `doc(cfg(...))`.
#![cfg_attr(docsrs, feature(doc_cfg))]

mod dispatch;
mod envelope;
mod events;
mod handler;
pub mod header;
mod matcher;
mod meta;
#[cfg(feature = "octocrab")]
mod octocrab;
mod payload;
mod receiver;
mod runtime;
mod signature;
#[cfg(test)]
mod test_support;
mod trace;

pub use dispatch::{DispatchError, Dispatcher, DispatcherBuilder, Match, Outcome, Tier};
pub use envelope::{BodyError, DecodeError, Envelope, ReceiveError};
pub use events::{Action, EventKind};
pub use handler::Handler;
pub use matcher::{AnyAction, EventMatcher, IntoMatcher};
pub use meta::{AccountMeta, EventMeta, RepositoryMeta, TargetType};
/// Derives [`Payload`] for a serde type, declaring its kind:
/// `#[derive(Payload)] #[payload(EventKind::..)]`. See the trait.
#[cfg(feature = "derive")]
pub use octoevents_derive::Payload;
pub use payload::{Event, FromEnvelope, Payload};
pub use receiver::{WebhookReceiver, WebhookReceiverBuilder};
pub use runtime::{BoxError, MaybeSend, MaybeSync};
pub use signature::{Signature, SignatureError, Verifier, WebhookSecret, WebhookSecretError};

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
// the `tower` feature provides, its views declare their kind with
// `#[derive(Payload)]`, which the `derive` feature provides, and its octocrab
// block names octocrab's types, which the `octocrab` feature provides, so the
// blocks are checked under all three. Blocks that continue a program rather
// than stand alone (the closure and observer fragments, and the tests) are
// marked `ignore` in the README itself; `tests/readme_testing.rs` compiles
// the tests.
#[cfg(all(doctest, feature = "tower", feature = "derive", feature = "octocrab"))]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
