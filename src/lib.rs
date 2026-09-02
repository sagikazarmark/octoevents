//! Receive and verify GitHub webhook events.
//!
//! `octoevents` is the receiving edge of a GitHub App: it turns an untrusted
//! HTTP request into a verified, routable [`Envelope`] and stops there. The
//! core is sans-I/O and wasm-safe; one receiver over `http` types serves
//! Axum, Cloudflare Workers, and anything else that can hand over a request.
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
//! `tower_service::Service` impl, and [`Dispatcher`] routes verified
//! envelopes by [`EventKind`] and [`Action`]. Each of those types carries its
//! own example.
//!
//! # Delivery semantics
//!
//! The crate deliberately provides no replay protection: GitHub signs no
//! timestamp, so consumers must treat [`Envelope::delivery_id`] as an
//! idempotency key.
//!
//! GitHub does not automatically retry failed webhook deliveries. Keep
//! handlers below GitHub's timeout (10 seconds on github.com and 30 seconds
//! on GitHub Enterprise Server) by persisting or forwarding an event before
//! returning.
#![cfg_attr(docsrs, feature(doc_cfg))]

mod dispatch;
mod envelope;
mod events;
mod respond;
mod runtime;
mod secret;
#[cfg(feature = "http")]
mod service;
#[cfg(feature = "octocrab")]
mod typed;
mod verify;

pub use dispatch::{Dispatcher, DispatcherBuilder, WebhookHandler};
pub use envelope::{Common, Envelope, HeaderView, ReceiveError, RepositoryRef, TargetType};
pub use events::{Action, EventKind};
pub use respond::ResponseStatus;
pub use runtime::{MaybeSend, MaybeSync};
pub use secret::Secret;
#[cfg(feature = "http")]
#[cfg_attr(docsrs, doc(cfg(feature = "http")))]
pub use service::{WebhookReceiver, WebhookReceiverBuilder};
pub use verify::{Verifier, VerifyError};

/// GitHub's maximum delivered payload size: 25 MiB.
pub const DEFAULT_BODY_LIMIT: usize = 25 * 1024 * 1024;
