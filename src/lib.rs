//! Receive and verify GitHub webhook events.
//!
//! `octoevents` is the receiving edge of a GitHub App: it turns an untrusted
//! HTTP request into a verified, routable [`Envelope`] and hands it to your
//! handlers. The core is sans-I/O and wasm-safe; one receiver over `http`
//! types serves Axum, Cloudflare Workers, and anything else that can hand
//! over a request.
//!
//! # Quick start
//!
//! Always pass the exact request bytes. Parsing, re-encoding, or normalizing
//! the body before verification invalidates GitHub's signature.
//!
//! ```
//! use bytes::Bytes;
//! use octoevents::{Envelope, HeaderView, Secret, Verifier};
//!
//! let body = Bytes::from_static(br#"{"action":"opened"}"#);
//! let headers = HeaderView::new()
//!     .signature("sha256=...")
//!     .delivery_id("72d3162e-cc78-11e3-81ab-4c9367dc0958")
//!     .event_name("pull_request")
//!     .content_type("application/json");
//! let verifier = Verifier::new(Secret::new("current secret"));
//!
//! // The placeholder signature above fails, as it should.
//! assert!(Envelope::from_signed(&verifier, &headers, body).is_err());
//! ```
//!
//! [`Envelope::from_signed`] is the only path in this crate that turns an
//! untrusted request into an envelope, and it authenticates before it
//! extracts. On the `http` feature (default), `WebhookReceiver` applies the
//! same construction to a whole `http::Request` and answers with the response
//! contract GitHub expects; the `tower` feature adds a
//! `tower_service::Service` impl.
//!
//! # Handlers
//!
//! Handlers are structs whose fields are their dependencies, with a plain
//! `async fn handle(&self, ..)` and their own error type. Four flavours
//! differ by what they receive:
//!
//! - A [`WebhookHandler`] receives the verified [`Envelope`]: metadata plus
//!   the raw payload bytes. This is what the receiver accepts.
//! - A [`MetaHandler`] receives only the [`EventMeta`]: no bytes and no
//!   decode, so it runs for any verified delivery. For audit, metrics, and
//!   deduplication.
//! - A [`PayloadHandler`] receives the `EventMeta` and one kind's decoded
//!   [`Payload`]; its kind is declared by the payload type. Implement
//!   `Payload` for your own serde view with [`impl_payload!`], or use
//!   octocrab's per-kind payload structs with the `octocrab` feature.
//! - An `EventHandler` (`octocrab` feature) receives the `EventMeta` and
//!   octocrab's decoded `WebhookEvent`, for logic that spans kinds.
//!
//! Every other flavour reaches the receiver through its
//! `into_webhook_handler()`. A [`Dispatcher`] routes handlers by [`EventKind`]
//! and [`Action`]: meta handlers in its `always` and `fallback` tiers, payload
//! handlers by the kind their payload type declares, and, with the `octocrab`
//! feature, event handlers by matcher through `on`. It converts each
//! handler's error into one application error via `From`.
//!
//! ```
//! use octoevents::{EventKind, EventMeta, PayloadHandler};
//!
//! #[derive(serde::Deserialize)]
//! struct PullRequestNumber { number: u64 }
//! octoevents::impl_payload!(PullRequestNumber => EventKind::PullRequest);
//!
//! struct Labeler { /* GitHub API client */ }
//!
//! impl PayloadHandler<PullRequestNumber> for Labeler {
//!     type Error = std::io::Error;
//!
//!     async fn handle(&self, meta: EventMeta, pr: PullRequestNumber) -> Result<(), Self::Error> {
//!         println!("{}: label PR #{} for installation {:?}", meta.delivery_id, pr.number, meta.installation_id);
//!         Ok(())
//!     }
//! }
//!
//! # #[cfg(feature = "http")] {
//! use octoevents::{Secret, Verifier, WebhookReceiverBuilder};
//!
//! let receiver = WebhookReceiverBuilder::new(Verifier::new(Secret::new("current secret")))
//!     .build(Labeler {}.into_webhook_handler());
//! # let _ = receiver;
//! # }
//! ```
//!
//! Closures implement every flavour too. Because registration is bound on a
//! trait, annotate the closure's parameter types (`|envelope: Envelope|`),
//! and state the error type where nothing else fixes it (`Ok::<_, E>(())`).
//!
//! # Delivery semantics
//!
//! The crate deliberately provides no replay protection: GitHub signs no
//! timestamp, so consumers must treat [`EventMeta::delivery_id`] as an
//! idempotency key.
//!
//! GitHub does not automatically retry failed webhook deliveries. Keep
//! handlers below GitHub's timeout (10 seconds on github.com and 30 seconds
//! on GitHub Enterprise Server) by persisting or forwarding an event before
//! returning. With a dispatcher, do that in a [`WebhookHandler`] that stores
//! the envelope and then calls the dispatcher's `handle`: no dispatcher
//! method accepts a webhook handler, so raw-bytes work always runs before any
//! typed handler. The `dispatcher` example shows the pattern.
//!
//! # Feature caveats
//!
//! Enabling the `octocrab` feature makes octocrab's pre-1.0 version part of
//! this crate's public API: `EventHandler`, `Dispatcher::on`, and the octocrab
//! `Payload` impls expose octocrab's types, so an octocrab major bump is a
//! breaking change for them. The core (envelope, verification, receiver,
//! `WebhookHandler`, `MetaHandler`, `PayloadHandler`, and the dispatcher's
//! other methods) does not depend on it.
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

pub use dispatch::{Dispatcher, DispatcherBuilder};
pub use envelope::{
    DecodeError, Envelope, EventMeta, HeaderView, ReceiveError, RepositoryRef, TargetType,
};
pub use events::{Action, EventKind};
#[cfg(feature = "octocrab")]
pub use handler::{EventAdapter, EventHandler};
pub use handler::{
    HandleError, MetaAdapter, MetaHandler, PayloadAdapter, PayloadHandler, WebhookHandler,
};
pub use matcher::EventMatcher;
pub use payload::Payload;
pub use respond::ResponseStatus;
pub use runtime::{MaybeSend, MaybeSync};
pub use secret::Secret;
#[cfg(feature = "http")]
pub use service::{WebhookReceiver, WebhookReceiverBuilder};
pub use verify::{Verifier, VerifyError};

/// GitHub's maximum delivered payload size: 25 MiB.
pub const DEFAULT_BODY_LIMIT: usize = 25 * 1024 * 1024;
