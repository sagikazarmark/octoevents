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
// A local macro rather than `impl_payload!` so every impl carries the same
// rustdoc: the note below is what a consumer reaching for `payload.sender`
// needs, and it renders on the `Payload` trait page beside each impl.
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
            /// objects you need with [`impl_payload!`](crate::impl_payload).
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
    /// [`Envelope`] that needs octocrab's models alongside the raw bytes.
    ///
    /// Best-effort: octocrab's webhook models are hand-maintained and
    /// self-described as beta. An event kind octocrab does not know still
    /// decodes -- it arrives as [`WebhookEventPayload::Unknown`] carrying the
    /// generic JSON -- so an error here means the payload was not a JSON
    /// object or a known kind's payload drifted. [`Envelope::raw`] is
    /// unaffected either way, and [`Envelope::decode`] decodes a caller-defined
    /// view that only breaks on fields you name.
    ///
    /// Decodes [`Envelope::raw`] on every call. Bind the result rather than
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
        WebhookEvent::try_from_header_and_body(self.meta.kind.as_str(), &self.raw)
            .map_err(DecodeError::Json)
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use octocrab::models::webhook_events::{WebhookEventPayload, WebhookEventType};

    use crate::{EventKind, test_support::envelope};

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
        let envelope = envelope(EventKind::PullRequest, br#"{"future":true}"#);

        assert!(envelope.decode_event().is_err());
        assert_eq!(envelope.raw, Bytes::from_static(br#"{"future":true}"#));
    }

    #[test]
    fn fails_for_invalid_json_without_touching_the_raw_bytes() {
        let envelope = envelope(EventKind::Ping, b"not json");

        assert!(envelope.decode_event().is_err());
        assert_eq!(envelope.raw, Bytes::from_static(b"not json"));
    }

    #[test]
    fn octocrab_payload_types_decode_the_fixture_corpus_for_their_kind() {
        use std::str::FromStr as _;

        use octocrab::models::webhook_events::payload::{
            CheckRunWebhookEventPayload, InstallationRepositoriesWebhookEventPayload,
            InstallationWebhookEventPayload, PingWebhookEventPayload,
            PullRequestWebhookEventPayload,
        };

        use crate::Payload;

        fn decodes<P: Payload + serde::de::DeserializeOwned>(
            event_name: &str,
            raw: &'static [u8],
        ) -> bool {
            let kind = EventKind::from_str(event_name).unwrap();
            assert_eq!(P::KIND, kind, "{event_name} maps to the wrong kind");
            envelope(kind, raw).decode_payload::<P>().is_ok()
        }

        assert!(decodes::<PullRequestWebhookEventPayload>(
            "pull_request",
            include_bytes!("../tests/fixtures/pull_request.opened.json"),
        ));
        assert!(decodes::<CheckRunWebhookEventPayload>(
            "check_run",
            include_bytes!("../tests/fixtures/check_run.completed.json"),
        ));
        assert!(decodes::<InstallationWebhookEventPayload>(
            "installation",
            include_bytes!("../tests/fixtures/installation.created.json"),
        ));
        assert!(decodes::<InstallationRepositoriesWebhookEventPayload>(
            "installation_repositories",
            include_bytes!("../tests/fixtures/installation_repositories.removed.json"),
        ));
        assert!(decodes::<PingWebhookEventPayload>(
            "ping",
            include_bytes!("../tests/fixtures/ping.json")
        ));
    }
}
