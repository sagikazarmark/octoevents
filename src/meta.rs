use std::fmt;

use serde::{Deserialize, Serialize};
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
    /// A compact repository reference, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<RepositoryRef>,
    /// The organization the webhook fired in, when present: an organization
    /// webhook's, or a GitHub App's installed on one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<AccountRef>,
    /// The user or app whose act triggered the webhook, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<AccountRef>,
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
    /// Best-effort and never fatal: the top level is read as a map of raw
    /// values, so malformed JSON leaves every probed field empty and one
    /// malformed field (a `repository` missing `full_name`, say) clears only
    /// itself. The rest of the document is not decoded.
    pub(crate) fn probe(
        delivery_id: impl Into<String>,
        kind: EventKind,
        raw_payload: &[u8],
    ) -> Self {
        let probe = serde_json::from_slice::<Probe<'_>>(raw_payload).unwrap_or_default();

        Self {
            delivery_id: delivery_id.into(),
            kind,
            action: probe
                .action
                .and_then(parse_probe::<String>)
                .map(|action| Action::from(action.as_str())),
            installation_id: probe
                .installation
                .and_then(parse_probe::<IdOnly>)
                .map(|installation| installation.id),
            repository: probe
                .repository
                .and_then(parse_probe::<RepoProbe>)
                .map(RepositoryRef::from),
            organization: probe.organization.and_then(parse_probe::<AccountRef>),
            sender: probe.sender.and_then(parse_probe::<AccountRef>),
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

/// A compact reference to a GitHub account, a user, an organization or an
/// app, extracted without parsing a full payload model: the numeric ID and
/// the login.
///
/// What [`EventMeta::organization`] and [`EventMeta::sender`] hold. The ID
/// is the stable identity, the login the name that can be changed under it,
/// so a policy that keys on an account (a tenant table, a rate limit, a
/// bot allow-list) keys on `id` and shows `login`. `Display` is the login,
/// for a log line. Every other field GitHub sends for an account (`type`,
/// `node_id`, `avatar_url`) is left to a decoded payload.
///
/// `#[non_exhaustive]` for the same reason as [`EventMeta`]. Build one in
/// tests with [`AccountRef::new`], which takes every field the crate probes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AccountRef {
    /// GitHub's numeric account ID.
    pub id: u64,
    /// The account's login.
    pub login: String,
}

impl AccountRef {
    /// Creates a reference from the fields GitHub sends in every payload's
    /// `sender` and `organization` objects.
    ///
    /// ```
    /// use octoevents::{AccountRef, EventKind, EventMeta};
    ///
    /// let mut meta = EventMeta::new("72d3162e-cc78-11e3-81ab-4c9367dc0958", EventKind::Push);
    /// meta.sender = Some(AccountRef::new(583231, "octocat"));
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

impl fmt::Display for AccountRef {
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
    pub enum TargetType {
        Integration => "integration",
        Repository => "repository",
        Organization => "organization",
    }
}

fn parse_probe<T: serde::de::DeserializeOwned>(value: &RawValue) -> Option<T> {
    serde_json::from_str(value.get()).ok()
}

/// The top level of a payload as raw values, one per field the probe reads,
/// so each is parsed on its own and a malformed one does not take its
/// siblings with it. `organization` and `sender` then parse as
/// [`AccountRef`] directly: its serde shape is the subset of GitHub's
/// account object the meta keeps, and the fields it does not name are
/// ignored.
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
