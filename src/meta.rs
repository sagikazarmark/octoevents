use std::{fmt, marker::PhantomData};

use http::HeaderMap;
use serde::{Deserialize, Deserializer, Serialize, de};

use crate::{Action, DecodeError, Envelope, EventKind, ReceiveError, events::string_enum, header};

/// The routing metadata of a delivery: the [`WebhookMeta`] read from its
/// headers, and the fields its payload names every delivery by.
///
/// A handler's input on its own, for one routed by kind and action that
/// decodes no view of its own, and the first half of
/// [`Event<P>`](crate::Event), so the delivery ID and installation ID travel
/// beside a decoded payload without going back to the envelope.
///
/// # Where the fields come from
///
/// Four fields are the [`WebhookMeta`]'s, read from the headers before the
/// body: the delivery ID and the kind, which every delivery carries, and the
/// target type and ID. The other five, the action, installation ID,
/// repository, organization and sender, are read from the payload by
/// [`EventMeta::decode`]. The envelope holds neither: it is what arrived,
/// the header meta and the bytes, and building one reads nothing.
///
/// The [`Dispatcher`](crate::Dispatcher) decodes the meta once per delivery,
/// before its first tier, because it routes by the action and the action is
/// in the payload, not in a header. Every input it builds for that delivery
/// is handed the same meta. The decode is strict: GitHub's payloads always
/// carry these fields in one shape (the action a string; the installation,
/// repository, organization and sender objects with the fields read here),
/// so a payload that does not fit it, or is not JSON at all, is a
/// [`DecodeError`] and fails the delivery at dispatch, visibly and
/// redeliverably, rather than being routed as if it had no action.
///
/// On the receiving path, verification authenticates the payload bytes, not
/// the delivery ID, event name, or target headers. Header-derived fields
/// alone must not authorize security-sensitive actions; use authenticated
/// payload data or independently trusted configuration. Delivery-ID
/// deduplication handles GitHub redelivery, not adversarial replay.
///
/// The one exception is the target, under two conditions a
/// [`VerifierSource`](crate::VerifierSource) must meet and verification
/// cannot see: the source chose the verifier by the target, answering each
/// target with its own secrets only, and no two targets share a secret. A
/// delivery that verified under such a source is the target's it claims.
/// With a single [`Verifier`](crate::Verifier), which ignores the target, the
/// target headers stay claims like the rest.
///
/// The crate produces this view and consumers only read it, so it is
/// `#[non_exhaustive]`: GitHub can add a stable routing field (an enterprise
/// reference, for example) without that becoming a breaking change here.
/// In a test, [`EventMeta::decode`] over an envelope from
/// [`Envelope::new`](crate::Envelope::new) is the meta the dispatcher would
/// hand its handlers for the same header meta and bytes; build a meta by
/// itself with [`EventMeta::new`], for a handler over `EventMeta` alone, and
/// assign the optional fields it reads.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub struct EventMeta {
    /// The `X-GitHub-Delivery` value. Use it as a downstream idempotency key.
    pub delivery_id: String,
    /// The event kind parsed from `X-GitHub-Event`.
    pub kind: EventKind,
    /// The payload's top-level action, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<Action>,
    /// The GitHub App installation ID, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation_id: Option<u64>,
    /// The repository's meta, when the payload carries a `repository`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<RepositoryMeta>,
    /// The organization the webhook fired in, when present: an organization
    /// webhook's, or a GitHub App's installed on one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<AccountMeta>,
    /// The user or app whose act triggered the webhook, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<AccountMeta>,
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
    /// that agrees with a payload, decode it from the envelope with
    /// [`EventMeta::decode`], as the dispatcher does. This constructor is for
    /// a handler over `EventMeta` alone, or a test that wants the meta and
    /// nothing else.
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

    /// Decodes the meta of the delivery in `envelope`: its [`WebhookMeta`]
    /// as it is, beside the action, installation ID, repository,
    /// organization and sender read from its payload.
    ///
    /// What the [`Dispatcher`](crate::Dispatcher) runs once per delivery
    /// before any handler, and hands every input it builds. Public for code
    /// outside it: a policy seam that reads a payload field before calling
    /// [`dispatch`](crate::Dispatcher::dispatch) (to refuse a suspended
    /// installation, say), which then decodes again inside `dispatch`, and a
    /// test that calls
    /// [`FromEnvelope::from_envelope`](crate::FromEnvelope::from_envelope)
    /// with the meta the dispatcher would have passed.
    ///
    /// One scan of the document, keeping those five top-level values and
    /// skipping the rest.
    ///
    /// ```
    /// use octoevents::{Action, Envelope, EventKind, EventMeta, WebhookMeta};
    ///
    /// let envelope = Envelope::new(
    ///     WebhookMeta::new("72d3162e-cc78-11e3-81ab-4c9367dc0958", EventKind::Issues),
    ///     br#"{"action":"opened","installation":{"id":42},"issue":{"number":7}}"#,
    /// );
    ///
    /// let meta = EventMeta::decode(&envelope)?;
    ///
    /// assert_eq!(meta.delivery_id, "72d3162e-cc78-11e3-81ab-4c9367dc0958");
    /// assert_eq!(meta.action, Some(Action::Opened));
    /// assert_eq!(meta.installation_id, Some(42));
    /// # Ok::<(), octoevents::DecodeError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::Json`] when the payload is not a JSON object,
    /// or one of the five fields it names is not in GitHub's shape: an
    /// `action` that is not a string, or an `installation`, `repository`,
    /// `organization` or `sender` that is not an object with the fields
    /// read here. A field that is absent or `null` is `None`, not an error.
    pub fn decode(envelope: &Envelope) -> Result<Self, DecodeError> {
        let Object(PayloadMeta {
            action,
            installation,
            repository,
            organization,
            sender,
        }) = envelope.decode()?;
        let WebhookMeta {
            delivery_id,
            kind,
            target_type,
            target_id,
        } = envelope.meta.clone();

        Ok(Self {
            delivery_id,
            kind,
            action,
            installation_id: installation.map(|Object(installation)| installation.id),
            repository: repository.map(|Object(repository)| repository.into()),
            organization: organization.map(|Object(organization)| organization),
            sender: sender.map(|Object(sender)| sender),
            target_type,
            target_id,
        })
    }
}

/// The meta of the request GitHub sends, read from its headers alone: the
/// delivery ID, the kind, and the target type and ID.
///
/// What an [`Envelope`](crate::Envelope) carries beside the payload bytes,
/// what a [`VerifierSource`](crate::VerifierSource) is handed to choose the
/// [`Verifier`](crate::Verifier) for a request, and the header half of the
/// [`EventMeta`] decoded from the envelope; the fields are `EventMeta`'s,
/// under the same names. The signature is not here: it is parsed on its own
/// into a [`Signature`](crate::Signature).
///
/// Nothing here is signed. GitHub's signature covers the body alone, so these
/// values are claims when the source reads them and still claims after the
/// body verifies. Selecting a secret by them is safe, since a forged target
/// selects a secret its sender does not know and verification fails; basing
/// an authorization decision on them is not.
///
/// `#[non_exhaustive]` for the reason [`EventMeta`] is: GitHub can add a
/// header worth selecting by (`X-GitHub-Hook-ID`, say) without that being a
/// breaking change for every source. A test builds one with
/// [`WebhookMeta::new`] and assigns the target.
///
/// ```
/// use octoevents::{EventKind, WebhookMeta, TargetType};
///
/// let mut meta = WebhookMeta::new("72d3162e-cc78-11e3-81ab-4c9367dc0958", EventKind::Issues);
/// meta.target_type = Some(TargetType::Integration);
/// meta.target_id = Some(12345);
/// # let _ = meta;
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub struct WebhookMeta {
    /// The `X-GitHub-Delivery` value.
    pub delivery_id: String,
    /// The event kind parsed from `X-GitHub-Event`.
    pub kind: EventKind,
    /// The webhook installation target type: for a GitHub App,
    /// [`TargetType::Integration`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_type: Option<TargetType>,
    /// The webhook installation target ID: for a GitHub App, the App ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<u64>,
}

impl WebhookMeta {
    /// Creates the header meta of one delivery of one kind, with no target.
    #[must_use]
    pub fn new(delivery_id: impl Into<String>, kind: EventKind) -> Self {
        Self {
            delivery_id: delivery_id.into(),
            kind,
            target_type: None,
            target_id: None,
        }
    }

    /// Reads the header meta off a request's headers, by the names in
    /// [`header`](crate::header).
    ///
    /// What the receiver does before the body, and what a transport built on
    /// [`authenticate`](crate::authenticate) does to ask a
    /// [`VerifierSource`](crate::VerifierSource) for the verifier it passes
    /// there. A transport that authenticated a request by its own means
    /// builds the envelope from it with
    /// [`Envelope::new`](crate::Envelope::new).
    ///
    /// A value that is not visible ASCII reads as absent. A target type this
    /// crate does not know is [`TargetType::Unknown`] with the value intact;
    /// a target ID that is present but not a number reads as `None`.
    ///
    /// GitHub sends the target headers, but does not document their values
    /// for a GitHub App
    /// ([github/rest-api-description#7210](https://github.com/github/rest-api-description/issues/7210));
    /// `integration` and the App ID are what is consistently observed. A
    /// deployment that cannot rely on them serves each App at a path of its
    /// own instead, with a receiver per path.
    ///
    /// ```
    /// use http::HeaderMap;
    /// use octoevents::{EventKind, WebhookMeta, TargetType, header};
    ///
    /// let mut headers = HeaderMap::new();
    /// headers.insert(header::DELIVERY_ID, "delivery-1".parse()?);
    /// headers.insert(header::EVENT_NAME, "issues".parse()?);
    /// headers.insert(header::TARGET_TYPE, "integration".parse()?);
    /// headers.insert(header::TARGET_ID, "12345".parse()?);
    ///
    /// let meta = WebhookMeta::from_headers(&headers)?;
    ///
    /// assert_eq!(meta.kind, EventKind::Issues);
    /// assert_eq!(meta.target_type, Some(TargetType::Integration));
    /// assert_eq!(meta.target_id, Some(12345));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`ReceiveError::MissingHeader`] when the delivery ID or the
    /// event name is absent or empty, in that order. The target headers are
    /// optional, and their absence refuses nothing.
    pub fn from_headers(headers: &HeaderMap) -> Result<Self, ReceiveError> {
        let delivery_id = header::required(headers, header::DELIVERY_ID)?;
        let event_name = header::required(headers, header::EVENT_NAME)?;

        Ok(Self {
            delivery_id: delivery_id.to_owned(),
            kind: EventKind::from(event_name),
            target_type: header::read(headers, &header::TARGET_TYPE).map(TargetType::from),
            target_id: header::read(headers, &header::TARGET_ID)
                .and_then(|value| value.parse().ok()),
        })
    }
}

/// The repository's meta: the fields [`EventMeta::decode`] keeps of the
/// payload's `repository` object, read without decoding a full payload
/// model.
///
/// What [`EventMeta::repository`] holds. Named as the meta of the repository,
/// not as the repository: it is the four fields that routing and a policy read,
/// where octocrab's `Repository` is the whole object, and a consumer with
/// both in scope should not confuse them. It is a plain struct: a test builds
/// one as a literal or with [`RepositoryMeta::new`], so a field GitHub adds
/// here would be a breaking change, as adding a field to any struct built as
/// a literal is.
///
/// ```
/// use octoevents::RepositoryMeta;
///
/// let literal = RepositoryMeta {
///     id: 1296269,
///     name: "Hello-World".into(),
///     full_name: "octocat/Hello-World".into(),
///     owner: "octocat".into(),
/// };
///
/// assert_eq!(
///     literal,
///     RepositoryMeta::new(1296269, "Hello-World", "octocat/Hello-World", "octocat")
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RepositoryMeta {
    /// GitHub's numeric repository ID.
    pub id: u64,
    /// The unqualified repository name.
    pub name: String,
    /// The owner-qualified repository name.
    pub full_name: String,
    /// The repository owner's login.
    pub owner: String,
}

impl RepositoryMeta {
    /// Creates the meta from the fields GitHub sends in every payload's
    /// `repository` object; a literal spells the same with three `.into()`s.
    ///
    /// ```
    /// use octoevents::{EventKind, EventMeta, RepositoryMeta};
    ///
    /// let mut meta = EventMeta::new("72d3162e-cc78-11e3-81ab-4c9367dc0958", EventKind::Push);
    /// meta.repository = Some(RepositoryMeta::new(1296269, "Hello-World", "octocat/Hello-World", "octocat"));
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

/// An account's meta, the numeric ID and the login: the two fields
/// [`EventMeta::decode`] keeps of the payload's account objects, a user's, an organization's or an
/// app's, read without decoding a full payload model.
///
/// What [`EventMeta::organization`] and [`EventMeta::sender`] hold. The ID
/// is the stable identity, the login the name that can be changed under it,
/// so a policy that keys on an account (a tenant table, a rate limit, a
/// bot allow-list) keys on `id` and shows `login`. `Display` is the login,
/// for a log line. Every other field GitHub sends for an account (`type`,
/// `node_id`, `avatar_url`) is left to a decoded payload. Named as
/// [`RepositoryMeta`] is, and a plain struct like it: a test builds one as a
/// literal or with [`AccountMeta::new`].
///
/// ```
/// use octoevents::AccountMeta;
///
/// let literal = AccountMeta { id: 583231, login: "octocat".into() };
///
/// assert_eq!(literal, AccountMeta::new(583231, "octocat"));
/// assert_eq!(literal.to_string(), "octocat");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AccountMeta {
    /// GitHub's numeric account ID.
    pub id: u64,
    /// The account's login.
    pub login: String,
}

impl AccountMeta {
    /// Creates the meta from the fields GitHub sends in every payload's
    /// `sender` and `organization` objects.
    ///
    /// ```
    /// use octoevents::{AccountMeta, EventKind, EventMeta};
    ///
    /// let mut meta = EventMeta::new("72d3162e-cc78-11e3-81ab-4c9367dc0958", EventKind::Push);
    /// meta.sender = Some(AccountMeta::new(583231, "octocat"));
    /// assert_eq!(meta.sender.unwrap().to_string(), "octocat");
    /// ```
    #[must_use]
    pub fn new(id: u64, login: impl Into<String>) -> Self {
        Self {
            id,
            login: login.into(),
        }
    }
}

impl fmt::Display for AccountMeta {
    /// The login, for a log line or an `@`-mention.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.login)
    }
}

string_enum! {
    /// The resource on which the webhook is installed, from
    /// `X-GitHub-Hook-Installation-Target-Type`: `integration` for a GitHub
    /// App, `repository` for a repository webhook and `organization` for an
    /// organization webhook.
    ///
    /// Unknown strings cannot be edited in place; replace the whole target
    /// type through `TargetType::from` instead.
    ///
    /// ```compile_fail,E0277
    /// use octoevents::TargetType;
    /// let mut target = TargetType::from("future_target");
    /// if let TargetType::Unknown { value, .. } = &mut target {
    ///     *value = "integration".into();
    /// }
    /// ```
    ///
    /// ```compile_fail,E0308
    /// use octoevents::{EventKind, TargetType};
    /// let EventKind::Unknown { value: kind, .. } = EventKind::from("integration") else { return };
    /// let mut target = TargetType::from("future_target");
    /// if let TargetType::Unknown { value, .. } = &mut target {
    ///     *value = kind;
    /// }
    /// ```
    pub enum TargetType, UnknownTargetType {
        Integration => "integration",
        Repository => "repository",
        Organization => "organization",
    }
}

/// The payload half of an [`EventMeta`], in GitHub's shape: what
/// [`EventMeta::decode`] reads of a payload, and nothing more. A plain
/// derive, so it is as strict as serde is: a field of another type, a
/// missing required field or a repeated key is an error. Every object is
/// read through [`Object`], so a positional array is one too.
#[derive(Debug, Deserialize)]
struct PayloadMeta {
    action: Option<Action>,
    installation: Option<Object<Installation>>,
    repository: Option<Object<Repository>>,
    organization: Option<Object<AccountMeta>>,
    sender: Option<Object<AccountMeta>>,
}

/// A JSON object read as `T`, and nothing else: a derived struct also reads
/// a positional array, which GitHub never sends for the meta's objects.
#[derive(Debug)]
struct Object<T>(T);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Object<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor<T>(PhantomData<T>);

        impl<'de, T: Deserialize<'de>> de::Visitor<'de> for ObjectVisitor<T> {
            type Value = Object<T>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object")
            }

            fn visit_map<M: de::MapAccess<'de>>(self, map: M) -> Result<Self::Value, M::Error> {
                T::deserialize(de::value::MapAccessDeserializer::new(map)).map(Object)
            }
        }

        deserializer.deserialize_map(ObjectVisitor(PhantomData))
    }
}

#[derive(Debug, Deserialize)]
struct Installation {
    id: u64,
}

#[derive(Debug, Deserialize)]
struct Repository {
    id: u64,
    name: String,
    full_name: String,
    owner: Object<Owner>,
}

impl From<Repository> for RepositoryMeta {
    fn from(repository: Repository) -> Self {
        Self {
            id: repository.id,
            name: repository.name,
            full_name: repository.full_name,
            owner: {
                let Object(owner) = repository.owner;
                owner.login
            },
        }
    }
}

#[derive(Debug, Deserialize)]
struct Owner {
    login: String,
}
