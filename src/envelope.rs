use std::{borrow::Cow, fmt, str::FromStr};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use bytes::Bytes;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::value::RawValue;
use thiserror::Error;

use crate::{Action, EventKind, Payload, Verifier, VerifyError};

#[cfg(feature = "http")]
const SIGNATURE_HEADER: &str = "x-hub-signature-256";
const DELIVERY_HEADER: &str = "x-github-delivery";
const EVENT_HEADER: &str = "x-github-event";
#[cfg(feature = "http")]
const CONTENT_TYPE_HEADER: &str = "content-type";
#[cfg(feature = "http")]
const TARGET_TYPE_HEADER: &str = "x-github-hook-installation-target-type";
#[cfg(feature = "http")]
const TARGET_ID_HEADER: &str = "x-github-hook-installation-target-id";

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
    #[serde(default)]
    pub action: Option<Action>,
    /// The GitHub App installation ID, when present.
    #[serde(default)]
    pub installation_id: Option<u64>,
    /// A compact repository reference, when present.
    #[serde(default)]
    pub repository: Option<RepositoryRef>,
    /// The organization login, when present.
    #[serde(default)]
    pub organization: Option<String>,
    /// The sender login, when present.
    #[serde(default)]
    pub sender: Option<String>,
    /// The webhook installation target type.
    #[serde(default)]
    pub target_type: Option<TargetType>,
    /// The webhook installation target ID.
    #[serde(default)]
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
/// from [`RepositoryRef::default`] and assign the fields you need; the crate itself
/// never produces a defaulted value.
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
    /// A target type introduced after this crate version.
    Other(String),
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
            Self::Other(value) => value,
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
            value => Self::Other(value.to_owned()),
        })
    }
}

/// The headers needed to authenticate and route a GitHub webhook.
///
/// Use [`From`] with an `http::HeaderMap` when the `http` feature is enabled.
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
            signature: value(headers, SIGNATURE_HEADER),
            delivery_id: value(headers, DELIVERY_HEADER),
            event_name: value(headers, EVENT_HEADER),
            content_type: value(headers, CONTENT_TYPE_HEADER),
            target_type: value(headers, TARGET_TYPE_HEADER),
            target_id: value(headers, TARGET_ID_HEADER),
            malformed_signature: headers
                .get(SIGNATURE_HEADER)
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
/// a trusted internal transport forwarded (see the [`Deserialize`] impl). A
/// value obtained that way carries no authentication claim; only one returned
/// by [`Envelope::from_signed`] does. Extensibility lives in [`EventMeta`],
/// which is `#[non_exhaustive]` and built with [`EventMeta::new`].
///
/// On the wire the metadata is flattened beside `raw`, so a serialized
/// envelope is one flat JSON object with no `meta` nesting. Envelopes
/// serialized before the metadata split nested four fields under `common`;
/// those fields deserialize as `None` from such a document.
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
    /// Probe parsing is best-effort. Malformed top-level JSON leaves all
    /// probe-derived fields empty; an invalid captured field clears only that
    /// field. In both cases, [`Envelope::raw`] is preserved.
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

        let delivery_id = required_header(headers.delivery_id.as_deref(), DELIVERY_HEADER)?;
        let event_name = required_header(headers.event_name.as_deref(), EVENT_HEADER)?;
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
    pub fn from_signed_parts(
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
    /// use bytes::Bytes;
    /// use octoevents::{DecodeError, Envelope, EventKind, EventMeta};
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
    use bytes::Bytes;
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;

    use super::{
        DecodeError, Envelope, EventMeta, HeaderView, ReceiveError, RepositoryRef, TargetType,
    };
    use crate::{Action, EventKind, Secret, Verifier, VerifyError, test_support};

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
            Some(RepositoryRef {
                id: 1,
                name: "repo".into(),
                full_name: "octo/repo".into(),
                owner: "octo".into(),
            })
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
    fn serializes_the_metadata_flat_beside_the_raw_bytes() {
        let signature = signature(b"secret", BODY);
        let envelope =
            Envelope::from_signed(&verifier(), &headers(&signature), Bytes::from_static(BODY))
                .unwrap();

        let value = serde_json::to_value(envelope).unwrap();
        let object = value.as_object().unwrap();

        // The meta/raw split is a Rust-side composition only: on the wire the
        // metadata sits at the top level with no `meta` nesting.
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
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
            Err(ReceiveError::MissingHeader("x-github-delivery"))
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
        map.insert("x-hub-signature-256", signature.parse().unwrap());
        map.insert("x-github-delivery", "delivery".parse().unwrap());
        map.insert("x-github-event", "pull_request".parse().unwrap());
        map.insert("content-type", "application/json".parse().unwrap());
        map.insert(
            "x-github-hook-installation-target-type",
            "integration".parse().unwrap(),
        );
        map.insert(
            "x-github-hook-installation-target-id",
            "12345".parse().unwrap(),
        );

        let envelope =
            Envelope::from_signed_parts(&verifier(), &map, Bytes::from_static(BODY)).unwrap();

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
            Envelope::from_signed_parts(&verifier(), &map, Bytes::new()),
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
