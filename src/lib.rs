//! Receive and verify GitHub webhook events.
//!
//! `octoevents` is the receiving edge of a GitHub App: it turns an untrusted
//! HTTP request into a verified [`Envelope`] and hands it to your handlers.
//! The core is sans-I/O and wasm-safe; one receiver over `http` types serves
//! Axum, Cloudflare Workers, and anything else that can hand over a request.
//!
//! A complete receiver that labels every opened issue and audits every
//! delivery, mounted on Axum with the `tower` feature:
//!
//! ```no_run
//! use octoevents::{
//!     Action, DecodeError, DispatchError, Dispatcher, Envelope, EventKind, EventMeta, Secret,
//!     Verifier,
//! };
//! # #[cfg(feature = "http")]
//! use octoevents::WebhookReceiverBuilder;
//!
//! /// The one error every handler returns. The dispatcher decodes payloads on
//! /// the handlers' behalf and reports a payload that does not fit through it.
//! #[derive(Debug, thiserror::Error)]
//! enum AppError {
//!     #[error(transparent)]
//!     Decode(#[from] DecodeError),
//! }
//!
//! /// The fields this bot reads off an `issues` payload, and nothing else.
//! #[derive(serde::Deserialize)]
//! struct IssueOpened {
//!     issue: Issue,
//! }
//!
//! #[derive(serde::Deserialize)]
//! struct Issue {
//!     number: u64,
//!     title: String,
//! }
//!
//! octoevents::impl_payload!(IssueOpened => EventKind::Issues);
//!
//! /// Runs for `issues.opened`, with the payload decoded as `IssueOpened`.
//! async fn label(issue: IssueOpened) -> Result<(), AppError> {
//!     println!("label #{} '{}'", issue.issue.number, issue.issue.title);
//!     Ok(())
//! }
//!
//! /// Runs for every verified delivery, bytes included.
//! async fn audit(envelope: Envelope) -> Result<(), AppError> {
//!     println!("{} {} ({} bytes)", envelope.meta.delivery_id, envelope.meta.kind, envelope.raw.len());
//!     Ok(())
//! }
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let secret = std::env::var("GITHUB_WEBHOOK_SECRET")?;
//!
//!     let dispatcher = Dispatcher::<AppError>::builder()
//!         .always(audit)
//!         .on_payload_action([Action::Opened], label)
//!         .build();
//!
//! #   #[cfg(feature = "http")] {
//!     let webhook = WebhookReceiverBuilder::new(Verifier::new(Secret::new(secret)))
//!         .on_error(|_: &EventMeta, error: &DispatchError<AppError>| {
//!             eprintln!("{error}: {}", error.source);
//!         })
//!         .build(dispatcher);
//!
//! #   #[cfg(feature = "tower")] {
//!     let app = axum::Router::new().route("/webhook", axum::routing::post_service(webhook));
//!     let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
//!     axum::serve(listener, app).await?;
//! #   }
//! #   }
//!     Ok(())
//! }
//! ```
//!
//! The receiver verifies `X-Hub-Signature-256` against the exact body bytes
//! with the secret (refusing an unsigned request before it reads the body)
//! and answers GitHub with a bare status: 204 when the handler succeeded,
//! 500 when it failed, 401, 400 or 413 for a request that never reached one.
//! The dispatcher routes each verified envelope by kind and action:
//! `audit` runs for every delivery, `label` for `issues.opened` only, with
//! the payload decoded as the view it asked for. The `on_error` observer is
//! where a failure's cause becomes visible: it prints where (the tier, the
//! failing handler's name and the line that registered it) and why (the
//! handler's own error). Without it a failed delivery is a bare 500 and, with
//! the `tracing` feature, one ERROR event naming the delivery; see
//! [Tracing](#tracing).
//!
//! Always pass the exact request bytes. Parsing, re-encoding, or normalizing
//! the body before verification invalidates GitHub's signature.
//!
//! # Handlers
//!
//! A handler is an `async fn` that takes one input and returns
//! `Result<(), E>`; the receiver and the dispatcher accept the function
//! itself, as the dispatcher above does with `audit` and `label`. One trait,
//! [`Handler<I>`](Handler), and the input type `I` says what the handler
//! receives and what is decoded for it:
//!
//! - [`Envelope`]: the [`EventMeta`] and the exact payload bytes, nothing
//!   decoded. What the receiver and the `always` and `fallback` tiers take;
//!   `audit` is one. `async fn audit(envelope: Envelope)`
//! - [`EventMeta`]: the meta alone, for a handler routed by kind and action
//!   that reads no payload. `async fn revoke(meta: EventMeta)`
//! - A [`Payload`] view `P`: the payload decoded as `P`, kind checked; the
//!   kind comes from the type, so `on_payload_action` above needed none and
//!   could not be given the wrong one. `label` is one.
//!   `async fn label(issue: IssueOpened)`
//! - [`Event<P>`](Event): the meta beside the payload, in one parameter. Meta
//!   and payload together is `Event<P>`, not two parameters; destructure it or
//!   read `event.meta` and `event.payload`.
//!   `async fn notify(Event { meta, payload }: Event<IssueOpened>)`
//!
//! A view over fields several kinds share (the sender, say) implements
//! [`FromEnvelope`] itself with the kind-free [`Envelope::decode`] and is
//! registered under those kinds with `on`. With the `octocrab` feature,
//! octocrab's `WebhookEvent` is an input too, on its own or inside `Event`,
//! and its per-kind payload structs are payloads.
//!
//! A handler with dependencies is a struct implementing the trait, the
//! dependencies its fields borrowed through `&self`:
//!
//! ```
//! use octoevents::{Event, EventKind, Handler};
//!
//! #[derive(serde::Deserialize)]
//! struct IssueOpened { issue: Issue }
//! #[derive(serde::Deserialize)]
//! struct Issue { number: u64 }
//! octoevents::impl_payload!(IssueOpened => EventKind::Issues);
//!
//! struct Labeler {
//!     label: String, // stands in for a GitHub API client
//! }
//!
//! impl Handler<Event<IssueOpened>> for Labeler {
//!     type Error = std::io::Error;
//!
//!     async fn handle(&self, Event { meta, payload }: Event<IssueOpened>) -> Result<(), Self::Error> {
//!         println!("{}: label #{} {}", meta.delivery_id, payload.issue.number, self.label);
//!         Ok(())
//!     }
//! }
//! ```
//!
//! A struct keeps its own error type; the dispatcher converts it into the
//! application error through `From`. `Arc<H>` is a handler when `H` is, for
//! sharing one struct between a route and a test that reads its state.
//! Closures work too, with the annotations [`Handler`] describes.
//!
//! # Routing
//!
//! A [`Dispatcher`] is itself a handler over the envelope that runs three
//! tiers in order: `always`, for every delivery, receiving the envelope; the
//! routed handlers registered for the delivery's kind and action, then for
//! the kind, by the payload type (`on_payload`, `on_payload_action`) or by an
//! [`EventMatcher`] over any `FromEnvelope` input (`on`); and `fallback`,
//! only when nothing routed matched, receiving the envelope. A routed handler
//! decodes its input only when its route matched; `always` and `fallback`
//! decode nothing. Unmatched deliveries succeed unless a fallback fails them.
//!
//! A failure is a [`DispatchError`]: the application error wrapped with the
//! [`Tier`], the delivery's ID, kind and action, the failing handler's name,
//! and the source location of the registration that put it there.
//! `dispatch` also reports an [`Outcome`], matched or unmatched with the kind
//! known or unknown to the route table, for a handler wrapping the dispatcher
//! to act on.
//!
//! # Testing without GitHub
//!
//! A handler is tested through [`Dispatcher::dispatch`] with an envelope
//! built by hand, [`EventMeta::new`] for the delivery and the payload bytes
//! as a literal; nothing is verified on that path, so nothing is signed. The
//! receiver is tested through [`WebhookReceiver::receive`] with a synthetic
//! request carrying the four headers under [`header`] and a signature of
//! `sha256=` plus the lowercase hex HMAC-SHA256 of the body under the secret.
//! The README shows both as `#[tokio::test]` functions.
//!
//! # One event, one handler
//!
//! A receiver for one kind and nothing else needs no dispatcher: a handler
//! over the envelope decodes its own view with [`Envelope::decode_payload`],
//! which refuses a delivery of any other kind at the kind. Without the
//! `tower` feature, the receiver mounts on Axum as a plain handler calling
//! [`WebhookReceiver::receive`]; the README shows the wiring.
//!
//! ```
//! use octoevents::{Action, DecodeError, Envelope, EventKind};
//!
//! #[derive(serde::Deserialize)]
//! struct ReleasePublished { release: Release }
//! #[derive(serde::Deserialize)]
//! struct Release { tag_name: String }
//! octoevents::impl_payload!(ReleasePublished => EventKind::Release);
//!
//! async fn announce(envelope: Envelope) -> Result<(), DecodeError> {
//!     if envelope.meta.action != Some(Action::Published) {
//!         return Ok(());
//!     }
//!     let payload = envelope.decode_payload::<ReleasePublished>()?;
//!     println!("{}: released {}", envelope.meta.delivery_id, payload.release.tag_name);
//!     Ok(())
//! }
//!
//! # #[cfg(feature = "http")] {
//! use octoevents::{Secret, Verifier, WebhookReceiverBuilder};
//!
//! let webhook = WebhookReceiverBuilder::new(Verifier::new(Secret::new("current secret")))
//!     .build(announce);
//! # let _ = webhook;
//! # }
//! ```
//!
//! The alternative is a dispatcher with one `on_payload` route, which answers
//! a delivery of any other kind with success rather than failure.
//!
//! # Without the `http` feature
//!
//! [`Envelope::from_signed`] is the sans-I/O entry point and the only path
//! that turns an untrusted request into an envelope; it authenticates before
//! it extracts. A transport with no `http::Request` builds a [`HeaderView`]
//! with [`HeaderView::from_lookup`], which asks its map for each header by
//! the names in [`header`], calls it with the body as [`Bytes`], and answers
//! with [`ResponseStatus`]. The header names are lowercase and the lookup
//! compares nothing itself, so matching the case of the map's keys is the
//! transport's concern. [`Dispatcher::dispatch`] is a plain `async fn` with
//! no runtime of its own, so a transport awaits it on whatever executor it
//! has. The docs of `from_signed` show, as code to copy, the three things
//! the receiver does that this path does not: refusing an unsigned request
//! before reading the body, bounding the body, and short-circuiting `ping`.
//! [`Envelope`] serializes with serde for forwarding, bytes in base64; its
//! docs show the document.
//!
//! # Delivery semantics
//!
//! The crate provides no replay protection: GitHub signs no timestamp, so
//! treat [`EventMeta::delivery_id`] as an idempotency key.
//!
//! GitHub does not retry a failed delivery on its own, and it abandons a
//! request after 10 seconds (30 on GitHub Enterprise Server). Persist or
//! forward an envelope before returning and process it afterwards. With a
//! dispatcher, that policy lives in a handler wrapping `dispatch`: it
//! stores the envelope, bytes included, before anything is routed; answers a
//! redelivery of a stored delivery ID with success without routing it; and
//! reads the [`Outcome`] to dead-letter a kind the route table does not
//! know. A tier can continue or fail but never skip, which is why that
//! wrapper, and not `always`, is the place; the `dispatcher` example shows
//! it. A forwarder that never needs to skip fits `always`.
//!
//! The receiver answers a failed delivery with a bare 500 and never reads the
//! error: the response is GitHub's delivery record, not a log. The observer
//! registered with `WebhookReceiverBuilder::on_error` receives the
//! [`EventMeta`] and the handler's error before the 500 is answered, with no
//! bound on the error type. It runs only when a handler ran and failed.
//!
//! # Tracing
//!
//! With the `tracing` feature, a delivery runs in three spans:
//!
//! - `octoevents.receive`, at INFO, around [`WebhookReceiver::receive`]. It
//!   records `delivery_id` and `event` from the headers as soon as they are
//!   read, before verification, and on the way out `outcome` and `status`,
//!   the HTTP code answered. `outcome` is one of `ok`, `bad_request`,
//!   `unauthorized`, `payload_too_large` and `handler_error`.
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
//! `installation_id` and `status` as integers, and `outcome` as a string
//! label with its own vocabulary per span. The one value two vocabularies
//! share, `handler_error`, partitions differently: on the receive span it is
//! every delivery a handler failed, since any handler error is a 500; on the
//! dispatch span it is a matched delivery a handler failed, and an unmatched
//! delivery failed by its `always` or `fallback` tier is `unmatched_error`.
//! A receive `handler_error` is a dispatch `handler_error` or
//! `unmatched_error`.
//!
//! A failed delivery also emits one event at ERROR, `handler failed`, with
//! `delivery_id`, `event`, `status`, and `action` and `installation_id` when
//! the delivery has them, so a subscriber filtering at ERROR sees every
//! failed delivery without an observer. A successful delivery, a request
//! refused before any handler ran and a short-circuited `ping` emit no event.
//! The event carries no text of the error, since the receiver places no
//! bound on the handler's error type; the text and its source chain appear
//! only through the opt-in `trace_error` observer, which exists with the
//! feature and is registered with `WebhookReceiverBuilder::on_error`. It
//! emits a second ERROR event, `handler error`, with `delivery_id` and the
//! error as an `error` field the subscriber renders with its sources.
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
//! bump is a breaking change for handlers over them. The core (envelope,
//! verification, receiver, the handler trait and its inputs, and the whole
//! dispatcher) does not depend on it.
// `doc_cfg` propagates each `#[cfg]` into the rendered docs on its own,
// including from a gated module to the items inside it, so gated items carry
// no separate `doc(cfg(...))`.
#![cfg_attr(docsrs, feature(doc_cfg))]

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
mod verify;

pub use dispatch::{DispatchError, Dispatcher, DispatcherBuilder, Match, Outcome, Tier};
pub use envelope::{
    DecodeError, Envelope, EventMeta, HeaderView, ReceiveError, RepositoryRef, TargetType,
};
pub use events::{Action, EventKind};
pub use handler::Handler;
pub use matcher::EventMatcher;
pub use payload::{Event, FromEnvelope, Payload};
pub use respond::ResponseStatus;
pub use runtime::{MaybeSend, MaybeSync};
pub use secret::Secret;
#[cfg(feature = "http")]
pub use service::{WebhookReceiver, WebhookReceiverBuilder};
#[cfg(feature = "tracing")]
pub use trace::trace_error;
pub use verify::{Verifier, VerifyError};

/// The byte buffer type of [`Envelope::raw`] and of the body
/// [`Envelope::from_signed`] takes, re-exported from the `bytes` crate.
///
/// A transport that never touches `bytes` otherwise builds the body from
/// here (`Bytes::from(String)`, `Bytes::from(Vec<u8>)`, or
/// `Bytes::from_static`) without adding the dependency for one type.
pub use bytes::Bytes;

/// GitHub's maximum delivered payload size: 25 MiB.
pub const DEFAULT_BODY_LIMIT: usize = 25 * 1024 * 1024;

// The README's Rust blocks compile as doctests, so its programs cannot drift
// from the API. Its first program mounts the receiver with `post_service`,
// which the `tower` feature provides, so the blocks are checked under that
// feature; blocks that continue the program rather than stand alone are
// marked `ignore` in the README itself.
#[cfg(all(doctest, feature = "tower"))]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
