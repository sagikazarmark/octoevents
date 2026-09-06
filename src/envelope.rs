use std::{borrow::Cow, fmt, str::FromStr};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use bytes::Bytes;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::value::RawValue;
use thiserror::Error;

use crate::{Action, EventKind, Payload, Verifier, VerifyError, header};

/// The routing metadata of a webhook: everything in an [`Envelope`] except
/// the payload bytes.
///
/// Typed handlers receive this alongside a decoded payload, so the delivery
/// ID and installation ID are available without going back to the envelope.
///
/// The crate produces this view and consumers only read it, so it is
/// `#[non_exhaustive]`: GitHub can add a stable routing field (an enterprise
/// reference, for example) without that becoming a breaking change here.
/// Build one in tests with [`EventMeta::new`] and assign the optional fields
/// you need.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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

/// The resource on which the webhook is installed.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TargetType {
    /// A GitHub App installation target.
    Integration,
    /// A repository webhook target.
    Repository,
    /// An organization webhook target.
    Organization,
    /// A wire value unknown to this version of the crate.
    Unknown(String),
}

impl Serialize for TargetType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for TargetType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        String::deserialize(deserializer)
            .map(|value| Self::from_str(&value).unwrap_or_else(|never| match never {}))
    }
}

impl TargetType {
    /// Returns GitHub's wire representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Integration => "integration",
            Self::Repository => "repository",
            Self::Organization => "organization",
            Self::Unknown(value) => value,
        }
    }
}

impl FromStr for TargetType {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value {
            "integration" => Self::Integration,
            "repository" => Self::Repository,
            "organization" => Self::Organization,
            value => Self::Unknown(value.to_owned()),
        })
    }
}

/// The headers needed to authenticate and route a GitHub webhook.
///
/// Use [`From`] with an `http::HeaderMap` when the `http` feature is enabled.
/// Otherwise look each header up in your transport's map by the names in
/// [`header`](crate::header) and pass the value to the setter of the same
/// name; the module docs show the shape.
///
/// Header-name case is the caller's concern. The view holds values, and
/// which header a value came from is fixed by the setter that received it,
/// so it never compares a name. `http::HeaderMap` matches names
/// case-insensitively; a plain string map does not, and the casing its keys
/// hold depends on the hop that filled it: HTTP/1.1 carries names as the
/// sender wrote them (GitHub writes `X-GitHub-Delivery`), HTTP/2 and HTTP/3
/// lowercase them, and a gateway in between may do either. A transport whose
/// map keeps the sender's casing lowercases its keys, or compares
/// case-insensitively, before looking up the lowercase constants.
#[derive(Clone, Default)]
pub struct HeaderView<'a> {
    signature: Option<Cow<'a, str>>,
    // Crate-visible so the receiver can record them on its span before
    // verification; the signature stays private and is reached only through
    // `require_signature`, so it cannot be recorded by accident.
    pub(crate) delivery_id: Option<Cow<'a, str>>,
    pub(crate) event_name: Option<Cow<'a, str>>,
    content_type: Option<Cow<'a, str>>,
    target_type: Option<Cow<'a, str>>,
    target_id: Option<Cow<'a, str>>,
    malformed_signature: bool,
}

impl fmt::Debug for HeaderView<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HeaderView")
            .field("signature", &self.signature.as_ref().map(|_| "[REDACTED]"))
            .field("delivery_id", &self.delivery_id)
            .field("event_name", &self.event_name)
            .field("content_type", &self.content_type)
            .field("target_type", &self.target_type)
            .field("target_id", &self.target_id)
            .field("malformed_signature", &self.malformed_signature)
            .finish()
    }
}

impl<'a> HeaderView<'a> {
    /// Creates an empty view.
    ///
    /// Every header is named by its own method rather than passed positionally,
    /// because the protocol headers are all optional strings and transposing
    /// two of them would otherwise compile silently. Each setter accepts a
    /// borrowed or an owned value, so a header assembled at run time does not
    /// need to outlive the view on its own.
    ///
    /// ```
    /// use octoevents::HeaderView;
    ///
    /// let headers = HeaderView::new()
    ///     .signature("sha256=...")
    ///     .delivery_id("72d3162e-cc78-11e3-81ab-4c9367dc0958")
    ///     .event_name("pull_request")
    ///     .content_type("application/json");
    /// # let _ = headers;
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the `X-Hub-Signature-256` value.
    #[must_use]
    pub fn signature(mut self, value: impl Into<Cow<'a, str>>) -> Self {
        self.signature = Some(value.into());
        self
    }

    /// Sets the `X-GitHub-Delivery` value.
    #[must_use]
    pub fn delivery_id(mut self, value: impl Into<Cow<'a, str>>) -> Self {
        self.delivery_id = Some(value.into());
        self
    }

    /// Sets the `X-GitHub-Event` value.
    #[must_use]
    pub fn event_name(mut self, value: impl Into<Cow<'a, str>>) -> Self {
        self.event_name = Some(value.into());
        self
    }

    /// Sets the `Content-Type` value.
    #[must_use]
    pub fn content_type(mut self, value: impl Into<Cow<'a, str>>) -> Self {
        self.content_type = Some(value.into());
        self
    }

    /// Sets the `X-GitHub-Hook-Installation-Target-Type` value.
    #[must_use]
    pub fn target_type(mut self, value: impl Into<Cow<'a, str>>) -> Self {
        self.target_type = Some(value.into());
        self
    }

    /// Sets the `X-GitHub-Hook-Installation-Target-ID` value.
    #[must_use]
    pub fn target_id(mut self, value: impl Into<Cow<'a, str>>) -> Self {
        self.target_id = Some(value.into());
        self
    }

    /// The signature to verify, or the header failure [`Envelope::from_signed`]
    /// reports for it.
    ///
    /// Decidable from the headers alone, so the receiver uses it to refuse an
    /// unsigned request before reading the body, and `from_signed` uses it so
    /// both paths agree on which failure a header earns.
    pub(crate) fn require_signature(&self) -> Result<&str, VerifyError> {
        if self.malformed_signature {
            return Err(VerifyError::MalformedSignature);
        }
        self.signature
            .as_deref()
            .ok_or(VerifyError::MissingSignature)
    }
}

#[cfg(feature = "http")]
impl<'a> From<&'a http::HeaderMap> for HeaderView<'a> {
    fn from(headers: &'a http::HeaderMap) -> Self {
        fn value<'a>(headers: &'a http::HeaderMap, name: &'static str) -> Option<Cow<'a, str>> {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(Cow::Borrowed)
        }

        Self {
            signature: value(headers, header::SIGNATURE),
            delivery_id: value(headers, header::DELIVERY_ID),
            event_name: value(headers, header::EVENT_NAME),
            content_type: value(headers, header::CONTENT_TYPE),
            target_type: value(headers, header::TARGET_TYPE),
            target_id: value(headers, header::TARGET_ID),
            malformed_signature: headers
                .get(header::SIGNATURE)
                .is_some_and(|value| value.to_str().is_err()),
        }
    }
}

/// A GitHub webhook and its routing metadata.
///
/// An envelope is the composition of its routing metadata and the exact
/// payload bytes: `meta` is everything a handler needs to route, deduplicate,
/// and authenticate against GitHub, and `raw` is the signed input. Typed
/// handlers receive only [`EventMeta`] with a decoded payload, so the metadata
/// has one home rather than being duplicated onto every decoded view.
///
/// [`Envelope::from_signed`] is the only path in this crate that turns an
/// untrusted request into an envelope, and it authenticates before it extracts.
/// The fields are nevertheless public and the struct is deliberately *not*
/// `#[non_exhaustive]`: consumers must be able to build synthetic envelopes to
/// unit-test handlers and dispatchers without HTTP, and to reconstruct one that
/// a trusted internal transport forwarded (see the wire format below). A
/// value obtained that way carries no authentication claim; only one returned
/// by [`Envelope::from_signed`] does. Extensibility lives in [`EventMeta`],
/// which is `#[non_exhaustive]` and built with [`EventMeta::new`].
///
/// # Wire format
///
/// A serialized envelope is one flat JSON object: the metadata sits at the
/// top level beside `raw`, with no `meta` nesting, so a consumer in another
/// language reads it without knowing the Rust-side split. This is the
/// envelope of a `pull_request` delivery with every field present:
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
///   "raw": "eyJhY3Rpb24iOiJvcGVuZWQifQ=="
/// }"#;
///
/// let envelope: Envelope = serde_json::from_str(document).unwrap();
/// assert_eq!(envelope.meta.kind, EventKind::PullRequest);
/// assert_eq!(envelope.meta.action, Some(Action::Opened));
/// assert_eq!(envelope.meta.target_type, Some(TargetType::Integration));
/// assert_eq!(envelope.raw, Bytes::from_static(br#"{"action":"opened"}"#));
///
/// // Serializing produces the same document back.
/// let expected: serde_json::Value = serde_json::from_str(document).unwrap();
/// assert_eq!(serde_json::to_value(&envelope).unwrap(), expected);
/// ```
///
/// - `raw` is the exact payload bytes in standard base64 with padding
///   (RFC 4648 section 4), so the payload survives the hop without being
///   re-encoded and still verifies against GitHub's signature.
/// - `kind`, `action` and `target_type` are GitHub's wire strings
///   (`"pull_request"`, `"opened"`, `"integration"`); a value this version
///   of the crate does not know reads back as the `Unknown` variant carrying
///   the string, never as an error.
/// - `repository` is an object with `id`, `name`, `full_name` and `owner`,
///   where `owner` is the login.
///
/// On deserialize, `delivery_id`, `kind` and `raw` are required; every other
/// field is optional, and a field that is absent reads the same as one that
/// is `null`. On serialize, an optional field with no value is omitted rather
/// than written as `null`. Unknown fields are ignored, so a producer may
/// annotate the document for its own transport, and a producer on a newer
/// version of this crate does not break an older consumer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    /// The routing metadata extracted from the headers and the payload probe.
    #[serde(flatten)]
    pub meta: EventMeta,
    /// The exact bytes over which the signature was calculated.
    ///
    /// Serialized as standard base64 so an envelope survives a JSON hop to an
    /// internal service without the body being re-encoded.
    #[serde(
        serialize_with = "serialize_bytes",
        deserialize_with = "deserialize_bytes"
    )]
    pub raw: Bytes,
}

impl Envelope {
    /// Authenticates the body before constructing an envelope and extracting fields.
    ///
    /// This is the sans-I/O entry point: the `http`-feature receiver is built
    /// on it, and a transport that has no `http::Request` (a serverless
    /// runtime handing over a header map and a body string, say) calls it
    /// directly with a [`HeaderView`] and the body as [`Bytes`]. Answer with
    /// the receiver's contract, as [`ResponseStatus`](crate::ResponseStatus):
    /// [`for_receive_error`](crate::ResponseStatus::for_receive_error) for a
    /// failure here, `NoContent` once the handler has succeeded, and
    /// `InternalServerError` when it has failed.
    ///
    /// Probe parsing is best-effort. Malformed top-level JSON leaves all
    /// probe-derived fields empty; an invalid captured field clears only that
    /// field. In both cases, [`Envelope::raw`] is preserved.
    ///
    /// The body must be `application/json`, which is a setting on the GitHub
    /// webhook; anything else is [`ReceiveError::UnsupportedContentType`]. The
    /// other setting, `application/x-www-form-urlencoded`, wraps the JSON in a
    /// `payload` form parameter and signs the form body, which would make
    /// [`Envelope::raw`] the signed input but no longer the payload every
    /// decode reads. The crate docs record this under
    /// [Deliberately left out](crate#deliberately-left-out).
    ///
    /// # What the receiver adds
    ///
    /// `WebhookReceiver` (`http` feature) does three things around this call
    /// that a transport built directly on it must do for itself, or decide
    /// to go without:
    ///
    /// - **Header-only rejection before reading the body.** The receiver
    ///   refuses a request with no `X-Hub-Signature-256` header before it
    ///   reads a byte of the body, so unsigned traffic never occupies
    ///   memory. This function takes the body already read; a transport
    ///   that streams checks the header is present
    ///   ([`header::SIGNATURE`](crate::header::SIGNATURE)) before buffering,
    ///   and answers its absence as `for_receive_error` answers
    ///   [`VerifyError::MissingSignature`], the error this function would
    ///   have returned.
    /// - **The body limit.** The receiver stops reading at its configured
    ///   limit ([`DEFAULT_BODY_LIMIT`](crate::DEFAULT_BODY_LIMIT), GitHub's
    ///   25 MiB cap) and answers as `for_receive_error` answers
    ///   [`ReceiveError::BodyTooLarge`]. This function verifies whatever it
    ///   is given; a transport bounds the body before calling.
    /// - **The ping short-circuit.** The receiver answers a verified `ping`
    ///   with `NoContent` and passes it to no handler unless configured to.
    ///   This function returns a `ping` like any other envelope, so a
    ///   transport that forwards every envelope forwards pings too unless it
    ///   checks [`EventMeta::kind`] for [`EventKind::Ping`] first.
    ///
    /// # Errors
    ///
    /// Returns an authentication error first, followed by content-type and
    /// required-header errors for an authenticated request.
    pub fn from_signed(
        verifier: &Verifier,
        headers: &HeaderView<'_>,
        body: Bytes,
    ) -> Result<Self, ReceiveError> {
        let signature = headers.require_signature()?;
        verifier.verify(signature, &body)?;

        if !headers
            .content_type
            .as_deref()
            .is_some_and(is_json_content_type)
        {
            return Err(ReceiveError::UnsupportedContentType);
        }

        let delivery_id = required_header(headers.delivery_id.as_deref(), header::DELIVERY_ID)?;
        let event_name = required_header(headers.event_name.as_deref(), header::EVENT_NAME)?;
        let probe = serde_json::from_slice::<Probe<'_>>(&body).unwrap_or_default();

        let kind = EventKind::from_str(event_name).unwrap_or_else(|never| match never {});
        let mut meta = EventMeta::new(delivery_id, kind);
        meta.action = probe
            .action
            .and_then(parse_probe::<String>)
            .map(|action| Action::from_str(&action).unwrap_or_else(|never| match never {}));
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
        meta.target_type = headers
            .target_type
            .as_deref()
            .map(|value| TargetType::from_str(value).unwrap_or_else(|never| match never {}));
        meta.target_id = headers
            .target_id
            .as_deref()
            .and_then(|value| value.parse().ok());

        Ok(Self { meta, raw: body })
    }

    /// Authenticates and constructs an envelope from standard HTTP headers.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Envelope::from_signed`].
    #[cfg(feature = "http")]
    pub fn from_signed_headers(
        verifier: &Verifier,
        headers: &http::HeaderMap,
        body: Bytes,
    ) -> Result<Self, ReceiveError> {
        Self::from_signed(verifier, &HeaderView::from(headers), body)
    }

    /// Decodes the exact payload into a caller-defined view, checking nothing
    /// about the kind.
    ///
    /// `T` is any serde type and nothing ties it to the envelope's kind, so a
    /// view over fields several kinds share (the sender's `type`, say) decodes
    /// from an envelope of any kind. For a view bound to one kind, implement
    /// [`Payload`] and call [`Envelope::decode_payload`], which refuses an
    /// envelope of another kind before decoding.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::Json`] when the payload does not fit `T`.
    pub fn decode<T: serde::de::DeserializeOwned>(&self) -> Result<T, DecodeError> {
        serde_json::from_slice(&self.raw).map_err(DecodeError::Json)
    }

    /// Decodes the payload as `P` after checking that the envelope is of
    /// `P`'s kind.
    ///
    /// This is the decode behind
    /// [`PayloadHandler::into_webhook_handler`](crate::PayloadHandler::into_webhook_handler),
    /// for calling by hand from a [`WebhookHandler`](crate::WebhookHandler)
    /// that matches on [`EventMeta::kind`] itself. The kind check reports a
    /// wrong payload type at the kind, not as a missing field somewhere in
    /// the JSON:
    ///
    /// ```
    /// use octoevents::{Bytes, DecodeError, Envelope, EventKind, EventMeta};
    ///
    /// #[derive(serde::Deserialize)]
    /// struct IssueNumber { issue: Numbered }
    /// #[derive(serde::Deserialize)]
    /// struct Numbered { number: u64 }
    /// octoevents::impl_payload!(IssueNumber => EventKind::Issues);
    ///
    /// let envelope = Envelope {
    ///     meta: EventMeta::new("delivery", EventKind::PullRequest),
    ///     raw: Bytes::from_static(br#"{"issue":{"number":7}}"#),
    /// };
    ///
    /// // The bytes would fit the view; the kind is what is wrong.
    /// assert!(matches!(
    ///     envelope.decode_payload::<IssueNumber>(),
    ///     Err(DecodeError::KindMismatch {
    ///         expected: EventKind::Issues,
    ///         actual: EventKind::PullRequest,
    ///     })
    /// ));
    /// ```
    ///
    /// To decode a view that is not bound to a kind, call [`Envelope::decode`].
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::KindMismatch`] when [`EventMeta::kind`] is not
    /// [`P::KIND`](Payload::KIND), and [`DecodeError::Json`] when the payload
    /// does not fit `P`.
    pub fn decode_payload<P: Payload>(&self) -> Result<P, DecodeError> {
        if self.meta.kind != P::KIND {
            return Err(DecodeError::KindMismatch {
                expected: P::KIND,
                actual: self.meta.kind.clone(),
            });
        }
        self.decode()
    }
}

/// A failure while receiving a webhook.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Error)]
#[non_exhaustive]
pub enum ReceiveError {
    /// Authentication failed.
    #[error(transparent)]
    Verify(#[from] VerifyError),
    /// A required delivery header was absent or empty.
    #[error("missing {0} header")]
    MissingHeader(&'static str),
    /// The request was not configured as JSON.
    #[error(
        "unsupported content type; configure the GitHub webhook content type as application/json"
    )]
    UnsupportedContentType,
    /// The transport stopped reading after the configured limit.
    #[error("webhook body exceeds the configured {limit}-byte limit")]
    BodyTooLarge {
        /// The configured maximum body size.
        limit: usize,
    },
}

/// Why an envelope's payload could not be decoded.
///
/// The one error type of every decode path: [`Envelope::decode`],
/// [`Envelope::decode_payload`], and `Envelope::decode_event` (`octocrab`
/// feature) return it, and the typed handler adapters carry it as
/// [`HandleError::Decode`](crate::HandleError::Decode). A single
/// `From<DecodeError>` impl is therefore the only conversion of a decode
/// failure an application error needs, whichever path decoded.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DecodeError {
    /// The envelope is of a kind the payload type does not cover.
    #[error("expected a {expected} event, received {actual}")]
    KindMismatch {
        /// The kind the payload type declares.
        expected: EventKind,
        /// The kind of the envelope that arrived.
        actual: EventKind,
    },
    /// The payload did not decode into the expected type.
    #[error("payload could not be decoded")]
    Json(#[source] serde_json::Error),
}

fn required_header<'a>(
    value: Option<&'a str>,
    name: &'static str,
) -> Result<&'a str, ReceiveError> {
    value
        .filter(|value| !value.is_empty())
        .ok_or(ReceiveError::MissingHeader(name))
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
mod tests {
    use std::str::FromStr as _;

    use bytes::Bytes;
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;

    use super::{
        DecodeError, Envelope, EventMeta, HeaderView, ReceiveError, RepositoryRef, TargetType,
    };
    use crate::{Action, EventKind, Secret, Verifier, VerifyError, header, test_support};

    const BODY: &[u8] = br#"{
        "action":"opened",
        "installation":{"id":42},
        "repository":{"id":1,"name":"repo","full_name":"octo/repo","owner":{"login":"octo"}},
        "organization":{"login":"github"},
        "sender":{"login":"monalisa"}
    }"#;

    fn signature(secret: &[u8], body: &[u8]) -> String {
        use std::fmt::Write as _;

        let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
        mac.update(body);
        let tag = mac.finalize().into_bytes();
        let mut out = String::from("sha256=");
        for byte in tag {
            write!(out, "{byte:02x}").unwrap();
        }
        out
    }

    fn verifier() -> Verifier {
        Verifier::new(Secret::new("secret"))
    }

    fn headers(signature: &str) -> HeaderView<'_> {
        HeaderView::new()
            .signature(signature)
            .delivery_id("delivery")
            .event_name("pull_request")
            .content_type("application/json; charset=utf-8")
            .target_type("repository")
            .target_id("7")
    }

    /// The top-level keys of a serialized envelope, sorted for comparison.
    fn sorted_keys(value: &serde_json::Value) -> Vec<&str> {
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        keys
    }

    #[test]
    fn verifies_then_extracts_the_metadata() {
        let verifier = verifier();
        let signature = signature(b"secret", BODY);

        let envelope =
            Envelope::from_signed(&verifier, &headers(&signature), Bytes::from_static(BODY))
                .unwrap();

        let meta = &envelope.meta;
        assert_eq!(meta.delivery_id, "delivery");
        assert_eq!(meta.kind, EventKind::PullRequest);
        assert_eq!(meta.action, Some(Action::Opened));
        assert_eq!(meta.installation_id, Some(42));
        assert_eq!(
            meta.repository,
            Some(RepositoryRef::new(1, "repo", "octo/repo", "octo"))
        );
        assert_eq!(meta.organization.as_deref(), Some("github"));
        assert_eq!(meta.sender.as_deref(), Some("monalisa"));
        assert_eq!(meta.target_type, Some(TargetType::Repository));
        assert_eq!(meta.target_id, Some(7));
        assert_eq!(envelope.raw, Bytes::from_static(BODY));
    }

    #[test]
    fn invalid_json_is_preserved_without_failing_the_envelope() {
        let body = Bytes::from_static(b"not json");
        let signature = signature(b"secret", &body);

        let envelope =
            Envelope::from_signed(&verifier(), &headers(&signature), body.clone()).unwrap();

        let mut expected = EventMeta::new("delivery", EventKind::PullRequest);
        expected.target_type = Some(TargetType::Repository);
        expected.target_id = Some(7);
        assert_eq!(envelope.meta, expected);
        assert_eq!(envelope.raw, body);
    }

    #[test]
    fn a_synthetic_envelope_is_built_from_the_metadata_constructor() {
        let mut meta = EventMeta::new("synthetic", EventKind::Issues);
        meta.action = Some(Action::Opened);
        meta.installation_id = Some(42);

        let envelope = Envelope {
            meta,
            raw: Bytes::from_static(br#"{"action":"opened"}"#),
        };

        // The constructor stores what it was given and leaves the rest empty.
        assert_eq!(envelope.meta.delivery_id, "synthetic");
        assert_eq!(envelope.meta.kind, EventKind::Issues);
        assert_eq!(envelope.meta.repository, None);
        assert_eq!(envelope.meta.organization, None);
        assert_eq!(envelope.meta.sender, None);
        assert_eq!(envelope.meta.target_type, None);
        assert_eq!(envelope.meta.target_id, None);
    }

    #[test]
    fn a_repository_ref_is_built_from_its_constructor() {
        let repository = RepositoryRef::new(1, "repo", "octo/repo", "octo");

        assert_eq!(repository.id, 1);
        assert_eq!(repository.name, "repo");
        assert_eq!(repository.full_name, "octo/repo");
        assert_eq!(repository.owner, "octo");
    }

    #[test]
    fn an_unknown_target_type_keeps_its_wire_value() {
        // Matches `EventKind::Unknown` and `Action::Unknown`: a value this
        // version does not know is carried verbatim, not dropped.
        let target_type = TargetType::from_str("enterprise").unwrap();

        assert_eq!(target_type, TargetType::Unknown("enterprise".to_owned()));
        assert_eq!(target_type.as_str(), "enterprise");
        assert_eq!(
            serde_json::to_string(&target_type).unwrap(),
            r#""enterprise""#
        );
        assert_eq!(
            serde_json::from_str::<TargetType>(r#""enterprise""#).unwrap(),
            target_type
        );
    }

    #[test]
    fn serializes_the_metadata_flat_beside_the_raw_bytes() {
        let signature = signature(b"secret", BODY);
        let envelope =
            Envelope::from_signed(&verifier(), &headers(&signature), Bytes::from_static(BODY))
                .unwrap();

        let value = serde_json::to_value(envelope).unwrap();

        // The meta/raw split is a Rust-side composition only: on the wire the
        // metadata sits at the top level with no `meta` nesting.
        assert_eq!(
            sorted_keys(&value),
            [
                "action",
                "delivery_id",
                "installation_id",
                "kind",
                "organization",
                "raw",
                "repository",
                "sender",
                "target_id",
                "target_type",
            ]
        );
        assert_eq!(value["delivery_id"], "delivery");
        assert_eq!(value["kind"], "pull_request");
        assert_eq!(value["installation_id"], 42);
        assert_eq!(value["repository"]["full_name"], "octo/repo");
    }

    #[test]
    fn malformed_probe_fields_do_not_discard_valid_siblings() {
        let body = Bytes::from_static(
            br#"{
                "action":"opened",
                "installation":{"id":42},
                "repository":{"id":1,"name":"repo","owner":{"login":"octo"}},
                "sender":{"login":"monalisa"}
            }"#,
        );
        let signature = signature(b"secret", &body);

        let envelope = Envelope::from_signed(&verifier(), &headers(&signature), body).unwrap();

        assert_eq!(envelope.meta.action, Some(Action::Opened));
        assert_eq!(envelope.meta.installation_id, Some(42));
        assert_eq!(envelope.meta.sender.as_deref(), Some("monalisa"));
        assert_eq!(envelope.meta.repository, None);
    }

    #[test]
    fn unknown_event_and_action_remain_routable() {
        let body = Bytes::from_static(br#"{"action":"brand_new"}"#);
        let signature = signature(b"secret", &body);
        let headers = HeaderView::new()
            .signature(&signature)
            .delivery_id("delivery")
            .event_name("brand_new")
            .content_type("application/json");

        let envelope = Envelope::from_signed(&verifier(), &headers, body).unwrap();

        assert_eq!(envelope.meta.kind, EventKind::Unknown("brand_new".into()));
        assert_eq!(
            envelope.meta.action,
            Some(Action::Unknown("brand_new".into()))
        );
    }

    #[test]
    fn authenticates_before_rejecting_content_type() {
        let headers = HeaderView::new()
            .signature("sha256=0000000000000000000000000000000000000000000000000000000000000000")
            .delivery_id("delivery")
            .event_name("push")
            .content_type("application/x-www-form-urlencoded");

        assert_eq!(
            Envelope::from_signed(&verifier(), &headers, Bytes::new()),
            Err(ReceiveError::Verify(VerifyError::Mismatch))
        );
    }

    #[test]
    fn requires_signature_content_type_and_routing_headers() {
        let no_signature = HeaderView::new()
            .delivery_id("delivery")
            .event_name("push")
            .content_type("application/json");
        assert_eq!(
            Envelope::from_signed(&verifier(), &no_signature, Bytes::new()),
            Err(ReceiveError::Verify(VerifyError::MissingSignature))
        );

        let signature = signature(b"secret", b"");
        let form = HeaderView::new()
            .signature(&signature)
            .delivery_id("delivery")
            .event_name("push")
            .content_type("application/x-www-form-urlencoded");
        assert_eq!(
            Envelope::from_signed(&verifier(), &form, Bytes::new()),
            Err(ReceiveError::UnsupportedContentType)
        );

        let no_delivery = HeaderView::new()
            .signature(&signature)
            .event_name("push")
            .content_type("application/json");
        assert_eq!(
            Envelope::from_signed(&verifier(), &no_delivery, Bytes::new()),
            Err(ReceiveError::MissingHeader(header::DELIVERY_ID))
        );
        assert_eq!(
            ReceiveError::MissingHeader(header::DELIVERY_ID).to_string(),
            "missing x-github-delivery header"
        );
    }

    #[test]
    fn serializes_the_raw_body_as_base64() {
        let verifier = verifier();
        let signature = signature(b"secret", BODY);
        let envelope =
            Envelope::from_signed(&verifier, &headers(&signature), Bytes::from_static(BODY))
                .unwrap();

        let value = serde_json::to_value(envelope).unwrap();

        assert_eq!(
            value["raw"],
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, BODY)
        );
        assert_eq!(value["target_type"], "repository");
    }

    #[test]
    fn survives_a_json_round_trip_to_a_forwarding_target() {
        let signature = signature(b"secret", BODY);
        let envelope =
            Envelope::from_signed(&verifier(), &headers(&signature), Bytes::from_static(BODY))
                .unwrap();

        let forwarded = serde_json::to_string(&envelope).unwrap();
        let received: Envelope = serde_json::from_str(&forwarded).unwrap();

        assert_eq!(received, envelope);
        assert_eq!(received.raw, Bytes::from_static(BODY));
        assert_eq!(received.meta.target_type, Some(TargetType::Repository));
    }

    #[test]
    fn omits_absent_optional_fields_from_the_serialized_envelope() {
        // A forwarded envelope says what it knows and nothing else, so a
        // consumer in another language reads a missing key, not a null.
        let envelope = Envelope {
            meta: EventMeta::new("delivery", EventKind::Push),
            raw: Bytes::from_static(b"{}"),
        };

        let value = serde_json::to_value(envelope).unwrap();

        assert_eq!(sorted_keys(&value), ["delivery_id", "kind", "raw"]);
    }

    #[test]
    fn deserializes_null_and_absent_optional_fields_alike() {
        // Both spellings of "unknown" that a producer might use read back as
        // the same envelope, so a hand-written forwarder need not pick one.
        let with_nulls = r#"{
            "delivery_id": "delivery",
            "kind": "push",
            "action": null,
            "installation_id": null,
            "repository": null,
            "organization": null,
            "sender": null,
            "target_type": null,
            "target_id": null,
            "raw": "e30="
        }"#;
        let without = r#"{"delivery_id": "delivery", "kind": "push", "raw": "e30="}"#;

        let from_nulls: Envelope = serde_json::from_str(with_nulls).unwrap();
        let from_absent: Envelope = serde_json::from_str(without).unwrap();

        assert_eq!(from_nulls, from_absent);
        assert_eq!(
            from_absent,
            Envelope {
                meta: EventMeta::new("delivery", EventKind::Push),
                raw: Bytes::from_static(b"{}"),
            }
        );
    }

    #[test]
    fn requires_delivery_id_kind_and_raw_on_deserialize() {
        // The three fields the docs name as required are the three whose
        // absence is an error; every other field defaults.
        for missing in ["delivery_id", "kind", "raw"] {
            let mut document =
                serde_json::json!({"delivery_id": "delivery", "kind": "push", "raw": "e30="});
            document.as_object_mut().unwrap().remove(missing);

            let error = serde_json::from_value::<Envelope>(document).unwrap_err();

            assert!(
                error.to_string().contains(missing),
                "removing {missing} should name it: {error}"
            );
        }
    }

    #[test]
    fn ignores_unknown_fields_on_deserialize() {
        // A producer on a newer version, or one that annotates the document
        // for its own transport, does not break an older consumer.
        let document = r#"{
            "delivery_id": "delivery",
            "kind": "push",
            "raw": "e30=",
            "enterprise": {"id": 1},
            "received_at": "2026-09-06T00:00:00Z"
        }"#;

        let envelope: Envelope = serde_json::from_str(document).unwrap();

        assert_eq!(envelope.meta, EventMeta::new("delivery", EventKind::Push));
        assert_eq!(envelope.raw, Bytes::from_static(b"{}"));
    }

    #[test]
    fn preserves_multibyte_and_escaped_payload_bytes_exactly() {
        // Signing happens over bytes as sent: a re-encode would change both the
        // escape form and the MAC, so assert the exact body survives.
        const UNICODE_BODY: &[u8] =
            "{\"action\":\"opened\",\"zen\":\"⚡ \\u00e9 caf\u{e9} 🐙\"}".as_bytes();

        let signature = signature(b"secret", UNICODE_BODY);
        let headers = HeaderView::new()
            .signature(&signature)
            .delivery_id("delivery")
            .event_name("pull_request")
            .content_type("application/json");

        let envelope =
            Envelope::from_signed(&verifier(), &headers, Bytes::from_static(UNICODE_BODY)).unwrap();

        assert_eq!(envelope.raw.as_ref(), UNICODE_BODY);
        assert_eq!(envelope.meta.action, Some(Action::Opened));

        let decoded: serde_json::Value = envelope.decode().unwrap();
        assert_eq!(decoded["zen"], "⚡ é café 🐙");
    }

    #[derive(Debug, serde::Deserialize)]
    struct IssueNumber {
        issue: Numbered,
    }

    #[derive(Debug, serde::Deserialize)]
    struct Numbered {
        number: u64,
    }

    crate::impl_payload!(IssueNumber => EventKind::Issues);

    #[test]
    fn decode_payload_refuses_an_envelope_of_another_kind_at_the_kind() {
        // The bytes would decode into the view; the kind is what is wrong, and
        // that is what the error names rather than a missing field.
        let envelope = test_support::envelope(EventKind::PullRequest, br#"{"issue":{"number":7}}"#);

        let error = envelope.decode_payload::<IssueNumber>().unwrap_err();

        assert!(matches!(
            error,
            DecodeError::KindMismatch {
                expected: EventKind::Issues,
                actual: EventKind::PullRequest,
            }
        ));
    }

    #[test]
    fn decode_payload_decodes_an_envelope_of_the_payloads_kind() {
        let envelope = test_support::envelope(EventKind::Issues, br#"{"issue":{"number":7}}"#);

        let payload = envelope.decode_payload::<IssueNumber>().unwrap();

        assert_eq!(payload.issue.number, 7);
    }

    #[test]
    fn decode_payload_reports_a_payload_that_does_not_fit_as_json() {
        // Right kind, wrong shape: the kind check passed, serde did not.
        let envelope = test_support::envelope(EventKind::Issues, br#"{"issue":{}}"#);

        let error = envelope.decode_payload::<IssueNumber>().unwrap_err();

        assert!(matches!(error, DecodeError::Json(_)));
    }

    #[test]
    fn decode_reports_a_payload_that_does_not_fit_with_the_shared_decode_error() {
        let envelope = test_support::envelope(EventKind::Issues, br#"{"issue":{}}"#);

        let error = envelope.decode::<IssueNumber>().unwrap_err();

        assert!(matches!(error, DecodeError::Json(_)));
    }

    #[test]
    fn decode_does_not_check_the_kind() {
        // The same view `decode_payload` refuses under this kind decodes here:
        // `decode` ties nothing to the kind.
        let envelope = test_support::envelope(EventKind::PullRequest, br#"{"issue":{"number":7}}"#);

        let payload = envelope.decode::<IssueNumber>().unwrap();

        assert_eq!(payload.issue.number, 7);
    }

    #[cfg(feature = "http")]
    #[test]
    fn constructs_from_an_http_header_map() {
        let signature = signature(b"secret", BODY);
        let mut map = http::HeaderMap::new();
        map.insert(header::SIGNATURE, signature.parse().unwrap());
        map.insert(header::DELIVERY_ID, "delivery".parse().unwrap());
        map.insert(header::EVENT_NAME, "pull_request".parse().unwrap());
        map.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        map.insert(header::TARGET_TYPE, "integration".parse().unwrap());
        map.insert(header::TARGET_ID, "12345".parse().unwrap());

        let envelope =
            Envelope::from_signed_headers(&verifier(), &map, Bytes::from_static(BODY)).unwrap();

        assert_eq!(envelope.meta.delivery_id, "delivery");
        assert_eq!(envelope.meta.kind, EventKind::PullRequest);
        assert_eq!(envelope.meta.action, Some(Action::Opened));
        assert_eq!(envelope.meta.target_type, Some(TargetType::Integration));
        assert_eq!(envelope.meta.target_id, Some(12345));
        assert_eq!(envelope.meta.installation_id, Some(42));
    }

    #[cfg(feature = "http")]
    #[test]
    fn rejects_a_non_ascii_signature_header_as_malformed() {
        let mut map = http::HeaderMap::new();
        map.insert(
            "x-hub-signature-256",
            http::HeaderValue::from_bytes(b"sha256=\xff\xfe").unwrap(),
        );
        map.insert("x-github-delivery", "delivery".parse().unwrap());
        map.insert("x-github-event", "push".parse().unwrap());
        map.insert("content-type", "application/json".parse().unwrap());

        assert_eq!(
            Envelope::from_signed_headers(&verifier(), &map, Bytes::new()),
            Err(ReceiveError::Verify(VerifyError::MalformedSignature))
        );
    }

    #[test]
    fn header_debug_output_redacts_the_signature() {
        let headers = HeaderView::new()
            .signature("sha256=secret-value")
            .delivery_id("delivery")
            .event_name("push")
            .content_type("application/json");

        let debug = format!("{headers:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("secret-value"));
    }
}
