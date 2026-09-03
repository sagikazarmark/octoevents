use octocrab::models::webhook_events::{WebhookEvent, payload};

use crate::{Envelope, EventKind};

// Every per-kind payload struct octocrab models, bound to its event kind so
// each can drive a `PayloadHandler`. The structs carry no
// `deny_unknown_fields`, and octocrab itself builds them from the same
// flattened JSON object, so decoding the whole payload into one works.
// `ScheduleWebhookEventPayload` is deliberately absent: `schedule` is a
// workflow trigger, not a webhook event GitHub delivers.
crate::impl_payload! {
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

impl Envelope {
    /// Parses the payload using octocrab's deep webhook models.
    ///
    /// Best-effort: octocrab's webhook models are hand-maintained and
    /// self-described as beta. An event kind octocrab does not know still
    /// parses -- it arrives as [`WebhookEventPayload::Unknown`] carrying the
    /// generic JSON -- so an error here means the body was not a JSON object
    /// or a known kind's payload drifted. [`Envelope::raw`] is unaffected
    /// either way, and [`Envelope::parse`] deserializes a caller-defined view
    /// that only breaks on fields you name.
    ///
    /// Parses [`Envelope::raw`] on every call. Bind the result rather than
    /// calling it repeatedly: a delivery can carry megabytes of JSON.
    ///
    /// Enabling the `octocrab` feature makes octocrab's pre-1.0 version part
    /// of this crate's public API: [`WebhookEvent`] is octocrab's type, so an
    /// octocrab major bump here is a breaking change for this method.
    ///
    /// [`WebhookEventPayload::Unknown`]: octocrab::models::webhook_events::WebhookEventPayload::Unknown
    ///
    /// # Errors
    ///
    /// Returns a serde error for non-object bodies and payloads octocrab
    /// cannot represent.
    #[cfg_attr(docsrs, doc(cfg(feature = "octocrab")))]
    pub fn parse_typed(&self) -> Result<WebhookEvent, serde_json::Error> {
        WebhookEvent::try_from_header_and_body(self.meta.kind.as_str(), &self.raw)
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use octocrab::models::webhook_events::{WebhookEventPayload, WebhookEventType};

    use crate::{Envelope, EventKind, EventMeta};

    fn envelope(kind: EventKind, raw: &'static [u8]) -> Envelope {
        Envelope {
            meta: EventMeta::new("delivery", kind),
            raw: Bytes::from_static(raw),
        }
    }

    #[test]
    fn returns_octocrab_models_when_the_payload_is_supported() {
        let event = envelope(EventKind::Ping, br#"{"zen":"Keep it logically awesome."}"#)
            .parse_typed()
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
        .parse_typed()
        .unwrap();

        assert_eq!(
            event.specific,
            WebhookEventPayload::Unknown(Box::new(serde_json::json!({"future": true})))
        );
    }

    #[test]
    fn fails_for_payloads_octocrab_cannot_represent() {
        let envelope = envelope(EventKind::PullRequest, br#"{"future":true}"#);

        assert!(envelope.parse_typed().is_err());
        assert_eq!(envelope.raw, Bytes::from_static(br#"{"future":true}"#));
    }

    #[test]
    fn fails_for_invalid_json_without_touching_the_raw_body() {
        let envelope = envelope(EventKind::Ping, b"not json");

        assert!(envelope.parse_typed().is_err());
        assert_eq!(envelope.raw, Bytes::from_static(b"not json"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn octocrab_payload_types_decode_the_fixture_corpus_for_their_kind() {
        use std::str::FromStr as _;

        use octocrab::models::webhook_events::payload::{
            CheckRunWebhookEventPayload, InstallationRepositoriesWebhookEventPayload,
            InstallationWebhookEventPayload, PingWebhookEventPayload,
            PullRequestWebhookEventPayload,
        };

        use crate::{Payload, PayloadHandler as _, WebhookHandler as _};

        async fn decodes<P: Payload + 'static>(event: &str, body: &'static [u8]) -> bool {
            let handler = (|_: EventMeta, _: P| async { Ok::<_, ()>(()) }).into_webhook_handler();
            let kind = EventKind::from_str(event).unwrap();
            assert_eq!(P::KIND, kind, "{event} maps to the wrong kind");
            handler.handle(envelope(kind, body)).await.is_ok()
        }

        assert!(
            decodes::<PullRequestWebhookEventPayload>(
                "pull_request",
                include_bytes!("../tests/fixtures/pull_request.opened.json"),
            )
            .await
        );
        assert!(
            decodes::<CheckRunWebhookEventPayload>(
                "check_run",
                include_bytes!("../tests/fixtures/check_run.completed.json"),
            )
            .await
        );
        assert!(
            decodes::<InstallationWebhookEventPayload>(
                "installation",
                include_bytes!("../tests/fixtures/installation.created.json"),
            )
            .await
        );
        assert!(
            decodes::<InstallationRepositoriesWebhookEventPayload>(
                "installation_repositories",
                include_bytes!("../tests/fixtures/installation_repositories.removed.json"),
            )
            .await
        );
        assert!(
            decodes::<PingWebhookEventPayload>(
                "ping",
                include_bytes!("../tests/fixtures/ping.json")
            )
            .await
        );
    }
}
