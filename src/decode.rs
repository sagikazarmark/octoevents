//! Decoding with octocrab's webhook models: its per-kind payload structs as
//! [`Payload`](crate::Payload) types, and an envelope's payload of any kind
//! as its [`WebhookEvent`].

use octocrab::models::webhook_events::{WebhookEvent, payload};

use crate::{DecodeError, Envelope, EventKind, FromEnvelope};

// Every per-kind payload struct octocrab models, bound to its event kind so
// each can be a handler's input. The structs carry no
// `deny_unknown_fields`, and octocrab itself builds them from the same
// flattened JSON object, so decoding the whole payload into one works.
// `ScheduleWebhookEventPayload` is deliberately absent: `schedule` is a
// workflow trigger, not a webhook event GitHub delivers.
//
// A local macro so every impl carries the same rustdoc: the note below is what
// a consumer reaching for `payload.sender` needs, and it renders on the
// `Payload` trait page beside each impl. The derive is for a consumer's own
// types; these are octocrab's, so the impls are written here.
macro_rules! octocrab_payloads {
    ($($payload:ty => $kind:expr),+ $(,)?) => {
        $(
            /// octocrab's payload for this kind: the fields specific to the
            /// kind. The top-level `installation`, `sender`, `repository`
            /// and `organization` objects belong to octocrab's
            /// [`WebhookEvent`], and its per-kind structs mostly do not
            /// repeat them, so check this struct's fields before reading
            /// `payload.sender`. [`EventMeta`](crate::EventMeta) carries the
            /// installation ID, the sender and organization logins and a
            /// repository reference beside every payload; for the full
            /// objects, decode the event as [`WebhookEvent`]
            /// ([`Envelope::decode_event`]), or define a view naming the
            /// objects you need and derive [`Payload`](crate::Payload) on it.
            impl crate::Payload for $payload {
                const KIND: EventKind = $kind;
            }
        )+
    };
}

octocrab_payloads! {
    payload::BranchProtectionRuleWebhookEventPayload => EventKind::BranchProtectionRule,
    payload::CheckRunWebhookEventPayload => EventKind::CheckRun,
    payload::CheckSuiteWebhookEventPayload => EventKind::CheckSuite,
    payload::CodeScanningAlertWebhookEventPayload => EventKind::CodeScanningAlert,
    payload::CommitCommentWebhookEventPayload => EventKind::CommitComment,
    payload::CreateWebhookEventPayload => EventKind::Create,
    payload::DeleteWebhookEventPayload => EventKind::Delete,
    payload::DependabotAlertWebhookEventPayload => EventKind::DependabotAlert,
    payload::DeployKeyWebhookEventPayload => EventKind::DeployKey,
    payload::DeploymentWebhookEventPayload => EventKind::Deployment,
    payload::DeploymentProtectionRuleWebhookEventPayload => EventKind::DeploymentProtectionRule,
    payload::DeploymentStatusWebhookEventPayload => EventKind::DeploymentStatus,
    payload::DiscussionWebhookEventPayload => EventKind::Discussion,
    payload::DiscussionCommentWebhookEventPayload => EventKind::DiscussionComment,
    payload::ForkWebhookEventPayload => EventKind::Fork,
    payload::GithubAppAuthorizationWebhookEventPayload => EventKind::GithubAppAuthorization,
    payload::GollumWebhookEventPayload => EventKind::Gollum,
    payload::InstallationWebhookEventPayload => EventKind::Installation,
    payload::InstallationRepositoriesWebhookEventPayload => EventKind::InstallationRepositories,
    payload::InstallationTargetWebhookEventPayload => EventKind::InstallationTarget,
    payload::IssueCommentWebhookEventPayload => EventKind::IssueComment,
    payload::IssuesWebhookEventPayload => EventKind::Issues,
    payload::LabelWebhookEventPayload => EventKind::Label,
    payload::MarketplacePurchaseWebhookEventPayload => EventKind::MarketplacePurchase,
    payload::MemberWebhookEventPayload => EventKind::Member,
    payload::MembershipWebhookEventPayload => EventKind::Membership,
    payload::MergeGroupWebhookEventPayload => EventKind::MergeGroup,
    payload::MetaWebhookEventPayload => EventKind::Meta,
    payload::MilestoneWebhookEventPayload => EventKind::Milestone,
    payload::OrgBlockWebhookEventPayload => EventKind::OrgBlock,
    payload::OrganizationWebhookEventPayload => EventKind::Organization,
    payload::PackageWebhookEventPayload => EventKind::Package,
    payload::PageBuildWebhookEventPayload => EventKind::PageBuild,
    payload::PersonalAccessTokenRequestWebhookEventPayload => EventKind::PersonalAccessTokenRequest,
    payload::PingWebhookEventPayload => EventKind::Ping,
    payload::ProjectWebhookEventPayload => EventKind::Project,
    payload::ProjectCardWebhookEventPayload => EventKind::ProjectCard,
    payload::ProjectColumnWebhookEventPayload => EventKind::ProjectColumn,
    payload::ProjectsV2WebhookEventPayload => EventKind::ProjectsV2,
    payload::ProjectsV2ItemWebhookEventPayload => EventKind::ProjectsV2Item,
    payload::PublicWebhookEventPayload => EventKind::Public,
    payload::PullRequestWebhookEventPayload => EventKind::PullRequest,
    payload::PullRequestReviewWebhookEventPayload => EventKind::PullRequestReview,
    payload::PullRequestReviewCommentWebhookEventPayload => EventKind::PullRequestReviewComment,
    payload::PullRequestReviewThreadWebhookEventPayload => EventKind::PullRequestReviewThread,
    payload::PushWebhookEventPayload => EventKind::Push,
    payload::RegistryPackageWebhookEventPayload => EventKind::RegistryPackage,
    payload::ReleaseWebhookEventPayload => EventKind::Release,
    payload::RepositoryWebhookEventPayload => EventKind::Repository,
    payload::RepositoryAdvisoryWebhookEventPayload => EventKind::RepositoryAdvisory,
    payload::RepositoryDispatchWebhookEventPayload => EventKind::RepositoryDispatch,
    payload::RepositoryImportWebhookEventPayload => EventKind::RepositoryImport,
    payload::RepositoryVulnerabilityAlertWebhookEventPayload => EventKind::RepositoryVulnerabilityAlert,
    payload::SecretScanningAlertWebhookEventPayload => EventKind::SecretScanningAlert,
    payload::SecretScanningAlertLocationWebhookEventPayload => EventKind::SecretScanningAlertLocation,
    payload::SecurityAdvisoryWebhookEventPayload => EventKind::SecurityAdvisory,
    payload::SecurityAndAnalysisWebhookEventPayload => EventKind::SecurityAndAnalysis,
    payload::SponsorshipWebhookEventPayload => EventKind::Sponsorship,
    payload::StarWebhookEventPayload => EventKind::Star,
    payload::StatusWebhookEventPayload => EventKind::Status,
    payload::TeamWebhookEventPayload => EventKind::Team,
    payload::TeamAddWebhookEventPayload => EventKind::TeamAdd,
    payload::WatchWebhookEventPayload => EventKind::Watch,
    payload::WorkflowDispatchWebhookEventPayload => EventKind::WorkflowDispatch,
    payload::WorkflowJobWebhookEventPayload => EventKind::WorkflowJob,
    payload::WorkflowRunWebhookEventPayload => EventKind::WorkflowRun,
}

/// octocrab's decoded event for any kind, so a handler over it is registered
/// with `Dispatcher::on` for logic that spans kinds; the decode is
/// [`Envelope::decode_event`]. octocrab's `WebhookEvent` is not a
/// [`Payload`](crate::Payload): it declares no single kind.
impl FromEnvelope for WebhookEvent {
    fn from_envelope(envelope: &Envelope) -> Result<Self, DecodeError> {
        envelope.decode_event()
    }
}

impl Envelope {
    /// Decodes the payload as octocrab's [`WebhookEvent`] for the envelope's
    /// kind.
    ///
    /// This is what a [`Handler`] over `WebhookEvent` (or over
    /// `Event<WebhookEvent>`) receives, through `WebhookEvent`'s
    /// [`FromEnvelope`] impl; call it directly from a handler over the
    /// [`Envelope`] that needs octocrab's models alongside the payload bytes.
    ///
    /// Best-effort: octocrab's webhook models are hand-maintained and
    /// self-described as beta. An event kind octocrab does not know still
    /// decodes -- it arrives as [`WebhookEventPayload::Unknown`] carrying the
    /// generic JSON -- so an error here means the payload was not a JSON
    /// object or a known kind's payload drifted. [`Envelope::raw_payload`] is
    /// unaffected either way, and [`Envelope::decode`] decodes a caller-defined
    /// view that only breaks on fields you name.
    ///
    /// Decodes [`Envelope::raw_payload`] on every call. Bind the result rather than
    /// calling it repeatedly: a delivery can carry megabytes of JSON.
    ///
    /// [`WebhookEventPayload::Unknown`]: octocrab::models::webhook_events::WebhookEventPayload::Unknown
    /// [`Handler`]: crate::Handler
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::Json`] for payloads that are not a JSON object
    /// and payloads octocrab cannot represent.
    pub fn decode_event(&self) -> Result<WebhookEvent, DecodeError> {
        WebhookEvent::try_from_header_and_body(self.meta.kind.as_str(), &self.raw_payload)
            .map_err(DecodeError::Json)
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use octocrab::models::webhook_events::{WebhookEventPayload, WebhookEventType};

    use crate::{
        DecodeError, Envelope, EventKind,
        test_support::{
            check_run_completed, envelope, installation_created, installation_repositories_removed,
            ping, pull_request_opened, unknown, unrepresentable,
        },
    };

    #[test]
    fn returns_octocrab_models_when_the_payload_is_supported() {
        let event = envelope(EventKind::Ping, br#"{"zen":"Keep it logically awesome."}"#)
            .decode_event()
            .unwrap();

        assert_eq!(event.kind, WebhookEventType::Ping);
        assert!(matches!(event.specific, WebhookEventPayload::Ping(_)));
    }

    #[test]
    fn represents_unknown_event_kinds_as_generic_json() {
        let event = envelope(
            EventKind::Unknown("future_event".into()),
            br#"{"future":true}"#,
        )
        .decode_event()
        .unwrap();

        assert_eq!(
            event.specific,
            WebhookEventPayload::Unknown(Box::new(serde_json::json!({"future": true})))
        );
    }

    #[test]
    fn fails_for_payloads_octocrab_cannot_represent() {
        // The fixture the dispatcher tests route to a handler over
        // `WebhookEvent` when a decode must fail: valid JSON under a known
        // kind, carrying nothing octocrab's model of that kind requires. A
        // consumer view over the same bytes still decodes; only octocrab's
        // path refuses them, and the raw payload is untouched either way.
        let envelope = unrepresentable();

        let error = envelope.decode_event().unwrap_err();

        assert!(matches!(error, DecodeError::Json(_)), "{error:?}");
        assert_eq!(
            envelope.raw_payload,
            Bytes::from_static(include_bytes!("../tests/fixtures/unrepresentable.json"))
        );
    }

    #[test]
    fn fails_for_invalid_json_without_touching_the_raw_bytes() {
        let envelope = envelope(EventKind::Ping, b"not json");

        assert!(envelope.decode_event().is_err());
        assert_eq!(envelope.raw_payload, Bytes::from_static(b"not json"));
    }

    #[test]
    fn octocrab_payload_types_decode_the_fixture_corpus_for_their_kind() {
        use octocrab::models::webhook_events::payload::{
            CheckRunWebhookEventPayload, InstallationRepositoriesWebhookEventPayload,
            InstallationWebhookEventPayload, PingWebhookEventPayload,
            PullRequestWebhookEventPayload,
        };

        use crate::Payload;

        /// The type is bound to the fixture's kind, and the fixture decodes
        /// into it: the two halves of `Payload` for one of octocrab's structs.
        fn assert_decodes<P: Payload + serde::de::DeserializeOwned>(envelope: &Envelope) {
            assert_eq!(P::KIND, envelope.meta.kind, "bound to the wrong kind");
            envelope
                .decode_payload::<P>()
                .unwrap_or_else(|error| panic!("{}: {error}", envelope.meta.kind));
        }

        assert_decodes::<PullRequestWebhookEventPayload>(&pull_request_opened());
        assert_decodes::<CheckRunWebhookEventPayload>(&check_run_completed());
        assert_decodes::<InstallationWebhookEventPayload>(&installation_created());
        assert_decodes::<InstallationRepositoriesWebhookEventPayload>(
            &installation_repositories_removed(),
        );
        assert_decodes::<PingWebhookEventPayload>(&ping());
    }

    #[test]
    fn decode_event_represents_every_corpus_fixture_as_its_kinds_payload() {
        use octocrab::models::webhook_events::payload::{
            CheckRunWebhookEventAction, InstallationRepositoriesWebhookEventAction,
            InstallationWebhookEventAction, PullRequestWebhookEventAction,
        };

        // Every fixture the corpus holds, with octocrab's kind for it. The
        // payload each decodes into is checked against a value the fixture is
        // known to carry, so a drift in octocrab's model of a kind fails at
        // the fixture that shows it, and an event name the crate does not
        // know still decodes, as generic JSON.
        let corpus = [
            (pull_request_opened(), WebhookEventType::PullRequest),
            (check_run_completed(), WebhookEventType::CheckRun),
            (installation_created(), WebhookEventType::Installation),
            (
                installation_repositories_removed(),
                WebhookEventType::InstallationRepositories,
            ),
            (ping(), WebhookEventType::Ping),
            (
                unknown(),
                WebhookEventType::Unknown("future_event".to_owned()),
            ),
        ];

        for (envelope, kind) in corpus {
            let event = envelope
                .decode_event()
                .unwrap_or_else(|error| panic!("{}: {error}", envelope.meta.kind));

            assert_eq!(event.kind, kind);
            match (kind, event.specific) {
                (WebhookEventType::PullRequest, WebhookEventPayload::PullRequest(payload)) => {
                    assert_eq!(payload.action, PullRequestWebhookEventAction::Opened);
                    assert_eq!(payload.number, 2);
                }
                (WebhookEventType::CheckRun, WebhookEventPayload::CheckRun(payload)) => {
                    assert_eq!(payload.action, CheckRunWebhookEventAction::Completed);
                    assert_eq!(payload.check_run["name"], "Octocoders-linter");
                }
                (WebhookEventType::Installation, WebhookEventPayload::Installation(payload)) => {
                    assert_eq!(payload.action, InstallationWebhookEventAction::Created);
                }
                (
                    WebhookEventType::InstallationRepositories,
                    WebhookEventPayload::InstallationRepositories(payload),
                ) => {
                    assert_eq!(
                        payload.action,
                        InstallationRepositoriesWebhookEventAction::Removed
                    );
                    assert_eq!(payload.repositories_removed.len(), 1);
                }
                (WebhookEventType::Ping, WebhookEventPayload::Ping(payload)) => {
                    assert_eq!(payload.zen.as_deref(), Some("Design for failure."));
                }
                (WebhookEventType::Unknown(_), WebhookEventPayload::Unknown(json)) => {
                    assert_eq!(json["zen"], "Design for failure.");
                }
                (kind, specific) => panic!("{kind:?} decoded as {specific:?}"),
            }
        }
    }
}
