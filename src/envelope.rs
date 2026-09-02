use std::{borrow::Cow, fmt, str::FromStr};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use bytes::Bytes;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::value::RawValue;
use thiserror::Error;

use crate::{Action, EventKind, Verifier, VerifyError};

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

/// The routing-oriented subset shared by GitHub webhook payloads.
///
/// This view is produced by the crate and only read by consumers, so it is
/// `#[non_exhaustive]`: GitHub can add a stable routing field (an enterprise
/// reference, for example) without that becoming a breaking change here.
/// Build one in tests from [`Common::default`] and assign the fields you need.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Common {
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
}

/// A compact repository reference extracted without parsing a full payload model.
///
/// `#[non_exhaustive]` for the same reason as [`Common`]. Build one in tests
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
    delivery_id: Option<Cow<'a, str>>,
    event_name: Option<Cow<'a, str>>,
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

    /// The signature-header failure `Envelope::from_signed` would report,
    /// decidable from the headers alone. The receiver uses it to refuse an
    /// unsigned request before reading the body.
    #[cfg(feature = "http")]
    pub(crate) fn signature_failure(&self) -> Option<VerifyError> {
        if self.malformed_signature {
            Some(VerifyError::MalformedSignature)
        } else if self.signature.is_none() {
            Some(VerifyError::MissingSignature)
        } else {
            None
        }
    }

    #[cfg(all(feature = "tracing", feature = "http"))]
    pub(crate) fn recorded_delivery_id(&self) -> Option<&str> {
        self.delivery_id.as_deref()
    }

    #[cfg(all(feature = "tracing", feature = "http"))]
    pub(crate) fn recorded_event_name(&self) -> Option<&str> {
        self.event_name.as_deref()
    }
}

#[cfg(feature = "http")]
#[cfg_attr(docsrs, doc(cfg(feature = "http")))]
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
/// [`Envelope::from_signed`] is the only path in this crate that turns an
/// untrusted request into an envelope, and it authenticates before it extracts.
/// The fields are nevertheless public and the struct is deliberately *not*
/// `#[non_exhaustive]`: consumers must be able to build synthetic envelopes to
/// unit-test handlers and dispatchers without HTTP, and to reconstruct one that
/// a trusted internal transport forwarded (see the [`Deserialize`] impl). A
/// value obtained that way carries no authentication claim; only one returned
/// by [`Envelope::from_signed`] does. The cost of that choice is that adding a
/// field here is a breaking change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    /// The `X-GitHub-Delivery` value. Use it as a downstream idempotency key.
    pub delivery_id: String,
    /// The event kind parsed from `X-GitHub-Event`.
    pub kind: EventKind,
    /// The payload's top-level action, when available.
    pub action: Option<Action>,
    /// Stable routing fields partially extracted from the payload.
    pub common: Common,
    /// The webhook installation target type.
    pub target_type: Option<TargetType>,
    /// The webhook installation target ID.
    pub target_id: Option<u64>,
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
        if headers.malformed_signature {
            return Err(VerifyError::MalformedSignature.into());
        }
        let signature = headers
            .signature
            .as_deref()
            .ok_or(VerifyError::MissingSignature)?;
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
        let action = probe
            .action
            .and_then(parse_probe::<String>)
            .map(|action| Action::from_str(&action).unwrap_or_else(|never| match never {}));

        Ok(Self {
            delivery_id: delivery_id.to_owned(),
            kind,
            action,
            common: Common {
                installation_id: probe
                    .installation
                    .and_then(parse_probe::<IdOnly>)
                    .map(|installation| installation.id),
                repository: probe
                    .repository
                    .and_then(parse_probe::<RepoProbe>)
                    .map(RepositoryRef::from),
                organization: probe
                    .organization
                    .and_then(parse_probe::<LoginOnly>)
                    .map(|organization| organization.login),
                sender: probe
                    .sender
                    .and_then(parse_probe::<LoginOnly>)
                    .map(|sender| sender.login),
            },
            target_type: headers
                .target_type
                .as_deref()
                .map(|value| TargetType::from_str(value).unwrap_or_else(|never| match never {})),
            target_id: headers
                .target_id
                .as_deref()
                .and_then(|value| value.parse().ok()),
            raw: body,
        })
    }

    /// Authenticates and constructs an envelope from standard HTTP headers.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Envelope::from_signed`].
    #[cfg(feature = "http")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http")))]
    pub fn from_signed_parts(
        verifier: &Verifier,
        headers: &http::HeaderMap,
        body: Bytes,
    ) -> Result<Self, ReceiveError> {
        Self::from_signed(verifier, &HeaderView::from(headers), body)
    }

    /// Deserializes the exact payload into a caller-defined view.
    ///
    /// # Errors
    ///
    /// Returns serde's parse error when the payload does not fit `T`.
    pub fn parse<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_slice(&self.raw)
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
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    use super::{Common, Envelope, HeaderView, ReceiveError, RepositoryRef, TargetType};
    use crate::{Action, EventKind, Secret, Verifier, VerifyError};

    const BODY: &[u8] = br#"{
        "action":"opened",
        "installation":{"id":42},
        "repository":{"id":1,"name":"repo","full_name":"octo/repo","owner":{"login":"octo"}},
        "organization":{"login":"github"},
        "sender":{"login":"monalisa"}
    }"#;

    fn signature(secret: &[u8], body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
        mac.update(body);
        format!("sha256={:x}", mac.finalize().into_bytes())
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
    fn verifies_then_extracts_the_common_view() {
        let verifier = verifier();
        let signature = signature(b"secret", BODY);

        let envelope =
            Envelope::from_signed(&verifier, &headers(&signature), Bytes::from_static(BODY))
                .unwrap();

        assert_eq!(envelope.delivery_id, "delivery");
        assert_eq!(envelope.kind, EventKind::PullRequest);
        assert_eq!(envelope.action, Some(Action::Opened));
        assert_eq!(envelope.target_type, Some(TargetType::Repository));
        assert_eq!(envelope.target_id, Some(7));
        assert_eq!(
            envelope.common,
            Common {
                installation_id: Some(42),
                repository: Some(RepositoryRef {
                    id: 1,
                    name: "repo".into(),
                    full_name: "octo/repo".into(),
                    owner: "octo".into(),
                }),
                organization: Some("github".into()),
                sender: Some("monalisa".into()),
            }
        );
        assert_eq!(envelope.raw, Bytes::from_static(BODY));
    }

    #[test]
    fn invalid_json_is_preserved_without_failing_the_envelope() {
        let body = Bytes::from_static(b"not json");
        let signature = signature(b"secret", &body);

        let envelope =
            Envelope::from_signed(&verifier(), &headers(&signature), body.clone()).unwrap();

        assert_eq!(envelope.action, None);
        assert_eq!(envelope.common, Common::default());
        assert_eq!(envelope.raw, body);
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

        assert_eq!(envelope.action, Some(Action::Opened));
        assert_eq!(envelope.common.installation_id, Some(42));
        assert_eq!(envelope.common.sender.as_deref(), Some("monalisa"));
        assert_eq!(envelope.common.repository, None);
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

        assert_eq!(envelope.kind, EventKind::Unknown("brand_new".into()));
        assert_eq!(envelope.action, Some(Action::Unknown("brand_new".into())));
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
        assert_eq!(received.target_type, Some(TargetType::Repository));
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
        assert_eq!(envelope.action, Some(Action::Opened));

        let parsed: serde_json::Value = envelope.parse().unwrap();
        assert_eq!(parsed["zen"], "⚡ é café 🐙");
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

        assert_eq!(envelope.delivery_id, "delivery");
        assert_eq!(envelope.kind, EventKind::PullRequest);
        assert_eq!(envelope.action, Some(Action::Opened));
        assert_eq!(envelope.target_type, Some(TargetType::Integration));
        assert_eq!(envelope.target_id, Some(12345));
        assert_eq!(envelope.common.installation_id, Some(42));
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
