use std::{borrow::Cow, fmt};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use bytes::Bytes;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::value::RawValue;
use thiserror::Error;

use crate::{Action, EventKind, TargetType, Verifier, VerifyError, header};

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

/// The headers needed to authenticate and route a GitHub webhook.
///
/// Use [`From`] with an `http::HeaderMap` when the `http` feature is enabled.
/// Otherwise [`HeaderView::from_lookup`] asks your transport's map for each
/// header by the names in [`header`](crate::header), in one call; the
/// setters build a view one header at a time, for a test or a transport that
/// has the values in hand.
///
/// Header-name case is the caller's concern. The view holds values, and
/// which header a value came from is fixed by the setter that received it,
/// so it never compares a name. `http::HeaderMap` matches names
/// case-insensitively; a plain string map does not, and the casing its keys
/// hold depends on the hop that filled it: HTTP/1.1 carries names as the
/// sender wrote them (GitHub writes `X-GitHub-Delivery`), HTTP/2 and HTTP/3
/// lowercase them, and a gateway in between may do either. A transport whose
/// map keeps the sender's casing lowercases its keys, or compares
/// case-insensitively, before looking up the lowercase constants. Skip that
/// step and every delivery is 401 with the secret correct: a map keyed
/// `X-Hub-Signature-256` answers `None` for `x-hub-signature-256`, so the
/// view has no signature and [`Envelope::from_signed`] refuses the request as
/// [`VerifyError::MissingSignature`] before it reads anything else.
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

    /// Builds a view by asking `lookup` for each header this crate reads.
    ///
    /// The one-call constructor for a transport that receives its headers as
    /// a string map. `lookup` is called once per header with the constant
    /// from [`header`](crate::header) that names it, always lowercase
    /// (`x-github-delivery`, not `X-GitHub-Delivery`); a header it answers
    /// `None` for is left unset, exactly as if its setter had not been called.
    /// A value may be borrowed from the map or owned, as with the setters.
    ///
    /// Matching the case of your map's keys is your concern: the lookup
    /// receives the lowercase name and the view compares nothing itself. A
    /// map that kept the sender's casing (HTTP/1.1 does) is lowercased
    /// first, or asked case-insensitively; see the type docs for which hops
    /// do what. Forgotten, the mismatch is not a missing delivery ID or event
    /// name but a 401 on every delivery, the secret notwithstanding: the map
    /// answers `None` for `x-hub-signature-256` as for every other name, the
    /// view has no signature, and [`Envelope::from_signed`] reports
    /// [`VerifyError::MissingSignature`] before it looks at the rest.
    ///
    /// A string map cannot hold a header whose bytes are not a string, so
    /// this constructor never marks the signature malformed the way the
    /// `From<&http::HeaderMap>` conversion (`http` feature) does for a
    /// header value that is not visible ASCII. A signature that is present
    /// but not `sha256=` followed by 64 hexadecimal characters is still
    /// [`VerifyError::MalformedSignature`], from the verifier.
    ///
    /// ```
    /// use std::collections::HashMap;
    ///
    /// use octoevents::HeaderView;
    ///
    /// // What an HTTP/1.1 hop hands over: names as GitHub wrote them.
    /// let received: HashMap<String, String> = [
    ///     ("X-Hub-Signature-256", "sha256=..."),
    ///     ("X-GitHub-Delivery", "72d3162e-cc78-11e3-81ab-4c9367dc0958"),
    ///     ("X-GitHub-Event", "pull_request"),
    ///     ("Content-Type", "application/json"),
    /// ]
    /// .into_iter()
    /// .map(|(name, value)| (name.to_ascii_lowercase(), value.to_owned()))
    /// .collect();
    ///
    /// let headers = HeaderView::from_lookup(|name| received.get(name).map(String::as_str));
    /// # let _ = headers;
    /// ```
    #[must_use]
    pub fn from_lookup<S>(mut lookup: impl FnMut(&str) -> Option<S>) -> Self
    where
        S: Into<Cow<'a, str>>,
    {
        Self {
            signature: lookup(header::SIGNATURE).map(Into::into),
            delivery_id: lookup(header::DELIVERY_ID).map(Into::into),
            event_name: lookup(header::EVENT_NAME).map(Into::into),
            content_type: lookup(header::CONTENT_TYPE).map(Into::into),
            target_type: lookup(header::TARGET_TYPE).map(Into::into),
            target_id: lookup(header::TARGET_ID).map(Into::into),
            malformed_signature: false,
        }
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
        let mut view =
            Self::from_lookup(|name| headers.get(name).and_then(|value| value.to_str().ok()));
        // A header value that is not visible ASCII has no `str` to look up,
        // so the lookup left the signature unset; this is what tells a
        // malformed one apart from an absent one.
        view.malformed_signature = headers
            .get(header::SIGNATURE)
            .is_some_and(|value| value.to_str().is_err());
        view
    }
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
    /// This is the sans-I/O entry point: the `http`-feature receiver is built
    /// on it, and a transport that has no `http::Request` (a serverless
    /// runtime handing over a header map and a body string, say) calls it
    /// directly with a [`HeaderView`] and the body as [`Bytes`]. Answer with
    /// the receiver's contract, as [`ResponseStatus`](crate::ResponseStatus):
    /// [`for_receive_error`](crate::ResponseStatus::for_receive_error) for a
    /// failure here, `NoContent` once the handler has succeeded, and
    /// `InternalServerError` when it has failed.
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
    /// `WebhookReceiver` (`http` feature) does three things around this call
    /// that a transport built directly on it must do for itself, or decide
    /// to go without. This is the receiver's sequence for a transport that is
    /// handed a string map and the body; each of the three returns the
    /// status the receiver would, and the handler runs only once all three
    /// have passed:
    ///
    /// ```
    /// use std::collections::HashMap;
    ///
    /// use octoevents::{
    ///     Bytes, DEFAULT_BODY_LIMIT, Envelope, EventKind, Handler, HeaderView, ReceiveError,
    ///     ResponseStatus, Verifier, VerifyError, header,
    /// };
    ///
    /// async fn receive<H: Handler<Envelope>>(
    ///     verifier: &Verifier,
    ///     received: &HashMap<String, String>,
    ///     body: Bytes,
    ///     handler: &H,
    /// ) -> ResponseStatus {
    ///     // Header-only rejection: an unsigned request is 401 before the
    ///     // body is read, so it never occupies memory. Decidable from the
    ///     // headers, so a transport that streams runs it before buffering;
    ///     // `from_signed` reaches the same answer for one that does not.
    ///     if received.get(header::SIGNATURE).is_none() {
    ///         let error = ReceiveError::from(VerifyError::MissingSignature);
    ///         return ResponseStatus::for_receive_error(&error);
    ///     }
    ///
    ///     // The body limit: 413 past GitHub's 25 MiB cap. The receiver stops
    ///     // reading at the limit; a transport that streams does the same,
    ///     // and one handed the body already read checks its length. A read
    ///     // that fails partway is `ReceiveError::BodyRead`, 400, its text the
    ///     // crate's and the transport's error one `source()` beneath, as a
    ///     // `BodyError`; a transport handed the bytes never sees one.
    ///     if body.len() > DEFAULT_BODY_LIMIT {
    ///         let error = ReceiveError::BodyTooLarge { limit: DEFAULT_BODY_LIMIT };
    ///         return ResponseStatus::for_receive_error(&error);
    ///     }
    ///
    ///     let headers = HeaderView::from_lookup(|name| received.get(name).map(String::as_str));
    ///     let envelope = match Envelope::from_signed(verifier, &headers, body) {
    ///         Ok(envelope) => envelope,
    ///         Err(error) => return ResponseStatus::for_receive_error(&error),
    ///     };
    ///
    ///     // The ping short-circuit: a verified `ping` is 204 and reaches no
    ///     // handler, unless the receiver was built with `handle_ping(true)`.
    ///     // After `from_signed`, so an unsigned ping is still 401.
    ///     if matches!(envelope.meta.kind, EventKind::Ping) {
    ///         return ResponseStatus::NoContent;
    ///     }
    ///
    ///     match handler.handle(envelope).await {
    ///         Ok(()) => ResponseStatus::NoContent,
    ///         Err(_) => ResponseStatus::InternalServerError,
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

        let mut envelope = Self::probed(delivery_id, EventKind::from(event_name), body);
        envelope.meta.target_type = headers.target_type.as_deref().map(TargetType::from);
        envelope.meta.target_id = headers
            .target_id
            .as_deref()
            .and_then(|value| value.parse().ok());

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
/// naming the step that refused the request: [`Verify`](Self::Verify), when
/// the signature header is absent, malformed or does not match;
/// [`MissingHeader`](Self::MissingHeader), when a required delivery header is
/// absent or empty; [`UnsupportedContentType`](Self::UnsupportedContentType),
/// when the webhook is not configured as JSON; [`BodyRead`](Self::BodyRead),
/// when the transport failed while the body was being read; and
/// [`BodyTooLarge`](Self::BodyTooLarge), when the body ran past the
/// configured limit.
/// [`ResponseStatus::for_receive_error`](crate::ResponseStatus::for_receive_error)
/// maps each to the status the receiver answers with, so every failure
/// before a handler has an error value and the same value selects the
/// status. The enum is `#[non_exhaustive]`, so a `match` over it keeps a
/// wildcard arm.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Error)]
#[non_exhaustive]
pub enum ReceiveError {
    /// Authentication failed.
    #[error(transparent)]
    Verify(#[from] VerifyError),
    /// A required delivery header was absent or empty.
    #[error("missing {name} header")]
    MissingHeader {
        /// The header's lowercase name, one of the constants in
        /// [`header`](crate::header).
        name: &'static str,
    },
    /// The request was not configured as JSON.
    #[error(
        "unsupported content type; configure the GitHub webhook content type as application/json"
    )]
    UnsupportedContentType,
    /// The transport failed while the body was being read.
    ///
    /// Produced by `WebhookReceiver` (`http` feature) when a body frame is an
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

fn required_header<'a>(
    value: Option<&'a str>,
    name: &'static str,
) -> Result<&'a str, ReceiveError> {
    value
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
