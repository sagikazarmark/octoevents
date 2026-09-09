use std::{borrow::Cow, fmt};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use bytes::Bytes;
use http::{HeaderMap, HeaderName, StatusCode};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::value::RawValue;
use thiserror::Error;

use crate::{Action, EventKind, Signature, SignatureError, TargetType, Verifier, header};

/// The routing metadata of a webhook: everything in an [`Envelope`] except
/// the payload bytes.
///
/// A handler's input on its own, for one routed by kind and action that
/// decodes nothing, and the first half of [`Event<P>`](crate::Event), so the
/// delivery ID and installation ID travel beside a decoded payload without
/// going back to the envelope.
///
/// The crate produces this view and consumers only read it, so it is
/// `#[non_exhaustive]`: GitHub can add a stable routing field (an enterprise
/// reference, for example) without that becoming a breaking change here.
/// In a test, an envelope from [`Envelope::new`] carries the meta the
/// receiver would have extracted from the same bytes; build a meta by itself
/// with [`EventMeta::new`], for a handler over `EventMeta` alone, and assign
/// the optional fields it reads.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub struct EventMeta {
    /// The `X-GitHub-Delivery` value. Use it as a downstream idempotency key.
    pub delivery_id: String,
    /// The event kind parsed from `X-GitHub-Event`.
    pub kind: EventKind,
    /// The payload's top-level action, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<Action>,
    /// The GitHub App installation ID, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation_id: Option<u64>,
    /// A compact repository reference, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<RepositoryRef>,
    /// The organization login, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    /// The sender login, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<String>,
    /// The webhook installation target type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_type: Option<TargetType>,
    /// The webhook installation target ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<u64>,
}

impl EventMeta {
    /// Creates metadata for one delivery of one kind, with every optional
    /// field empty.
    ///
    /// This reads no bytes: the action, installation ID, repository,
    /// organization and sender are whatever the caller assigns. For a meta
    /// that agrees with a payload, build the envelope with [`Envelope::new`],
    /// which reads those fields from the payload the way the receiver does.
    /// This constructor is for a handler over `EventMeta` alone, or a test
    /// that wants the meta and nothing else.
    ///
    /// ```
    /// use octoevents::{Action, EventKind, EventMeta};
    ///
    /// let mut meta = EventMeta::new("72d3162e-cc78-11e3-81ab-4c9367dc0958", EventKind::Issues);
    /// meta.action = Some(Action::Opened);
    /// meta.installation_id = Some(42);
    /// # let _ = meta;
    /// ```
    #[must_use]
    pub fn new(delivery_id: impl Into<String>, kind: EventKind) -> Self {
        Self {
            delivery_id: delivery_id.into(),
            kind,
            action: None,
            installation_id: None,
            repository: None,
            organization: None,
            sender: None,
            target_type: None,
            target_id: None,
        }
    }
}

/// A compact repository reference extracted without parsing a full payload model.
///
/// `#[non_exhaustive]` for the same reason as [`EventMeta`]. Build one in tests
/// with [`RepositoryRef::new`], which takes every field the crate probes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RepositoryRef {
    /// GitHub's numeric repository ID.
    pub id: u64,
    /// The unqualified repository name.
    pub name: String,
    /// The owner-qualified repository name.
    pub full_name: String,
    /// The repository owner's login.
    pub owner: String,
}

impl RepositoryRef {
    /// Creates a reference from the fields GitHub sends in every payload's
    /// `repository` object.
    ///
    /// ```
    /// use octoevents::{EventKind, EventMeta, RepositoryRef};
    ///
    /// let mut meta = EventMeta::new("72d3162e-cc78-11e3-81ab-4c9367dc0958", EventKind::Push);
    /// meta.repository = Some(RepositoryRef::new(1296269, "Hello-World", "octocat/Hello-World", "octocat"));
    /// # let _ = meta;
    /// ```
    #[must_use]
    pub fn new(
        id: u64,
        name: impl Into<String>,
        full_name: impl Into<String>,
        owner: impl Into<String>,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            full_name: full_name.into(),
            owner: owner.into(),
        }
    }
}

/// The signature to verify, parsed from `headers`, or the header failure
/// [`Envelope::from_signed`] reports for it: [`SignatureError::Missing`]
/// when the header is absent, decided here and nowhere else;
/// [`SignatureError::Malformed`] when its bytes are not a signature, decided
/// by `Signature::try_from` and nowhere else. [`Verifier::verify`] takes the
/// parsed value and can only mismatch.
///
/// The value's bytes are parsed, not its `str`: an `http::HeaderValue` need
/// not be visible ASCII, and one that is not is present and not a signature
/// (400), never mistaken for an absent header (401), so a corrupting hop and
/// a missing header stay distinguishable.
///
/// Decidable from the headers alone, so the receiver uses it to refuse an
/// unsigned or malformed request before reading the body, and `from_signed`
/// uses it so both paths agree on which failure a header earns.
pub(crate) fn require_signature(headers: &HeaderMap) -> Result<Signature, SignatureError> {
    headers
        .get(&header::SIGNATURE)
        .ok_or(SignatureError::Missing)
        .and_then(Signature::try_from)
}

/// A GitHub webhook and its routing metadata.
///
/// An envelope is the composition of its routing metadata and the exact
/// payload bytes: `meta` is everything a handler needs to route, deduplicate,
/// and authenticate against GitHub, and `raw_payload` is the payload as it
/// arrived, undecoded. A handler over a decoded payload receives
/// [`EventMeta`] beside it, as [`Event<P>`](crate::Event), so the metadata
/// has one home rather than being duplicated onto every decoded view; the two
/// types hold the same document in its two states, `raw_payload` here and
/// `payload` there.
///
/// The crate produces envelopes and consumers read them. Outside the crate
/// one comes from [`Envelope::from_signed`], which authenticates an untrusted
/// request before it reads the payload, from [`Envelope::new`], which reads
/// the payload a test supplies the same way and authenticates nothing, or
/// from the serde `Deserialize` impl for one a trusted internal transport
/// forwarded (see the wire format below). Only the first carries an
/// authentication claim. The struct is `#[non_exhaustive]` so that a struct
/// literal cannot pair a meta with a payload that says something else; the
/// fields stay public, so reading them and destructuring with `..` work as
/// before:
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
///   "organization": "octocat",
///   "sender": "monalisa",
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
///   where `owner` is the login.
///
/// On deserialize, `delivery_id`, `kind` and `raw_payload` are required;
/// every other field is optional, and a field that is absent reads the same
/// as one that is `null`. On serialize, an optional field with no value is
/// omitted rather than written as `null`. Unknown fields are ignored, so a
/// producer may annotate the document for its own transport, and a producer
/// on a newer version of this crate does not break an older consumer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    #[serde(
        serialize_with = "serialize_bytes",
        deserialize_with = "deserialize_bytes"
    )]
    pub raw_payload: Bytes,
}

impl Envelope {
    /// Authenticates the body before constructing an envelope and extracting fields.
    ///
    /// This is the sans-I/O entry point: the receiver (`http-body` feature)
    /// is built on it, and a transport with no `http_body::Body` calls it
    /// directly with the request's `http::HeaderMap` and the body as
    /// [`Bytes`], which is the shape every surveyed Rust runtime hands over:
    /// `lambda_http` and `spin-sdk` give an `http::Request`, `worker` and
    /// `fastly` convert to one, and `aws_lambda_events` carries a `HeaderMap`
    /// in its event structs. A consumer hand-parsing a raw invocation event
    /// collects its `(name, value)` pairs into a `HeaderMap`, and header-name
    /// case is `HeaderName`'s to handle, not theirs. Answer with the
    /// receiver's contract, as an `http::StatusCode`: [`ReceiveError::status`]
    /// for a failure here, `NO_CONTENT` once the handler has succeeded, and
    /// `INTERNAL_SERVER_ERROR` when it has failed.
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
    /// target type and ID.
    ///
    /// The body must be `application/json`, which is a setting on the GitHub
    /// webhook; anything else is [`ReceiveError::UnsupportedContentType`]. The
    /// other setting, `application/x-www-form-urlencoded`, wraps the JSON in a
    /// `payload` form parameter and signs the form body, so the signed input
    /// would no longer be the payload every decode reads. Refusing it is what
    /// lets [`Envelope::raw_payload`] be both.
    ///
    /// # What the receiver adds
    ///
    /// `WebhookReceiver` (`http-body` feature) does three things around this
    /// call that a transport built directly on it must do for itself, or
    /// decide to go without. This is the receiver's sequence for a transport
    /// that is handed the headers and the body; each of the three returns the
    /// status the receiver would, and the handler runs only once all three
    /// have passed:
    ///
    /// ```
    /// use http::{HeaderMap, StatusCode};
    /// use octoevents::{
    ///     Bytes, DEFAULT_BODY_LIMIT, Envelope, EventKind, Handler, ReceiveError, Signature,
    ///     SignatureError, Verifier, header,
    /// };
    ///
    /// async fn receive<H: Handler<Envelope>>(
    ///     verifier: &Verifier,
    ///     headers: &HeaderMap,
    ///     body: Bytes,
    ///     handler: &H,
    /// ) -> StatusCode {
    ///     // Header-only rejection: a request whose signature header is
    ///     // absent (401) or not a signature (400) is refused before the body
    ///     // is read, so it never occupies memory. Decidable from the headers,
    ///     // so a transport that streams runs it before buffering;
    ///     // `from_signed` reaches the same answer for one that does not.
    ///     let signature = headers
    ///         .get(&header::SIGNATURE)
    ///         .ok_or(SignatureError::Missing)
    ///         .and_then(Signature::try_from);
    ///     if let Err(error) = signature {
    ///         return ReceiveError::from(error).status();
    ///     }
    ///
    ///     // The body limit: 413 past GitHub's 25 MiB cap. The receiver stops
    ///     // reading at the limit; a transport that streams does the same,
    ///     // and one handed the body already read checks its length. A read
    ///     // that fails partway is `ReceiveError::BodyRead`, 400, its text the
    ///     // crate's and the transport's error one `source()` beneath, as a
    ///     // `BodyError`; a transport handed the bytes never sees one.
    ///     if body.len() > DEFAULT_BODY_LIMIT {
    ///         return ReceiveError::BodyTooLarge { limit: DEFAULT_BODY_LIMIT }.status();
    ///     }
    ///
    ///     let envelope = match Envelope::from_signed(verifier, headers, body) {
    ///         Ok(envelope) => envelope,
    ///         Err(error) => return error.status(),
    ///     };
    ///
    ///     // The ping short-circuit: a verified `ping` is 204 and reaches no
    ///     // handler, unless the receiver was built with `handle_ping(true)`.
    ///     // After `from_signed`, so an unsigned ping is still 401.
    ///     if matches!(envelope.meta.kind, EventKind::Ping) {
    ///         return StatusCode::NO_CONTENT;
    ///     }
    ///
    ///     match handler.handle(envelope).await {
    ///         Ok(()) => StatusCode::NO_CONTENT,
    ///         Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    ///     }
    /// }
    /// ```
    ///
    /// Without the first, unsigned traffic is buffered before it is refused;
    /// without the second, this function verifies whatever it is given;
    /// without the third, a transport that forwards every envelope forwards
    /// pings too.
    ///
    /// # Errors
    ///
    /// Returns an authentication error first, [`ReceiveError::Signature`]:
    /// [`SignatureError::Missing`] when the header is absent,
    /// [`SignatureError::Malformed`] when it does not parse as a
    /// [`Signature`], [`SignatureError::Mismatch`] when no configured secret
    /// produced it for `body`, in that order. Then content-type and
    /// required-header errors, for an authenticated request.
    pub fn from_signed(
        verifier: &Verifier,
        headers: &HeaderMap,
        body: Bytes,
    ) -> Result<Self, ReceiveError> {
        let signature = require_signature(headers)?;
        verifier.verify(&signature, &body)?;

        if !header_str(headers, &header::CONTENT_TYPE).is_some_and(is_json_content_type) {
            return Err(ReceiveError::UnsupportedContentType);
        }

        let delivery_id = required_header(headers, header::DELIVERY_ID)?;
        let event_name = required_header(headers, header::EVENT_NAME)?;

        let mut envelope = Self::probed(delivery_id, EventKind::from(event_name), body);
        envelope.meta.target_type = header_str(headers, &header::TARGET_TYPE).map(TargetType::from);
        envelope.meta.target_id =
            header_str(headers, &header::TARGET_ID).and_then(|value| value.parse().ok());

        Ok(envelope)
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
    /// construction. Malformed top-level JSON leaves every payload-derived
    /// field empty; one malformed field (a `repository` object missing
    /// `full_name`, say) clears only that field and leaves its siblings
    /// intact. In both cases [`Envelope::raw_payload`] holds the bytes as
    /// given.
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
        Self::probed(delivery_id, kind, Bytes::copy_from_slice(payload.as_ref()))
    }

    /// The probe: the meta's payload-derived fields read from `raw_payload`,
    /// with the header-derived target left empty. Both constructors come
    /// through here, [`Envelope::new`] after copying a test's payload into
    /// [`Bytes`] and [`Envelope::from_signed`] with the authenticated body as
    /// it holds it.
    fn probed(delivery_id: impl Into<String>, kind: EventKind, raw_payload: Bytes) -> Self {
        let probe = serde_json::from_slice::<Probe<'_>>(&raw_payload).unwrap_or_default();

        let mut meta = EventMeta::new(delivery_id, kind);
        meta.action = probe
            .action
            .and_then(parse_probe::<String>)
            .map(|action| Action::from(action.as_str()));
        meta.installation_id = probe
            .installation
            .and_then(parse_probe::<IdOnly>)
            .map(|installation| installation.id);
        meta.repository = probe
            .repository
            .and_then(parse_probe::<RepoProbe>)
            .map(RepositoryRef::from);
        meta.organization = probe
            .organization
            .and_then(parse_probe::<LoginOnly>)
            .map(|organization| organization.login);
        meta.sender = probe
            .sender
            .and_then(parse_probe::<LoginOnly>)
            .map(|sender| sender.login);

        Self { meta, raw_payload }
    }

    /// Decodes the exact payload into a caller-defined view, checking nothing
    /// about the kind.
    ///
    /// `T` is any serde type and nothing ties it to the envelope's kind, so a
    /// view over fields several kinds share (the sender's `type`, say) decodes
    /// from an envelope of any kind. For a view bound to one kind, implement
    /// [`Payload`](crate::Payload) and call [`Envelope::decode_payload`],
    /// which refuses an envelope of another kind before decoding.
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
/// when the webhook is not configured as JSON; [`BodyRead`](Self::BodyRead),
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
    /// The request was not configured as JSON.
    #[error(
        "unsupported content type; configure the GitHub webhook content type as application/json"
    )]
    UnsupportedContentType,
    /// The transport failed while the body was being read.
    ///
    /// Produced by `WebhookReceiver` (`http-body` feature) when a body frame is an
    /// error rather than data or trailers: the connection dropped, the client
    /// stopped sending. Never by [`Envelope::from_signed`], which is handed
    /// the bytes already read; a transport that streams the body itself
    /// constructs it for the same failure. The [`source`](std::error::Error::source)
    /// is the transport's own error as text, a [`BodyError`].
    #[error("could not read the webhook body")]
    BodyRead(#[source] BodyError),
    /// The transport stopped reading after the configured limit.
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
    /// and 64 hex digits, a missing required header, a body that is not JSON
    /// and a body the transport could not read are malformed requests,
    /// `400 Bad Request`; a body over the limit is `413 Payload Too Large`.
    /// `WebhookReceiver` applies this itself; it is public so a transport
    /// built directly on [`Envelope::from_signed`] answers GitHub the same
    /// way, as its docs show.
    #[must_use]
    pub const fn status(&self) -> StatusCode {
        match self {
            Self::Signature(SignatureError::Missing | SignatureError::Mismatch) => {
                StatusCode::UNAUTHORIZED
            }
            Self::Signature(SignatureError::Malformed)
            | Self::MissingHeader { .. }
            | Self::UnsupportedContentType
            | Self::BodyRead(_) => StatusCode::BAD_REQUEST,
            Self::BodyTooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
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
/// The one error type of every decode path: [`Envelope::decode`],
/// [`Envelope::decode_payload`], and `Envelope::decode_event` (`octocrab`
/// feature) return it, and the dispatcher reports it for a handler whose
/// input could not be decoded. A single `From<DecodeError>` impl is therefore
/// the only conversion of a decode failure an application error needs,
/// whichever path decoded.
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
    /// [`DecodeError::input`] or [`DecodeError::input_with_source`].
    ///
    /// Named for what reports the reason, as `KindMismatch` and `Json` are
    /// named for what went wrong. `Custom` was considered and rejected as
    /// naming the mechanism (serde's `Error::custom`) rather than the
    /// concept, and `Missing { field }` as covering the meta-derived case
    /// exactly while leaving a cross-kind view's other reasons unnameable.
    #[error("{message}")]
    Input {
        /// The consumer's own reason, as `Display` shows it.
        message: Cow<'static, str>,
        /// The underlying error, when the consumer attached one.
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
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
    /// an observer that walks the chain finds it. Any `Error + Send + Sync`
    /// is accepted, boxed or not. For a view that reads a field the payload
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
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self::Input {
            message: message.into(),
            source: Some(source.into()),
        }
    }
}

/// The first value of `name` in `headers` as a string, or `None` when the
/// header is absent or its value is not visible ASCII: a value the crate
/// cannot read is a value it does not have. The receiver reads the span's
/// `delivery_id` and `event` through it too, so the two read a header alike.
pub(crate) fn header_str<'a>(headers: &'a HeaderMap, name: &HeaderName) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// A header the receiving path cannot do without: [`header_str`], with an
/// empty value refused as [`ReceiveError::MissingHeader`] like an absent one.
fn required_header(headers: &HeaderMap, name: HeaderName) -> Result<&str, ReceiveError> {
    header_str(headers, &name)
        .filter(|value| !value.is_empty())
        .ok_or(ReceiveError::MissingHeader { name })
}

fn is_json_content_type(value: &str) -> bool {
    value
        .split(';')
        .next()
        .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("application/json"))
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

fn parse_probe<T: serde::de::DeserializeOwned>(value: &RawValue) -> Option<T> {
    serde_json::from_str(value.get()).ok()
}

#[derive(Debug, Default, Deserialize)]
struct Probe<'a> {
    #[serde(borrow)]
    action: Option<&'a RawValue>,
    #[serde(borrow)]
    installation: Option<&'a RawValue>,
    #[serde(borrow)]
    repository: Option<&'a RawValue>,
    #[serde(borrow)]
    organization: Option<&'a RawValue>,
    #[serde(borrow)]
    sender: Option<&'a RawValue>,
}

#[derive(Debug, Deserialize)]
struct IdOnly {
    id: u64,
}

#[derive(Debug, Deserialize)]
struct RepoProbe {
    id: u64,
    name: String,
    full_name: String,
    owner: LoginOnly,
}

impl From<RepoProbe> for RepositoryRef {
    fn from(repository: RepoProbe) -> Self {
        Self {
            id: repository.id,
            name: repository.name,
            full_name: repository.full_name,
            owner: repository.owner.login,
        }
    }
}

#[derive(Debug, Deserialize)]
struct LoginOnly {
    login: String,
}

#[cfg(test)]
mod tests;
