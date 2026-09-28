use std::{borrow::Cow, fmt};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use bytes::Bytes;
use http::{HeaderName, StatusCode};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

use crate::{BoxError, EventKind, SignatureError, TargetType, WebhookMeta};

/// The unit of receipt: the header meta and the exact payload bytes.
///
/// An envelope is what arrived, and nothing read out of it: `meta` is the
/// [`WebhookMeta`] read from the request's headers (the delivery ID, the kind
/// and the target), and `raw_payload` is the payload as it arrived,
/// undecoded. Building one reads nothing of the payload and cannot fail, so a
/// transport that verifies and forwards pays for the verification alone.
///
/// What the payload says is decoded later, and only where it is asked for.
/// The [`Dispatcher`](crate::Dispatcher) decodes the delivery's
/// [`EventMeta`](crate::EventMeta) once, for the action it routes by, and hands it to every
/// input it builds: a handler over a decoded payload receives it as
/// [`Event<P>`](crate::Event), so the installation ID a GitHub App mints its
/// token for travels beside the view. Code outside the dispatcher decodes
/// it with [`EventMeta::decode`](crate::EventMeta::decode).
///
/// An envelope is data, and makes no claim that it was authenticated. What
/// makes a received one trustworthy is the path it came from:
/// [`authenticate`](crate::authenticate), which verifies an untrusted request
/// before it builds the envelope, and the receiver, which is built on it.
/// Otherwise an envelope comes from [`Envelope::new`] or a struct literal,
/// which pair a [`WebhookMeta`] with bytes and authenticate nothing, or from
/// the serde `Deserialize` impl for one a trusted internal transport
/// forwarded (see the wire format below). The struct is plain data, so the
/// three build the same value:
///
/// ```
/// use octoevents::{Bytes, Envelope, EventKind, WebhookMeta};
///
/// let meta = WebhookMeta::new("delivery-1", EventKind::Issues);
/// let literal = Envelope {
///     meta: meta.clone(),
///     raw_payload: Bytes::from_static(br#"{"action":"opened"}"#),
/// };
///
/// assert_eq!(literal, Envelope::new(meta, br#"{"action":"opened"}"#));
/// ```
///
/// # Wire format
///
/// A serialized envelope is one flat JSON object: the header meta sits at
/// the top level beside `raw_payload`, with no `meta` nesting, so a consumer
/// in another language reads it without knowing the Rust-side split. This is
/// the envelope of a `pull_request` delivery with every field present:
///
/// ```
/// use octoevents::{Bytes, Envelope, EventKind, TargetType};
///
/// let document = r#"{
///   "delivery_id": "72d3162e-cc78-11e3-81ab-4c9367dc0958",
///   "kind": "pull_request",
///   "target_type": "integration",
///   "target_id": 12345,
///   "raw_payload": "eyJhY3Rpb24iOiJvcGVuZWQifQ=="
/// }"#;
///
/// let envelope: Envelope = serde_json::from_str(document).unwrap();
/// assert_eq!(envelope.meta.kind, EventKind::PullRequest);
/// assert_eq!(envelope.meta.target_type, Some(TargetType::Integration));
/// assert_eq!(envelope.raw_payload, Bytes::from_static(br#"{"action":"opened"}"#));
///
/// // Serializing produces the same document back.
/// let expected: serde_json::Value = serde_json::from_str(document).unwrap();
/// assert_eq!(serde_json::to_value(&envelope).unwrap(), expected);
/// ```
///
/// - `raw_payload` is the exact payload bytes in standard base64 with
///   padding (RFC 4648 section 4), a string and not a nested object, so the
///   payload survives the hop without being re-encoded and still verifies
///   against GitHub's signature. The cost is size: `raw_payload` is 4/3 of
///   the payload, and the header meta beside it adds some 100 bytes.
/// - `kind` and `target_type` are GitHub's wire strings (`"pull_request"`,
///   `"integration"`); a value this version of the crate does not know reads
///   back as the `Unknown` variant carrying the string, never as an error.
///
/// On deserialize, `delivery_id`, `kind` and `raw_payload` are required;
/// `target_type` and `target_id` are optional, and a field that is absent
/// reads the same as one that is `null`. On serialize, an optional field
/// with no value is omitted rather than written as `null`. Unknown fields
/// are ignored, so a producer may annotate the document for its own
/// transport.
///
/// The payload meta is not on the wire: the consumer's dispatcher decodes it
/// from `raw_payload`, as it does for an envelope from the receiver, so a
/// forwarded envelope cannot carry meta that disagrees with its bytes. A
/// document written by 0.3, which carried `action`, `installation_id`,
/// `repository`, `organization` and `sender` beside the header fields, reads
/// back with those fields ignored and dispatches by the bytes. A 0.3
/// consumer reading a document written now routes every delivery as having
/// no action, so consumers upgrade before producers.
///
/// The wire format follows the crate's versioning, with no separate version
/// field. Adding optional fields is wire-compatible. Removing or renaming
/// existing fields, making optional fields required, or changing existing
/// fields' encodings or meanings is treated as a breaking change and
/// documented in [release notes](https://github.com/sagikazarmark/octoevents/releases).
/// Before 1.0, a minor release may include such
/// changes; producers and consumers crossing that boundary must migrate
/// together or use a transport adapter.
///
/// The header meta is read back as forwarded, and nothing is verified. The
/// producer is trusted to have authenticated what it forwards, through
/// [`authenticate`](crate::authenticate) or by its own means, which is what
/// the wire format is for: a hop between services of one deployment, not an
/// input from outside it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Envelope {
    /// The meta read from the request's headers.
    #[serde(flatten)]
    pub meta: WebhookMeta,
    /// The payload as it arrived: the exact bytes, undecoded and never
    /// re-encoded.
    ///
    /// From [`authenticate`](crate::authenticate) these are the bytes the
    /// signature was verified over; [`Envelope::new`] and the `Deserialize`
    /// impl hold whatever they were given, with no such claim. Either way
    /// they are the bytes every decode reads, [`EventMeta::decode`](crate::EventMeta::decode)
    /// included. Serialized as standard base64 so an envelope survives a JSON
    /// hop to an internal service without the payload being re-encoded; the
    /// encoded field is 4/3 of the payload's size.
    #[serde(serialize_with = "serialize_bytes")]
    pub raw_payload: Bytes,
}

// Keep this flat: serde's `flatten` buffers unknown values before ignoring
// them, imposing numeric, string and nesting limits on transport annotations.
// Listing the fields here lets derived deserialization skip them instead,
// the payload meta fields a 0.3 producer wrote among them. Keep the header
// fields in sync with WebhookMeta; the round-trip tests cover every current
// field.
#[derive(Deserialize)]
struct EnvelopeWire {
    delivery_id: String,
    kind: EventKind,
    target_type: Option<TargetType>,
    target_id: Option<u64>,
    #[serde(deserialize_with = "deserialize_bytes")]
    raw_payload: Bytes,
}

impl From<EnvelopeWire> for Envelope {
    fn from(wire: EnvelopeWire) -> Self {
        let mut meta = WebhookMeta::new(wire.delivery_id, wire.kind);
        meta.target_type = wire.target_type;
        meta.target_id = wire.target_id;
        Self {
            meta,
            raw_payload: wire.raw_payload,
        }
    }
}

impl<'de> Deserialize<'de> for Envelope {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // The wire format is an object. A derived struct also accepts a
        // positional array, so restrict the entry point to a map as before.
        struct EnvelopeVisitor;

        impl<'de> de::Visitor<'de> for EnvelopeVisitor {
            type Value = Envelope;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an envelope object")
            }

            fn visit_map<M: de::MapAccess<'de>>(self, map: M) -> Result<Self::Value, M::Error> {
                EnvelopeWire::deserialize(de::value::MapAccessDeserializer::new(map))
                    .map(Into::into)
            }
        }

        deserializer.deserialize_map(EnvelopeVisitor)
    }
}

impl Envelope {
    /// Builds an envelope from the header meta and the payload bytes,
    /// verifying nothing and reading nothing.
    ///
    /// A data constructor, the struct literal with the bytes copied: the
    /// envelope it returns makes no claim that the bytes were authenticated,
    /// and nothing of the payload is read, so any bytes build one, JSON or
    /// not. The path that authenticates a request and builds its envelope is
    /// [`authenticate`](crate::authenticate), which the receiver is built on.
    /// This is the path for everything else: a test, which dispatches an
    /// envelope built here through
    /// [`Dispatcher::dispatch`](crate::Dispatcher::dispatch) with nothing
    /// signed and no [`Verifier`](crate::Verifier) needed; and a transport
    /// that authenticated the request by its own means, which builds the
    /// envelope `authenticate` would have from the [`WebhookMeta`] it read,
    /// target included, and the bytes.
    ///
    /// The payload is anything that views as bytes, a byte-string literal
    /// included, and is copied into [`Envelope::raw_payload`]: one
    /// allocation, which a test's small payload does not notice and a
    /// transport pays once per delivery. The action, installation ID and the
    /// rest of the payload meta are the dispatcher's to decode, or
    /// [`EventMeta::decode`](crate::EventMeta::decode)'s.
    ///
    /// ```
    /// use octoevents::{Envelope, EventKind, TargetType, WebhookMeta};
    ///
    /// let mut meta = WebhookMeta::new("72d3162e-cc78-11e3-81ab-4c9367dc0958", EventKind::Issues);
    /// meta.target_type = Some(TargetType::Integration);
    /// meta.target_id = Some(12345);
    ///
    /// let envelope = Envelope::new(meta, br#"{"action":"opened","issue":{"number":7}}"#);
    ///
    /// assert_eq!(envelope.meta.kind, EventKind::Issues);
    /// assert_eq!(envelope.meta.target_id, Some(12345));
    /// ```
    #[must_use]
    pub fn new(meta: WebhookMeta, payload: impl AsRef<[u8]>) -> Self {
        Self {
            meta,
            raw_payload: Bytes::copy_from_slice(payload.as_ref()),
        }
    }

    /// Decodes the exact payload into a caller-defined view, checking nothing
    /// about the kind.
    ///
    /// `T` is any serde type and nothing ties it to the envelope's kind, so a
    /// view over fields several kinds share (the sender's `type`, say) decodes
    /// from an envelope of any kind. For a view bound to one kind, implement
    /// [`Payload`](crate::Payload) and decode with
    /// [`FromEnvelope::from_envelope`](crate::FromEnvelope::from_envelope),
    /// which refuses an envelope of another kind before decoding. This is the
    /// one decoding primitive on the envelope: a consumer's own
    /// [`FromEnvelope`](crate::FromEnvelope) impl decodes with it, and so does
    /// every serde `Payload`, after its kind check.
    ///
    /// A view may borrow from the raw payload for as long as the envelope is
    /// borrowed, for example with `&str` or `&serde_json::value::RawValue`
    /// fields. Routed inputs still use the owned decode through `FromEnvelope`.
    ///
    /// ```
    /// use octoevents::{Envelope, EventKind, WebhookMeta};
    ///
    /// /// The sender's account type, which every kind carries.
    /// #[derive(serde::Deserialize)]
    /// struct SenderType { sender: Sender }
    /// #[derive(serde::Deserialize)]
    /// struct Sender { r#type: String }
    ///
    /// let envelope = Envelope::new(
    ///     WebhookMeta::new("delivery-1", EventKind::Push),
    ///     br#"{"ref":"refs/heads/main","sender":{"id":1,"login":"octocat","type":"User"}}"#,
    /// );
    ///
    /// let view: SenderType = envelope.decode()?;
    /// assert_eq!(view.sender.r#type, "User");
    /// # Ok::<(), octoevents::DecodeError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::Json`] when the payload does not fit `T`.
    pub fn decode<'de, T: Deserialize<'de>>(&'de self) -> Result<T, DecodeError> {
        serde_json::from_slice(&self.raw_payload).map_err(DecodeError::Json)
    }
}

/// A failure while receiving a webhook.
///
/// What the receiving path reports before any handler runs, each variant
/// naming the step that refused the request: [`Signature`](Self::Signature),
/// when the signature header is absent, malformed or does not match;
/// [`UnknownTarget`](Self::UnknownTarget), when the receiver's
/// [`VerifierSource`](crate::VerifierSource) has no verifier for the request;
/// [`MissingHeader`](Self::MissingHeader), when a required delivery header is
/// absent or empty; [`UnsupportedContentType`](Self::UnsupportedContentType),
/// when the request's content type is not `application/json`;
/// [`BodyRead`](Self::BodyRead),
/// when the transport failed while the body was being read; and
/// [`BodyTooLarge`](Self::BodyTooLarge), when the body ran past the
/// configured limit. [`status`](Self::status) is the `http::StatusCode` the
/// receiver answers each with, so every failure before a handler has an
/// error value and the same value selects the status. The enum is
/// `#[non_exhaustive]`, so a `match` over it keeps a wildcard arm.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Error)]
#[non_exhaustive]
pub enum ReceiveError {
    /// The signature was absent, malformed or did not match: the request
    /// did not authenticate.
    #[error(transparent)]
    Signature(#[from] SignatureError),
    /// The receiver's [`VerifierSource`](crate::VerifierSource) has no
    /// verifier for the request: its target headers are absent, name a
    /// target the source does not know, or the source could not look it up.
    ///
    /// Answered 401, as a mismatching signature is: either way the request
    /// did not authenticate, and a client learns nothing from the status
    /// about which targets are configured. The variant, and the receive
    /// span's `error` text, tell the two apart for an operator. Produced by
    /// the receiver before verification (on `receive`, before the body is
    /// read), never by [`authenticate`](crate::authenticate), which is
    /// handed its verifier; a transport choosing the verifier itself
    /// constructs it for the same failure.
    #[error("no webhook verifier is available for the request's target")]
    UnknownTarget,
    /// A required delivery header was absent or empty.
    #[error("missing {name} header")]
    MissingHeader {
        /// The header's name, one of the constants in [`header`](crate::header),
        /// so an assertion compares by type and cannot drift from the constant
        /// by a string. `Display` renders it lowercase: `missing
        /// x-github-delivery header`.
        name: HeaderName,
    },
    /// The request's content type is not `application/json`, the content
    /// type the GitHub webhook must be configured to send.
    #[error(
        "unsupported content type; configure the GitHub webhook content type as application/json"
    )]
    UnsupportedContentType,
    /// The transport failed while the body was being read.
    ///
    /// Produced by `WebhookReceiver::receive` (`http-body` feature) when a
    /// body frame is an error rather than data or trailers: the connection
    /// dropped, the client stopped sending. Never by
    /// [`authenticate`](crate::authenticate) or `WebhookReceiver::receive_bytes`, which
    /// are handed the bytes already read; a transport that streams the body
    /// itself constructs it for the same failure. The
    /// [`source`](std::error::Error::source) is the transport's own error as
    /// text, a [`BodyError`].
    #[error("could not read the webhook body")]
    BodyRead(#[source] BodyError),
    /// The body is over the configured limit.
    ///
    /// On `WebhookReceiver::receive` the read stopped at the limit, from the
    /// body's size hint before the first frame or at the frame that crossed
    /// it; on `receive_bytes`, and for a transport that constructs this
    /// itself, the body was already in hand and its length was over.
    #[error("webhook body exceeds the configured {limit}-byte limit")]
    BodyTooLarge {
        /// The configured maximum body size.
        limit: usize,
    },
}

impl ReceiveError {
    /// The status the receiver answers this failure with: the crate's
    /// response contract.
    ///
    /// An absent or mismatched signature, and a request whose target no
    /// verifier is known for, are the client failing to
    /// authenticate, `401 Unauthorized`; a signature that is not `sha256=`
    /// and 64 hex digits, a missing required header, a content type other
    /// than `application/json` and a body the transport could not read are
    /// malformed requests, `400 Bad Request`; a body over the limit is `413
    /// Payload Too Large`. Payload bytes that are not valid JSON earn no
    /// status here: the envelope is built around them and a handler over it
    /// runs, so only an input that decodes them fails, as a handler failure.
    /// `WebhookReceiver` applies this itself; it is public so a transport
    /// built directly on [`authenticate`](crate::authenticate) answers GitHub the same
    /// way, as its docs show.
    #[must_use]
    pub const fn status(&self) -> StatusCode {
        self.refusal().status()
    }

    /// The class of this refusal: the one partition of the variants, which
    /// the status and the receive span's `outcome` label are both read off.
    ///
    /// A variant this crate adds fails to compile here until it is placed,
    /// and cannot be placed differently for the status and the label.
    pub(crate) const fn refusal(&self) -> Refusal {
        match self {
            Self::Signature(SignatureError::Missing | SignatureError::Mismatch)
            | Self::UnknownTarget => Refusal::Unauthorized,
            Self::Signature(SignatureError::Malformed)
            | Self::MissingHeader { .. }
            | Self::UnsupportedContentType
            | Self::BodyRead(_) => Refusal::BadRequest,
            Self::BodyTooLarge { .. } => Refusal::PayloadTooLarge,
        }
    }
}

/// The three classes a request is refused in before any handler runs, each
/// answered with one status, and each one `outcome` label on the receive
/// span, which the receiver spells since the vocabulary is the span's.
///
/// Exhaustive on purpose: the receiver's label is a match over it, so the
/// label and the status are read off one value and cannot disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The client failed to authenticate: `401`.
    Unauthorized,
    /// The request was malformed: `400`.
    BadRequest,
    /// The body was over the limit: `413`.
    PayloadTooLarge,
}

impl Refusal {
    pub(crate) const fn status(self) -> StatusCode {
        match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::BadRequest => StatusCode::BAD_REQUEST,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        }
    }
}

/// The transport's reason a webhook body could not be read, as text.
///
/// The [`source`](std::error::Error::source) of
/// [`ReceiveError::BodyRead`]: the body's own error type (`http_body`'s
/// `Body::Error`, so `hyper::Error`, `axum::Error`, a Worker's) rendered
/// through its `Display`, which is all the receiver asks of it. Carrying the
/// text rather than the error keeps [`ReceiveError`] comparable and
/// cloneable, and the transport's type out of this crate's API.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Error)]
#[error("{0}")]
pub struct BodyError(String);

impl BodyError {
    /// Captures `error`'s text: the transport's error, or a message a
    /// transport built on [`authenticate`](crate::authenticate) writes for itself.
    #[must_use]
    pub fn new(error: impl fmt::Display) -> Self {
        Self(error.to_string())
    }
}

/// Why an envelope's payload could not be decoded.
///
/// The one error type of every decode path: [`Envelope::decode`] and every
/// [`FromEnvelope`](crate::FromEnvelope) impl return it, and the dispatcher
/// reports it, boxed as the source of its
/// [`DispatchError`](crate::DispatchError), for a handler whose input could
/// not be decoded; `downcast_ref::<DecodeError>()` on that source is how a
/// policy tells a decode failure from the handler's own.
///
/// Three variants, each saying why: [`KindMismatch`](Self::KindMismatch),
/// when a [`Payload`](crate::Payload) type's kind disagrees with the
/// envelope's; [`Json`](Self::Json), when the bytes do not fit the type; and
/// [`Input`](Self::Input), the one a consumer's own
/// [`FromEnvelope`](crate::FromEnvelope) impl returns for a reason that is
/// neither, built with [`DecodeError::input`] or
/// [`DecodeError::input_with_source`]. The enum is `#[non_exhaustive]`, so a
/// `match` over it keeps a wildcard arm.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DecodeError {
    /// The envelope is of a kind the payload type does not cover.
    #[error("expected {} `{expected}` event, received `{actual}`", article(.expected))]
    KindMismatch {
        /// The kind the payload type declares.
        expected: EventKind,
        /// The kind of the envelope that arrived.
        actual: EventKind,
    },
    /// The payload did not decode into the expected type.
    #[error("payload could not be decoded")]
    Json(#[source] serde_json::Error),
    /// The input reported a reason of its own, neither a kind mismatch nor a
    /// JSON error.
    ///
    /// What a consumer's own [`FromEnvelope`](crate::FromEnvelope) impl
    /// returns when its decode fails on its own terms: a field the meta does
    /// not carry, a payload a view over several kinds accepts but cannot
    /// use. [`Display`](fmt::Display) is the consumer's message, verbatim;
    /// an underlying error the consumer attached is the
    /// [`source`](std::error::Error::source). Built with
    /// [`DecodeError::input`] or [`DecodeError::input_with_source`]. Named
    /// for what reports the reason, as `KindMismatch` and `Json` are named
    /// for what went wrong.
    ///
    /// The variant is non-exhaustive: use the constructors and match with
    /// `Input { message, source, .. }` so fields can be added later.
    ///
    /// ```compile_fail,E0639
    /// use octoevents::DecodeError;
    /// let error = DecodeError::Input { message: "invalid input".into(), source: None };
    /// ```
    #[error("{message}")]
    #[non_exhaustive]
    Input {
        /// The consumer's own reason, as `Display` shows it.
        message: Cow<'static, str>,
        /// The underlying error, when the consumer attached one.
        #[source]
        source: Option<BoxError>,
    },
}

/// The indefinite article `kind`'s wire string takes: "an `issues` event",
/// "a `push` event".
fn article(kind: &EventKind) -> &'static str {
    match kind.as_str().as_bytes().first() {
        Some(b'a' | b'e' | b'i' | b'o' | b'u') => "an",
        _ => "a",
    }
}

impl DecodeError {
    /// An [`Input`](Self::Input) error with `message` as its reason and no
    /// underlying source.
    ///
    /// The one line a consumer's [`FromEnvelope`](crate::FromEnvelope) impl
    /// needs to report a failure that is neither a kind mismatch nor a JSON
    /// error; the trait's docs show one over a required meta field. A
    /// `&'static str` or a `String` is accepted.
    #[must_use]
    pub fn input(message: impl Into<Cow<'static, str>>) -> Self {
        Self::Input {
            message: message.into(),
            source: None,
        }
    }

    /// An [`Input`](Self::Input) error with `message` as its reason and
    /// `source` as the underlying error.
    ///
    /// `Display` is the message; the source is one `source()` hop down, where
    /// a reporter that walks the chain finds it. Any error convertible to
    /// [`BoxError`] is accepted, including non-`Send` errors on `wasm32`.
    /// For a view that reads a field the payload
    /// carries as text and parses it further, the parse error is the source:
    ///
    /// ```
    /// use octoevents::{DecodeError, Envelope, EventMeta, FromEnvelope};
    ///
    /// #[derive(serde::Deserialize)]
    /// struct Tagged { release: Release }
    /// #[derive(serde::Deserialize)]
    /// struct Release { tag_name: String }
    ///
    /// /// The released major version, read off a `v1.2.3` tag.
    /// struct Major(u64);
    ///
    /// impl FromEnvelope for Major {
    ///     fn from_envelope(envelope: &Envelope, _meta: &EventMeta) -> Result<Self, DecodeError> {
    ///         let Tagged { release } = envelope.decode()?;
    ///         let tag = release.tag_name;
    ///         let major = tag.trim_start_matches('v').split('.').next().unwrap_or_default();
    ///         major.parse().map(Self).map_err(|error| {
    ///             DecodeError::input_with_source(format!("tag {tag} has no major version"), error)
    ///         })
    ///     }
    /// }
    /// ```
    #[must_use]
    pub fn input_with_source(
        message: impl Into<Cow<'static, str>>,
        source: impl Into<BoxError>,
    ) -> Self {
        Self::Input {
            message: message.into(),
            source: Some(source.into()),
        }
    }
}

fn serialize_bytes<S>(value: &Bytes, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&BASE64.encode(value))
}

fn deserialize_bytes<'de, D>(deserializer: D) -> Result<Bytes, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let encoded = String::deserialize(deserializer)?;
    BASE64
        .decode(encoded.as_bytes())
        .map(Bytes::from)
        .map_err(serde::de::Error::custom)
}

#[cfg(test)]
mod tests;
