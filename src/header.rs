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

use http::{HeaderMap, HeaderName};

use crate::{ReceiveError, Signature, SignatureError};

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

// How the receiving path reads the headers named above. Crate-private: the
// module's public face is the names, and the two paths that read them,
// `Envelope::from_signed` and the receiver, read them through these so they
// agree on what a header's value is and which failure its absence earns.

/// The signature to verify, parsed from `headers`, or the header failure
/// [`Envelope::from_signed`](crate::Envelope::from_signed) reports for it:
/// [`SignatureError::Missing`] when the header is absent, decided here and
/// nowhere else; [`SignatureError::Malformed`] when its bytes are not a
/// signature, decided by `Signature::try_from` and nowhere else.
/// [`Verifier::verify`](crate::Verifier::verify) takes the parsed value and
/// can only mismatch.
///
/// The value's bytes are parsed, not its `str`: an `http::HeaderValue` need
/// not be visible ASCII, and one that is not is present and not a signature
/// (400), never mistaken for an absent header (401), so a corrupting hop and
/// a missing header stay distinguishable.
///
/// Decidable from the headers alone, so the receiver uses it to refuse an
/// unsigned or malformed request before reading the body, and `from_signed`
/// uses it so both paths agree on which failure a header earns.
pub(crate) fn signature(headers: &HeaderMap) -> Result<Signature, SignatureError> {
    headers
        .get(&SIGNATURE)
        .ok_or(SignatureError::Missing)
        .and_then(Signature::try_from)
}

/// The first value of `name` in `headers` as a string, or `None` when the
/// header is absent or its value is not visible ASCII: a value the crate
/// cannot read is a value it does not have. The receiver reads the span's
/// `delivery_id` and `event` through it too, so the two read a header alike.
pub(crate) fn read<'a>(headers: &'a HeaderMap, name: &HeaderName) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// A header the receiving path cannot do without: [`read`], with an empty
/// value refused as [`ReceiveError::MissingHeader`] like an absent one.
pub(crate) fn required(headers: &HeaderMap, name: HeaderName) -> Result<&str, ReceiveError> {
    read(headers, &name)
        .filter(|value| !value.is_empty())
        .ok_or(ReceiveError::MissingHeader { name })
}

/// Whether `headers` declare the body `application/json`, by the media type
/// alone: a `charset` or any other parameter is ignored, and the comparison
/// is case-insensitive. An absent `Content-Type` is not JSON.
pub(crate) fn is_json(headers: &HeaderMap) -> bool {
    read(headers, &CONTENT_TYPE).is_some_and(|value| {
        value
            .split(';')
            .next()
            .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("application/json"))
    })
}
