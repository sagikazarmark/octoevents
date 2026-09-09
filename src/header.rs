//! The names of the request headers this crate reads, as `http::HeaderName`s.
//!
//! [`Envelope::from_signed`] reads them off the `http::HeaderMap` it is
//! handed; the constants are for the two places a consumer names a header
//! itself. A transport that streams the body checks for the signature before
//! buffering, with the two lines `from_signed`'s docs show:
//!
//! ```
//! use octoevents::{Signature, SignatureError, header};
//!
//! # let mut headers = http::HeaderMap::new();
//! # headers.insert(header::SIGNATURE, "sha256=".parse().unwrap());
//! let signature = headers
//!     .get(&header::SIGNATURE)
//!     .ok_or(SignatureError::Missing)
//!     .and_then(Signature::try_from);
//! # assert_eq!(signature.unwrap_err(), SignatureError::Malformed);
//! ```
//!
//! And a test forges a delivery with `http::Request::builder()`, the idiom
//! every hyper test already uses; the signature [`Verifier::sign`] gives goes
//! on as it is:
//!
//! ```
//! use octoevents::{Verifier, WebhookSecret, header};
//!
//! let verifier = Verifier::new(WebhookSecret::new("test-secret"));
//! let body = r#"{"action":"opened"}"#;
//! let request = http::Request::builder()
//!     .header(header::CONTENT_TYPE, "application/json")
//!     .header(header::DELIVERY_ID, "72d3162e-cc78-11e3-81ab-4c9367dc0958")
//!     .header(header::EVENT_NAME, "issues")
//!     .header(header::SIGNATURE, verifier.sign(body.as_bytes()))
//!     .body(body.to_string())
//!     .unwrap();
//! # let _ = request;
//! ```
//!
//! [`Envelope::from_signed`]: crate::Envelope::from_signed
//! [`Verifier::sign`]: crate::Verifier::sign

use http::HeaderName;

/// `X-Hub-Signature-256`: the HMAC-SHA256 of the body under the webhook
/// secret, which [`Verifier`](crate::Verifier) checks.
///
/// GitHub sends the SHA-1 `X-Hub-Signature` beside it; this crate never
/// reads that one, so it has no constant here.
pub const SIGNATURE: HeaderName = HeaderName::from_static("x-hub-signature-256");

/// `X-GitHub-Delivery`: the GUID of one delivery attempt, which becomes
/// [`EventMeta::delivery_id`](crate::EventMeta::delivery_id).
pub const DELIVERY_ID: HeaderName = HeaderName::from_static("x-github-delivery");

/// `X-GitHub-Event`: the event name, which parses into
/// [`EventMeta::kind`](crate::EventMeta::kind).
pub const EVENT_NAME: HeaderName = HeaderName::from_static("x-github-event");

/// `Content-Type`: must be `application/json` for the body to be accepted.
///
/// `http`'s own constant, re-exported rather than defined again, so the
/// headers a request needs are named from one module and the two spellings
/// are one `HeaderName`.
#[doc(inline)]
pub use http::header::CONTENT_TYPE;

/// `X-GitHub-Hook-Installation-Target-Type`: the resource the webhook is
/// installed on, which parses into
/// [`EventMeta::target_type`](crate::EventMeta::target_type).
pub const TARGET_TYPE: HeaderName =
    HeaderName::from_static("x-github-hook-installation-target-type");

/// `X-GitHub-Hook-Installation-Target-ID`: the ID of that resource, which
/// parses into [`EventMeta::target_id`](crate::EventMeta::target_id).
pub const TARGET_ID: HeaderName = HeaderName::from_static("x-github-hook-installation-target-id");
