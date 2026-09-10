use std::{fmt, marker::PhantomData};

use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::value::RawValue;

use crate::{Action, EventKind, events::string_enum};

/// The routing metadata of a webhook: everything in an
/// [`Envelope`](crate::Envelope) except the payload bytes.
///
/// A handler's input on its own, for one routed by kind and action that
/// decodes nothing, and the first half of [`Event<P>`](crate::Event), so the
/// delivery ID and installation ID travel beside a decoded payload without
/// going back to the envelope.
///
/// # Where the fields come from
///
/// Four fields are read from the headers: the delivery ID and the kind,
/// which every delivery carries, and the target type and ID. The other five,
/// the action, installation ID, repository, organization and sender, are read
/// from the payload when the envelope is built, by
/// [`Envelope::from_signed`](crate::Envelope::from_signed) once the body is
/// authenticated and by [`Envelope::new`](crate::Envelope::new) alike. That
/// read, the *probe*, first validates UTF-8 across the bytes, then scans the
/// JSON to keep those five top-level values and skip everything else. The
/// retained values are decoded separately. The validation adds an
/// allocation-free pass; the total cost stays linear in the body, as the
/// signature check over the same bytes is, with no model built of the rest of
/// the document. It runs for every envelope whatever the handler's input will
/// be, because the dispatcher routes by the action, and the action is in the
/// payload, not in a header. It is best-effort and cannot fail; the rules are
/// on `Envelope::new`.
///
/// On the receiving path, verification authenticates the payload bytes, not
/// the delivery ID, event name, or target headers. Header-derived fields
/// alone must not authorize security-sensitive actions; use authenticated
/// payload data or independently trusted configuration. Delivery-ID
/// deduplication handles GitHub redelivery, not adversarial replay.
///
/// So "decodes nothing", said of a handler over this type, means no decode on
/// the handler's behalf, not that the payload went unread. A decode is the
/// fallible turn of the bytes into an input that asks for it, and it happens
/// only for a routed handler whose route matched.
///
/// The crate produces this view and consumers only read it, so it is
/// `#[non_exhaustive]`: GitHub can add a stable routing field (an enterprise
/// reference, for example) without that becoming a breaking change here.
/// In a test, an envelope from [`Envelope::new`](crate::Envelope::new)
/// carries the meta the receiver would have extracted from the same bytes;
/// build a meta by itself with [`EventMeta::new`], for a handler over
/// `EventMeta` alone, and assign the optional fields it reads.
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
    /// The repository's meta, when the payload carries a complete
    /// `repository` object.
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
    /// that agrees with a payload, build the envelope with
    /// [`Envelope::new`](crate::Envelope::new), which reads those fields from
    /// the payload the way the receiver does. This constructor is for a
    /// handler over `EventMeta` alone, or a test that wants the meta and
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

    /// The probe: the metadata with its payload-derived fields (action,
    /// installation ID, repository, organization, sender) read from
    /// `raw_payload`, and the header-derived target left empty for the
    /// caller that has the headers. Both envelope constructors come through
    /// here, so the test path and the receiving path read a payload alike.
    ///
    /// Best-effort and never fatal: after validating UTF-8, the top level is
    /// read as a map of raw values, so invalid UTF-8, malformed JSON syntax,
    /// or JSON whose top level is not an object, leaves every probed field
    /// empty, and one malformed field (a
    /// `repository` missing `full_name`, say) clears only itself. A duplicated
    /// metadata key also clears only itself; a duplicate required key inside
    /// an object invalidates that object. Skipped string values are checked
    /// for escape syntax, not surrogate pairing; see `Envelope::new` for the
    /// policy. The rest of the document is not decoded.
    pub(crate) fn probe(
        delivery_id: impl Into<String>,
        kind: EventKind,
        raw_payload: &[u8],
    ) -> Self {
        // Skipped JSON strings do not get UTF-8 validation from serde_json.
        // Validate the entire payload before any field can supply metadata.
        let probe = std::str::from_utf8(raw_payload)
            .ok()
            .and_then(|payload| serde_json::from_str::<Probe<'_>>(payload).ok())
            .unwrap_or_default();

        Self {
            delivery_id: delivery_id.into(),
            kind,
            action: probe
                .action
                .and_then(parse_probe::<String>)
                .map(Action::from),
            installation_id: probe
                .installation
                .and_then(parse_object::<IdOnly>)
                .map(|installation| installation.id),
            repository: probe
                .repository
                .and_then(parse_object::<RepoProbe>)
                .map(RepositoryMeta::from),
            organization: probe.organization.and_then(parse_object::<AccountMeta>),
            sender: probe.sender.and_then(parse_object::<AccountMeta>),
            target_type: None,
            target_id: None,
        }
    }
}

/// The repository's meta: the fields the probe (the read of the payload
/// described under [Where the fields come
/// from](EventMeta#where-the-fields-come-from)) keeps of the payload's
/// `repository` object, read without decoding a full payload model.
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

/// An account's meta, the numeric ID and the login: the two fields the probe
/// keeps of the payload's account objects, a user's, an organization's or an
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

fn parse_probe<T: serde::de::DeserializeOwned>(value: &RawValue) -> Option<T> {
    serde_json::from_str(value.get()).ok()
}

fn parse_object<T: de::DeserializeOwned>(value: &RawValue) -> Option<T> {
    parse_probe::<Object<T>>(value).map(|object| object.0)
}

/// Restricts an internal probe value to a JSON object without changing the
/// serde behavior of the type it wraps (derived structs also accept arrays).
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

/// The top level of a payload as raw values, one per field the probe reads,
/// so each is parsed on its own and a malformed one does not take its
/// siblings with it. Only objects are accepted, and a repeated metadata key
/// clears its field, even if its first value was null. Unknown keys are skipped.
#[derive(Debug, Default)]
struct Probe<'a> {
    action: Option<&'a RawValue>,
    installation: Option<&'a RawValue>,
    repository: Option<&'a RawValue>,
    organization: Option<&'a RawValue>,
    sender: Option<&'a RawValue>,
}

impl<'de> Deserialize<'de> for Probe<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(field_identifier, rename_all = "snake_case")]
        enum Field {
            Action,
            Installation,
            Repository,
            Organization,
            Sender,
            #[serde(other)]
            Unknown,
        }

        struct ProbeVisitor;

        impl<'de> de::Visitor<'de> for ProbeVisitor {
            type Value = Probe<'de>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a payload object")
            }

            fn visit_map<M: de::MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut probe = Probe::default();
                let mut seen = [false; 5];
                while let Some(key) = map.next_key::<Field>()? {
                    let (field, seen) = match key {
                        Field::Action => (&mut probe.action, &mut seen[0]),
                        Field::Installation => (&mut probe.installation, &mut seen[1]),
                        Field::Repository => (&mut probe.repository, &mut seen[2]),
                        Field::Organization => (&mut probe.organization, &mut seen[3]),
                        Field::Sender => (&mut probe.sender, &mut seen[4]),
                        Field::Unknown => {
                            map.next_value::<de::IgnoredAny>()?;
                            continue;
                        }
                    };
                    let value = map.next_value::<&RawValue>()?;
                    *field = if *seen { None } else { Some(value) };
                    *seen = true;
                }
                Ok(probe)
            }
        }

        deserializer.deserialize_map(ProbeVisitor)
    }
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
    owner: Object<LoginOnly>,
}

impl From<RepoProbe> for RepositoryMeta {
    fn from(repository: RepoProbe) -> Self {
        Self {
            id: repository.id,
            name: repository.name,
            full_name: repository.full_name,
            owner: repository.owner.0.login,
        }
    }
}

#[derive(Debug, Deserialize)]
struct LoginOnly {
    login: String,
}
