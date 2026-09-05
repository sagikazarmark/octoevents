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
//! and [`Action`]: webhook handlers in its raw tier (`always_raw`), meta
//! handlers in its `always` and `fallback` tiers, payload handlers by the kind
//! their payload type declares (`on_payload`, or `on_payload_action` for some
//! of its actions), and, with the `octocrab` feature, event handlers by
//! matcher through `on`. It converts each handler's error into one
//! application error via `From`, and reports a failure as a
//! [`DispatchError`] wrapping that error with the [`Tier`] it came from, the
//! delivery's ID, kind and action, and the source location that registered
//! the failing handler. Its `dispatch` reports an [`Outcome`]: whether the
//! delivery matched, and if not, whether its kind was known to the route
//! table, beside the handlers' result. As a `WebhookHandler` it keeps only
//! the result, so the receiver sees an unmatched delivery as a success unless
//! a fallback failed it.
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
//! Closures implement every flavour too. Annotate the parameters the body
//! uses (`|envelope: Envelope|`, `|meta: EventMeta, pr: PullRequestNumber|`):
//! registration is bound on the handler trait rather than on `Fn`, so rustc
//! does not read their types off the call, though a parameter the body
//! ignores may stay a bare `_`. Always state the error type
//! (`Ok::<_, E>(())`): a bare `Ok(())` fails with E0282 on the receiver
//! path, where nothing constrains it, and with E0283 on the dispatcher path,
//! where every error type the application error has a `From` for would fit.
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
//! returning. With a dispatcher, register that work with `always_raw`: the
//! raw tier receives the verified envelope, bytes included, and runs before
//! every other tier, so the envelope is stored before any typed handler sees
//! it, and a delivery whose envelope could not be stored is not routed. The
//! `dispatcher` example shows the pattern.
//!
//! The receiver answers a failed delivery with a bare 500 and discards the
//! handler's error: the response is GitHub's delivery record, not a log. To
//! see why a delivery failed, wrap the handler; `WebhookReceiverBuilder::build`
//! shows an `Observe<H>` wrapper that logs the error and its source chain,
//! which for a dispatcher is the [`DispatchError`] naming the tier, the
//! delivery, and the line that registered the failing handler.
//!
//! # Deliberately left out
//!
//! Some requests come up in every webhook library and are declined here on
//! purpose. The evidence is in the repository's
//! [`docs/research/`](https://github.com/sagikazarmark/octoevents/tree/main/docs/research),
//! a survey of GitHub-webhook receivers in other ecosystems and of dispatcher
//! designs in Rust; each item below is also recorded on the type where the
//! request would land.
//!
//! - **No SHA-1 fallback.** Only `X-Hub-Signature-256` is verified. GitHub
//!   sends the SHA-1 `X-Hub-Signature` beside it, and go-github falls back to
//!   that header when the SHA-256 one is absent; here a request carrying only
//!   the SHA-1 header is refused as unsigned, with
//!   [`VerifyError::MissingSignature`]. The stronger header is always present
//!   to verify, so the fallback would only let a sender choose the weaker
//!   algorithm. See [`Verifier`].
//! - **No form-urlencoded body.** The webhook must deliver `application/json`;
//!   anything else is [`ReceiveError::UnsupportedContentType`]. go-github
//!   also accepts `application/x-www-form-urlencoded`, JSON under a `payload`
//!   form parameter with the signature over the form body. Here
//!   [`Envelope::raw`] is both the signed input and the payload, and a form
//!   body would make it one but not the other. See [`Envelope::from_signed`].
//! - **Kind from the header, not the payload's shape.** [`EventMeta::kind`] is
//!   parsed from `X-GitHub-Event`, and a name this crate does not know is
//!   [`EventKind::Unknown`], never a failure. Inferring the kind from the
//!   payload's shape (octoapp's `#[serde(untagged)]` event enum) mis-resolves
//!   when kinds share a shape and has no answer for a kind it was not built
//!   with; every other library surveyed reads the header. See [`EventKind`].
//! - **Consumer-defined views, not one blessed struct per kind.** A
//!   [`Payload`] is any serde type that declares its kind with
//!   [`impl_payload!`], so a handler names the fields it reads and nothing
//!   else. go-playground/webhooks ships one hand-written struct per kind, and
//!   its issue tracker is a record of fields those structs lack and per-action
//!   variance they cannot follow. octocrab's per-kind structs are available
//!   as payloads behind the `octocrab` feature for handlers that want the
//!   whole document. See [`Payload`].
//! - **No priorities or propagation control.** Handlers run in tier order,
//!   then registration order, and each can only continue or fail: none can be
//!   moved ahead of an earlier registration, stop the chain, or pass a
//!   delivery on as "not mine". Symfony's numeric priorities and
//!   `stopPropagation`, and dptree's `ControlFlow::Continue`, were surveyed
//!   and left out: the tiers cover what a webhook receiver needs, and matching
//!   decided by handlers at run time would make the route table unable to say
//!   what it routes. See [`Dispatcher`].
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

pub use dispatch::{DispatchError, Dispatcher, DispatcherBuilder, Match, Outcome, Tier};
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
