use std::{convert::Infallible, fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

macro_rules! string_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $($variant:ident => $wire:literal,)*
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        #[non_exhaustive]
        pub enum $name {
            $(
                #[doc = concat!("The `", $wire, "` wire value.")]
                $variant,
            )*
            /// A wire value unknown to this version of the crate.
            Unknown(String),
        }

        impl $name {
            /// Returns the original GitHub wire value.
            #[must_use]
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $wire,)*
                    Self::Unknown(value) => value,
                }
            }

            #[cfg(test)]
            fn known_values() -> &'static [Self] {
                &[$(Self::$variant,)*]
            }
        }

        impl From<&str> for $name {
            /// The variant for a GitHub wire value, or [`Unknown`](Self::Unknown)
            /// carrying the value verbatim; never a failure.
            fn from(value: &str) -> Self {
                match value {
                    $($wire => Self::$variant,)*
                    value => Self::Unknown(value.to_owned()),
                }
            }
        }

        impl FromStr for $name {
            type Err = Infallible;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Ok(Self::from(value))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                String::deserialize(deserializer).map(|value| Self::from(value.as_str()))
            }
        }
    };
}

// Known GitHub webhook event names. Keep existing variants for API compatibility.
string_enum! {
    /// The event name from `X-GitHub-Event`.
    ///
    /// The kind is always taken from that header, never inferred from the
    /// payload's shape. Kinds share shapes (an `issues` and an `issue_comment`
    /// payload both carry `issue`, `repository` and `sender`), so a guess from
    /// the shape can land on the wrong kind and has no answer for a kind it
    /// was not built with. The header names the kind outright, and a name
    /// this crate does not know arrives as [`Unknown`](Self::Unknown) with
    /// the wire value intact, so it can still be routed, logged, or rejected.
    pub enum EventKind {
        BranchProtectionConfiguration => "branch_protection_configuration",
        BranchProtectionRule => "branch_protection_rule",
        CheckRun => "check_run",
        CheckSuite => "check_suite",
        CodeScanningAlert => "code_scanning_alert",
        CommitComment => "commit_comment",
        Create => "create",
        CustomProperty => "custom_property",
        CustomPropertyValues => "custom_property_values",
        Delete => "delete",
        DependabotAlert => "dependabot_alert",
        DeployKey => "deploy_key",
        Deployment => "deployment",
        DeploymentProtectionRule => "deployment_protection_rule",
        DeploymentReview => "deployment_review",
        DeploymentStatus => "deployment_status",
        Discussion => "discussion",
        DiscussionComment => "discussion_comment",
        Fork => "fork",
        GithubAppAuthorization => "github_app_authorization",
        Gollum => "gollum",
        Installation => "installation",
        InstallationRepositories => "installation_repositories",
        InstallationTarget => "installation_target",
        IssueComment => "issue_comment",
        IssueDependencies => "issue_dependencies",
        Issues => "issues",
        Label => "label",
        MarketplacePurchase => "marketplace_purchase",
        Member => "member",
        Membership => "membership",
        MergeGroup => "merge_group",
        Meta => "meta",
        Milestone => "milestone",
        OrgBlock => "org_block",
        Organization => "organization",
        Package => "package",
        PageBuild => "page_build",
        PersonalAccessTokenRequest => "personal_access_token_request",
        Ping => "ping",
        Project => "project",
        ProjectCard => "project_card",
        ProjectColumn => "project_column",
        ProjectsV2 => "projects_v2",
        ProjectsV2Item => "projects_v2_item",
        ProjectsV2StatusUpdate => "projects_v2_status_update",
        Public => "public",
        PullRequest => "pull_request",
        PullRequestReview => "pull_request_review",
        PullRequestReviewComment => "pull_request_review_comment",
        PullRequestReviewThread => "pull_request_review_thread",
        Push => "push",
        RegistryPackage => "registry_package",
        Release => "release",
        Repository => "repository",
        RepositoryAdvisory => "repository_advisory",
        RepositoryDispatch => "repository_dispatch",
        RepositoryImport => "repository_import",
        RepositoryRuleset => "repository_ruleset",
        RepositoryVulnerabilityAlert => "repository_vulnerability_alert",
        SecretScanningAlert => "secret_scanning_alert",
        SecretScanningAlertLocation => "secret_scanning_alert_location",
        SecretScanningScan => "secret_scanning_scan",
        SecurityAdvisory => "security_advisory",
        SecurityAndAnalysis => "security_and_analysis",
        Sponsorship => "sponsorship",
        Star => "star",
        Status => "status",
        SubIssues => "sub_issues",
        Team => "team",
        TeamAdd => "team_add",
        Watch => "watch",
        WorkflowDispatch => "workflow_dispatch",
        WorkflowJob => "workflow_job",
        WorkflowRun => "workflow_run",
    }
}

// Known top-level action names shared across GitHub webhook events.
string_enum! {
    /// An action extracted from a webhook payload.
    pub enum Action {
        Added => "added",
        AddedToRepository => "added_to_repository",
        Answered => "answered",
        AppearedInBranch => "appeared_in_branch",
        Approved => "approved",
        Archived => "archived",
        Assigned => "assigned",
        AssigneesChanged => "assignees_changed",
        AutoDismissed => "auto_dismissed",
        AutoMergeDisabled => "auto_merge_disabled",
        AutoMergeEnabled => "auto_merge_enabled",
        AutoReopened => "auto_reopened",
        Blocked => "blocked",
        BlockedByAdded => "blocked_by_added",
        BlockedByRemoved => "blocked_by_removed",
        BlockingAdded => "blocking_added",
        BlockingRemoved => "blocking_removed",
        Cancelled => "cancelled",
        CategoryChanged => "category_changed",
        Changed => "changed",
        ChecksRequested => "checks_requested",
        Closed => "closed",
        ClosedByUser => "closed_by_user",
        Completed => "completed",
        Converted => "converted",
        ConvertedToDraft => "converted_to_draft",
        Create => "create",
        Created => "created",
        Deleted => "deleted",
        Demilestoned => "demilestoned",
        Denied => "denied",
        Dequeued => "dequeued",
        Destroyed => "destroyed",
        Disabled => "disabled",
        Dismiss => "dismiss",
        Dismissed => "dismissed",
        Edited => "edited",
        Enabled => "enabled",
        Enqueued => "enqueued",
        FieldAdded => "field_added",
        FieldRemoved => "field_removed",
        Fixed => "fixed",
        InProgress => "in_progress",
        Labeled => "labeled",
        Locked => "locked",
        MemberAdded => "member_added",
        MemberInvited => "member_invited",
        MemberRemoved => "member_removed",
        MetadataCreated => "metadata_created",
        MetadataRemoved => "metadata_removed",
        Milestoned => "milestoned",
        Moved => "moved",
        NewPermissionsAccepted => "new_permissions_accepted",
        Opened => "opened",
        ParentIssueAdded => "parent_issue_added",
        ParentIssueRemoved => "parent_issue_removed",
        PendingCancellation => "pending_cancellation",
        PendingChange => "pending_change",
        PendingChangeCancelled => "pending_change_cancelled",
        PendingTierChange => "pending_tier_change",
        Performed => "performed",
        Pinned => "pinned",
        Prereleased => "prereleased",
        Privatized => "privatized",
        PromoteToEnterprise => "promote_to_enterprise",
        Publicized => "publicized",
        PubliclyLeaked => "publicly_leaked",
        Published => "published",
        Purchased => "purchased",
        Queued => "queued",
        ReadyForReview => "ready_for_review",
        Reintroduced => "reintroduced",
        Rejected => "rejected",
        Released => "released",
        Removed => "removed",
        RemovedFromRepository => "removed_from_repository",
        Renamed => "renamed",
        Reopen => "reopen",
        Reopened => "reopened",
        ReopenedByUser => "reopened_by_user",
        Reordered => "reordered",
        Reported => "reported",
        Requested => "requested",
        RequestedAction => "requested_action",
        Rerequested => "rerequested",
        Resolve => "resolve",
        Resolved => "resolved",
        Restored => "restored",
        ReviewRequestRemoved => "review_request_removed",
        ReviewRequested => "review_requested",
        Revoked => "revoked",
        Stacked => "stacked",
        Started => "started",
        SubIssueAdded => "sub_issue_added",
        SubIssueRemoved => "sub_issue_removed",
        Submitted => "submitted",
        Suspend => "suspend",
        Synchronize => "synchronize",
        TierChanged => "tier_changed",
        Transferred => "transferred",
        Typed => "typed",
        Unanswered => "unanswered",
        Unarchived => "unarchived",
        Unassigned => "unassigned",
        Unblocked => "unblocked",
        Unlabeled => "unlabeled",
        Unlocked => "unlocked",
        Unpinned => "unpinned",
        Unpublished => "unpublished",
        Unresolved => "unresolved",
        Unsuspend => "unsuspend",
        Untyped => "untyped",
        Updated => "updated",
        UpdatedAssignment => "updated_assignment",
        Validated => "validated",
        Waiting => "waiting",
        Withdrawn => "withdrawn",
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

#[cfg(test)]
mod tests {
    use std::fmt;

    use serde::Serialize;

    use super::{Action, EventKind, TargetType};

    /// Every known value comes back from its own wire string, displays as it
    /// and serializes as it quoted.
    fn round_trips<T>(values: &[T])
    where
        T: fmt::Debug + fmt::Display + PartialEq + Serialize + for<'a> From<&'a str>,
    {
        for value in values {
            let wire = value.to_string();
            assert_eq!(&T::from(wire.as_str()), value);
            assert_eq!(
                serde_json::to_string(value).unwrap(),
                format!("\"{wire}\""),
                "{value:?}"
            );
        }
    }

    #[test]
    fn event_names_round_trip() {
        round_trips(EventKind::known_values());
    }

    #[test]
    fn action_names_round_trip() {
        round_trips(Action::known_values());
    }

    #[test]
    fn target_types_round_trip() {
        round_trips(TargetType::known_values());
    }

    #[test]
    fn the_wire_values_are_githubs() {
        // The header and payload strings GitHub sends, so a rename of a
        // variant cannot silently change what it parses from.
        assert_eq!(EventKind::from("pull_request"), EventKind::PullRequest);
        assert_eq!(Action::from("ready_for_review"), Action::ReadyForReview);
        assert_eq!(TargetType::from("integration"), TargetType::Integration);
        assert_eq!(TargetType::from("repository"), TargetType::Repository);
        assert_eq!(TargetType::from("organization"), TargetType::Organization);
    }

    #[test]
    fn unknown_values_are_lossless() {
        let event: EventKind = serde_json::from_str("\"future_event\"").unwrap();
        let action = Action::from("future_action");
        let target_type: TargetType = "enterprise".parse().unwrap();

        assert_eq!(event, EventKind::Unknown("future_event".into()));
        assert_eq!(action, Action::Unknown("future_action".into()));
        assert_eq!(target_type, TargetType::Unknown("enterprise".into()));
        assert_eq!(serde_json::to_string(&event).unwrap(), "\"future_event\"");
        assert_eq!(target_type.to_string(), "enterprise");
        assert_eq!(
            serde_json::from_str::<TargetType>(r#""enterprise""#).unwrap(),
            target_type
        );
    }
}
