use std::{borrow::Cow, fmt};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use bytes::Bytes;
use http::{HeaderMap, HeaderName, StatusCode};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

use crate::{
    AccountMeta, Action, BoxError, EventKind, EventMeta, RepositoryMeta, SignatureError,
    TargetType, Verifier, header,
};

/// The verified unit of receipt: the exact payload bytes and their metadata.
///
/// An envelope is the composition of its routing metadata and the exact
/// payload bytes: `meta` is what a handler routes and deduplicates by, and
/// what a GitHub App acts on the API with (the `installation_id` it mints its
/// token for); `raw_payload` is the payload as it arrived, undecoded. A
/// handler over a decoded payload receives [`EventMeta`] beside it, as
/// [`Event<P>`](crate::Event), so the metadata has one home rather than
/// being duplicated onto every decoded view; the two types hold the same
/// document in its two states, `raw_payload` here and `payload` there.
///
/// The crate produces envelopes and consumers read them. Outside the crate
/// one comes from [`Envelope::from_signed`], which authenticates an untrusted
/// request before it reads the payload, from [`Envelope::new`], which reads
/// the payload a test supplies the same way and authenticates nothing, or
/// from the serde `Deserialize` impl for one a trusted internal transport
/// forwarded (see the wire format below). Only the first carries an
/// authentication claim. The struct is `#[non_exhaustive]` so that a struct
/// literal cannot pair a meta with a payload that says something else; the
/// fields stay public, so reading them and destructuring with `..` work:
///
/// ```
/// use octoevents::{Action, Envelope, EventKind};
///
/// let envelope = Envelope::new("delivery-1", EventKind::Issues, br#"{"action":"opened"}"#);
///
/// let Envelope { meta, .. } = &envelope;
/// assert_eq!(meta.action, Some(Action::Opened));
/// assert_eq!(envelope.raw_payload.len(), 19);
///
/// // A field the payload cannot supply is assigned afterwards.
/// let mut envelope = envelope;
/// envelope.meta.target_id = Some(7);
/// ```
///
/// The literal is rejected outside the crate, where it could otherwise
/// disagree with the payload:
///
/// ```compile_fail,E0639
/// use octoevents::{Bytes, Envelope, EventKind, EventMeta};
///
/// let envelope = Envelope {
///     meta: EventMeta::new("delivery-1", EventKind::Issues),
///     raw_payload: Bytes::from_static(br#"{"action":"opened"}"#),
/// };
/// ```
///
/// [`EventMeta`] is `#[non_exhaustive]` too, for the other reason: GitHub can
/// add a stable routing field without that being a breaking change here.
///
/// # Wire format
///
/// A serialized envelope is one flat JSON object: the metadata sits at the
/// top level beside `raw_payload`, with no `meta` nesting, so a consumer in
/// another language reads it without knowing the Rust-side split. This is
/// the envelope of a `pull_request` delivery with every field present:
///
/// ```
/// use octoevents::{Action, Bytes, Envelope, EventKind, TargetType};
///
/// let document = r#"{
///   "delivery_id": "72d3162e-cc78-11e3-81ab-4c9367dc0958",
///   "kind": "pull_request",
///   "action": "opened",
///   "installation_id": 42,
///   "repository": {
///     "id": 1296269,
///     "name": "Hello-World",
///     "full_name": "octocat/Hello-World",
///     "owner": "octocat"
///   },
///   "organization": { "id": 9919, "login": "github" },
///   "sender": { "id": 583231, "login": "octocat" },
///   "target_type": "integration",
///   "target_id": 12345,
///   "raw_payload": "eyJhY3Rpb24iOiJvcGVuZWQifQ=="
/// }"#;
///
/// let envelope: Envelope = serde_json::from_str(document).unwrap();
/// assert_eq!(envelope.meta.kind, EventKind::PullRequest);
/// assert_eq!(envelope.meta.action, Some(Action::Opened));
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
///   the payload, and the meta beside it, some 300 bytes with every field
///   present, repeats values the payload already carries (the action, the
///   installation, the repository, the sender). The document therefore
///   roughly doubles a small payload of a few hundred bytes, and settles
///   toward 4/3 of the several-kilobyte payloads GitHub usually sends.
/// - `kind`, `action` and `target_type` are GitHub's wire strings
///   (`"pull_request"`, `"opened"`, `"integration"`); a value this version
///   of the crate does not know reads back as the `Unknown` variant carrying
///   the string, never as an error.
/// - `repository` is an object with `id`, `name`, `full_name` and `owner`,
///   where `owner` is the login; `organization` and `sender` are objects
///   with `id` and `login`, the subset of GitHub's own account object that
///   the meta keeps.
///
/// On deserialize, `delivery_id`, `kind` and `raw_payload` are required;
/// every other field is optional, and a field that is absent reads the same
/// as one that is `null`. On serialize, an optional field with no value is
/// omitted rather than written as `null`. Unknown fields are ignored, so a
/// producer may annotate the document for its own transport.
///
/// The wire format follows the crate's versioning, with no separate version
/// field. Adding optional fields is wire-compatible. Removing or renaming
/// existing fields, making optional fields required, or changing existing
/// fields' encodings or meanings is treated as a breaking change and
/// documented in release notes. Before 1.0, a minor release may include such
/// changes; producers and consumers crossing that boundary must migrate
/// together or use a transport adapter.
///
/// The meta is read back as forwarded: nothing is verified, and the payload
/// is not probed again, so a document whose `action` disagrees with the
/// `action` inside its `raw_payload` reads back disagreeing. The producer is
/// trusted to have built the envelope through one of the two constructors,
/// which is what the wire format is for: a hop between services of one
/// deployment, not an input from outside it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[non_exhaustive]
pub struct Envelope {
    /// The routing metadata extracted from the headers and the payload probe.
    #[serde(flatten)]
    pub meta: EventMeta,
    /// The payload as it arrived: the exact bytes, undecoded and never
    /// re-encoded.
    ///
    /// On the receiving path these are the bytes the signature was verified
    /// over; [`Envelope::new`] and the `Deserialize` impl hold whatever they
    /// were given, with no such claim. Either way they are the bytes every
    /// decode reads. Serialized as standard base64 so an envelope survives a
    /// JSON hop to an internal service without the payload being re-encoded;
    /// the encoded field is 4/3 of the payload's size.
    #[serde(serialize_with = "serialize_bytes")]
    pub raw_payload: Bytes,
}

// Keep this flat: serde's `flatten` buffers unknown values before ignoring
// them, imposing numeric, string and nesting limits on transport annotations.
// Listing the fields here lets derived deserialization skip them instead.
// Keep the metadata fields in sync with EventMeta; the round-trip tests cover
// every current field. Conversion preserves forwarded meta without probing.
#[derive(Deserialize)]
struct EnvelopeWire {
    delivery_id: String,
    kind: EventKind,
    action: Option<Action>,
    installation_id: Option<u64>,
    repository: Option<RepositoryMeta>,
    organization: Option<AccountMeta>,
    sender: Option<AccountMeta>,
    target_type: Option<TargetType>,
    target_id: Option<u64>,
    #[serde(deserialize_with = "deserialize_bytes")]
    raw_payload: Bytes,
}

impl From<EnvelopeWire> for Envelope {
    fn from(wire: EnvelopeWire) -> Self {
        Self {
            meta: EventMeta {
                delivery_id: wire.delivery_id,
                kind: wire.kind,
                action: wire.action,
                installation_id: wire.installation_id,
                repository: wire.repository,
                organization: wire.organization,
                sender: wire.sender,
                target_type: wire.target_type,
                target_id: wire.target_id,
            },
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
    /// Verifies the signature over the body, then builds the envelope.
    ///
    /// This is the one step of receiving that produces the envelope, and the
    /// receiver is built on it. A transport calls it directly when it wants
    /// the envelope and not the receiver's answer: to forward the envelope
    /// over the wire format, to persist it before any handler runs, or to
    /// route it itself.
    ///
    /// It takes the request's `http::HeaderMap` and the body as [`Bytes`],
    /// the shape every surveyed Rust runtime hands over. A consumer
    /// hand-parsing a raw invocation event collects its `(name, value)` pairs
    /// into a `HeaderMap`; header-name case is `HeaderName`'s to handle. A
    /// failure is answered with [`ReceiveError::status`], the receiver's
    /// contract.
    ///
    /// Verification authenticates only the payload bytes. GitHub's signature
    /// does not cover the delivery ID, event name, or target headers, so
    /// their metadata is not an authenticated authorization claim. Use
    /// authenticated payload data or independently trusted configuration for
    /// authorization. Delivery-ID deduplication handles GitHub redelivery,
    /// not an attacker resubmitting a captured signed payload with a new ID.
    ///
    /// For the whole contract over the same two arguments (the header-only
    /// refusal, the body limit, the `ping` short-circuit, the handler, the
    /// observer and the tracing), call
    /// [`WebhookReceiver::receive_bytes`](crate::WebhookReceiver::receive_bytes)
    /// instead, which is in the core beside this.
    ///
    /// The headers are read by the names in [`header`](crate::header), by
    /// which `HeaderMap` matches case-insensitively, and a repeated header
    /// reads as its first value. The signature is parsed from the header
    /// value's bytes, so a value that is not visible ASCII is
    /// [`SignatureError::Malformed`], not `Missing`; for every other header
    /// such a value reads as absent, and an empty delivery ID or event name
    /// is [`ReceiveError::MissingHeader`] as an absent one is.
    ///
    /// The probe of the payload is best-effort and never fails the
    /// construction; the rules are on [`Envelope::new`], which reads the
    /// payload the same way. Once the body is authenticated and the headers
    /// are read, this constructor adds what only the headers carry: the
    /// target type and ID. A target type this crate does not know is
    /// [`TargetType::Unknown`] with the value intact; a target ID that is
    /// present but not a number reads as `None`, since the header is
    /// optional and refusing an authenticated delivery over it would serve
    /// nothing.
    ///
    /// The body must be `application/json`, which is a setting on the GitHub
    /// webhook; anything else is [`ReceiveError::UnsupportedContentType`]. The
    /// other setting, `application/x-www-form-urlencoded`, wraps the JSON in a
    /// `payload` form parameter and signs the form body, so the signed input
    /// would no longer be the payload every decode reads. Refusing it is what
    /// lets [`Envelope::raw_payload`] be both.
    ///
    /// In a test, the request is signed with the verifier the envelope is
    /// checked against:
    ///
    /// ```
    /// use http::HeaderMap;
    /// use octoevents::{Action, Bytes, Envelope, EventKind, Verifier, WebhookSecret, header};
    ///
    /// let verifier = Verifier::new(WebhookSecret::new("test-secret"));
    /// let body = Bytes::from_static(br#"{"action":"opened","installation":{"id":42}}"#);
    ///
    /// let mut headers = HeaderMap::new();
    /// headers.insert(header::CONTENT_TYPE, "application/json".parse()?);
    /// headers.insert(header::DELIVERY_ID, "delivery-1".parse()?);
    /// headers.insert(header::EVENT_NAME, "issues".parse()?);
    /// headers.insert(header::SIGNATURE, verifier.sign(&body).into());
    ///
    /// let envelope = Envelope::from_signed(&verifier, &headers, body)?;
    ///
    /// assert_eq!(envelope.meta.delivery_id, "delivery-1");
    /// assert_eq!(envelope.meta.kind, EventKind::Issues);
    /// assert_eq!(envelope.meta.action, Some(Action::Opened));
    /// assert_eq!(envelope.meta.installation_id, Some(42));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # What the receiver adds
    ///
    /// Around this call the receiver refuses a request whose signature header
    /// is absent (401) or not a signature (400) from the headers alone: on
    /// `receive`, before the body is read from the transport, so unsigned
    /// traffic is never buffered by the receiver, and on `receive_bytes`,
    /// before anything else, the bytes being the caller's already. It bounds
    /// the payload length at the configured limit (413), and answers a
    /// verified `ping` 204 before any handler runs, unless asked to
    /// `handle_ping`. A transport
    /// calling this function directly does those for itself, or decides to go
    /// without: without the first, unsigned traffic is buffered before it is
    /// refused; without the second, this function verifies whatever it is
    /// given; without the third, a transport that forwards every envelope
    /// forwards pings too. The header check is `Signature::try_from` on the
    /// [`header::SIGNATURE`](crate::header::SIGNATURE) value, answered with
    /// [`ReceiveError::status`]:
    ///
    /// ```
    /// use http::{HeaderMap, StatusCode};
    /// use octoevents::{Bytes, Envelope, ReceiveError, Signature, SignatureError, Verifier, WebhookSecret, header};
    ///
    /// fn envelope_or_status(
    ///     verifier: &Verifier,
    ///     headers: &HeaderMap,
    ///     body: Bytes,
    /// ) -> Result<Envelope, StatusCode> {
    ///     // Decidable from the headers, so a transport that streams runs it
    ///     // before buffering; `from_signed` reaches the same answer after.
    ///     headers
    ///         .get(&header::SIGNATURE)
    ///         .ok_or(SignatureError::Missing)
    ///         .and_then(Signature::try_from)
    ///         .map_err(|error| ReceiveError::from(error).status())?;
    ///
    ///     Envelope::from_signed(verifier, headers, body).map_err(|error| error.status())
    /// }
    ///
    /// // An unsigned request is refused at the header check, before the body is read.
    /// let verifier = Verifier::new(WebhookSecret::new("test-secret"));
    /// let unsigned = HeaderMap::new();
    /// assert_eq!(
    ///     envelope_or_status(&verifier, &unsigned, Bytes::from_static(b"{}")).unwrap_err(),
    ///     StatusCode::UNAUTHORIZED
    /// );
    /// ```
    ///
    /// # Errors
    ///
    /// Returns a signature error first, [`ReceiveError::Signature`]:
    /// [`SignatureError::Missing`] when the header is absent,
    /// [`SignatureError::Malformed`] when it does not parse as a
    /// [`Signature`](crate::Signature), [`SignatureError::Mismatch`] when no configured secret
    /// produced it for `body`, in that order. Then content-type and
    /// required-header errors, for an authenticated request.
    pub fn from_signed(
        verifier: &Verifier,
        headers: &HeaderMap,
        body: Bytes,
    ) -> Result<Self, ReceiveError> {
        let signature = header::signature(headers)?;
        verifier.verify(&signature, &body)?;

        if !header::is_json(headers) {
            return Err(ReceiveError::UnsupportedContentType);
        }

        let delivery_id = header::required(headers, header::DELIVERY_ID)?;
        let event_name = header::required(headers, header::EVENT_NAME)?;

        let mut meta = EventMeta::probe(delivery_id, EventKind::from(event_name), &body);
        meta.target_type = header::read(headers, &header::TARGET_TYPE).map(TargetType::from);
        meta.target_id =
            header::read(headers, &header::TARGET_ID).and_then(|value| value.parse().ok());

        Ok(Self {
            meta,
            raw_payload: body,
        })
    }

    /// Builds an envelope from the delivery ID, the kind and the payload
    /// bytes, verifying nothing.
    ///
    /// This is the test's path: a handler is tested through
    /// [`Dispatcher::dispatch`](crate::Dispatcher::dispatch) with an envelope
    /// built here, so nothing is signed and no [`Verifier`] is needed. The
    /// receiving path is [`Envelope::from_signed`], which authenticates the
    /// request first and reads the payload the same way.
    ///
    /// The meta carries what the receiver would have read from the same
    /// payload: the action, the installation ID, the repository, the
    /// organization and the sender, so a handler over
    /// [`Event<P>`](crate::Event) sees the `installation_id` the payload
    /// carries rather than whatever a test remembered to assign. The target
    /// type and ID come from headers this constructor does not have, so they
    /// stay `None`; assign them if the handler reads them.
    ///
    /// The read of the payload is best-effort and never fails the
    /// construction. Invalid UTF-8 anywhere in the payload, malformed JSON
    /// syntax, or a top level that is not an object (`[]`, `42`), leaves every
    /// payload-derived field empty; one malformed field (a `repository`
    /// object missing `full_name`, say)
    /// clears only that field and leaves its siblings intact. Installation,
    /// repository, repository owner, organization and sender must be objects,
    /// never positional arrays; a malformed owner clears the repository.
    /// A duplicated top-level metadata key clears that field, even if the
    /// values agree or one is null. A duplicate required key inside a metadata
    /// object invalidates that object, clearing its top-level metadata field.
    /// Unknown keys are ignored. Skipped string values must have valid escape
    /// syntax (`\q` and incomplete `\uXXXX` escapes clear every payload-derived
    /// field), but escaped surrogates need not be paired there: `\uD800` in a
    /// skipped value leaves the metadata intact. Strings retained as metadata
    /// must decode to Unicode scalar values; an unpaired escaped surrogate
    /// clears only the metadata field that reads it. Valid surrogate pairs
    /// decode normally. A handler's decode can therefore fail on a string the
    /// probe skipped. In every case [`Envelope::raw_payload`] holds the bytes
    /// as given.
    ///
    /// The payload is anything that views as bytes, a byte-string literal
    /// included, and is copied into [`Envelope::raw_payload`]; a test's
    /// payload is small and the copy is one allocation.
    ///
    /// ```
    /// use octoevents::{Action, Envelope, EventKind};
    ///
    /// let envelope = Envelope::new(
    ///     "72d3162e-cc78-11e3-81ab-4c9367dc0958",
    ///     EventKind::Issues,
    ///     br#"{"action":"opened","installation":{"id":42},"issue":{"number":7}}"#,
    /// );
    ///
    /// assert_eq!(envelope.meta.action, Some(Action::Opened));
    /// assert_eq!(envelope.meta.installation_id, Some(42));
    /// assert_eq!(envelope.meta.target_id, None);
    /// ```
    #[must_use]
    pub fn new(delivery_id: impl Into<String>, kind: EventKind, payload: impl AsRef<[u8]>) -> Self {
        let raw_payload = Bytes::copy_from_slice(payload.as_ref());
        Self {
            meta: EventMeta::probe(delivery_id, kind, &raw_payload),
            raw_payload,
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
    /// ```
    /// use octoevents::{Envelope, EventKind};
    ///
    /// /// The sender's account type, which every kind carries.
    /// #[derive(serde::Deserialize)]
    /// struct SenderType { sender: Sender }
    /// #[derive(serde::Deserialize)]
    /// struct Sender { r#type: String }
    ///
    /// let envelope = Envelope::new(
    ///     "delivery-1",
    ///     EventKind::Push,
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
    pub fn decode<T: serde::de::DeserializeOwned>(&self) -> Result<T, DecodeError> {
        serde_json::from_slice(&self.raw_payload).map_err(DecodeError::Json)
    }
}

/// A failure while receiving a webhook.
///
/// What the receiving path reports before any handler runs, each variant
/// naming the step that refused the request: [`Signature`](Self::Signature),
/// when the signature header is absent, malformed or does not match;
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
    /// [`Envelope::from_signed`] or `WebhookReceiver::receive_bytes`, which
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
    /// An absent or mismatched signature is the client failing to
    /// authenticate, `401 Unauthorized`; a signature that is not `sha256=`
    /// and 64 hex digits, a missing required header, a content type other
    /// than `application/json` and a body the transport could not read are
    /// malformed requests, `400 Bad Request`; a body over the limit is `413
    /// Payload Too Large`. Payload bytes that are not valid JSON earn no
    /// status here: the envelope is built around them and a handler over it
    /// runs, so only an input that decodes them fails, as a handler failure.
    /// `WebhookReceiver` applies this itself; it is public so a transport
    /// built directly on [`Envelope::from_signed`] answers GitHub the same
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
            Self::Signature(SignatureError::Missing | SignatureError::Mismatch) => {
                Refusal::Unauthorized
            }
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
    /// transport built on [`Envelope::from_signed`] writes for itself.
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
    /// an observer that walks the chain finds it. Any error convertible to
    /// [`BoxError`] is accepted, including non-`Send` errors on `wasm32`.
    /// For a view that reads a field the payload
    /// carries as text and parses it further, the parse error is the source:
    ///
    /// ```
    /// use octoevents::{DecodeError, Envelope, FromEnvelope};
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
    ///     fn from_envelope(envelope: &Envelope) -> Result<Self, DecodeError> {
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
