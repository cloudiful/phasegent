#![allow(unused_imports)]
use super::support;
use super::support::{
    MockResponse, TEST_API_KEY, current_user_response, git_mirror_response, issue_collection,
    issue_response, membership_collection, membership_collection_page, mirror_env, one,
    project_collection, project_response, provider, role_collection, role_collection_page,
    sequence, strings, time_entry_activities, time_entry_collection, time_entry_response,
    user_from_response, version_collection, version_collection_page,
};
use crate::auth;
use crate::command::{
    self, Command, IssueCommand, ProjectCommand, RelationCommand, StatusCommand, WorkflowCommand,
};
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::infra::storage::{Storage, TimerRun};
use crate::policy::{Capability, Role};
use crate::providers::redmine::model::{RedmineRelationType, RedmineTimeEntryActivity};
use crate::providers::{
    IssueProvider, ProviderDispatcher, ProviderKind, RedmineConfig, RedmineIssueStatus,
    RedmineMetadataProvider, RedmineProvider,
};
use std::str::FromStr;
use std::{fs, time};

#[test]
fn metadata_parser_requires_confirmation_and_required_fields() {
    let list =
        command::parse_with_role_env(&strings(["project", "list"]), Some("executor")).unwrap();
    assert!(matches!(
        list.command,
        Command::Project(ProjectCommand::List)
    ));

    let status =
        command::parse_with_role_env(&strings(["status", "list"]), Some("reviewer")).unwrap();
    assert!(matches!(
        status.command,
        Command::Status(StatusCommand::List)
    ));

    for args in [
        strings([
            "project",
            "create",
            "--name",
            "Workflow",
            "--identifier",
            "workflow",
        ]),
        strings(["project", "create", "--name", "Workflow", "--confirm"]),
    ] {
        assert!(command::parse_with_role_env(&args, Some("orchestrator")).is_err());
    }

    let create = command::parse_with_role_env(
        &strings([
            "project",
            "create",
            "--name",
            "Workflow",
            "--identifier",
            "workflow",
            "--description",
            "Tracking project",
            "--confirm",
        ]),
        Some("orchestrator"),
    )
    .unwrap();
    assert!(matches!(
        create.command,
        Command::Project(ProjectCommand::Create {
            ref name,
            ref identifier,
            ref description,
            confirmed: true,
        }) if name == "Workflow"
            && identifier == "workflow"
            && description.as_deref() == Some("Tracking project")
    ));

    let bootstrap = command::parse_with_role_env(
        &strings([
            "--provider",
            "redmine",
            "admin",
            "workflow",
            "bootstrap",
            "--repository",
            "Cloud1ful/repo",
            "--close-status-name",
            "Closed",
        ]),
        Some("admin"),
    )
    .unwrap();
    assert!(matches!(
        bootstrap.command,
        Command::Workflow(WorkflowCommand::Bootstrap {
            ref repository,
            ref close_status_name,
            close_status_id: None,
        }) if repository.as_deref() == Some("Cloud1ful/repo")
            && close_status_name.as_deref() == Some("Closed")
    ));

    for (flag, value) in [
        ("--group-name", "AI Agents"),
        ("--group-role", "Developer"),
        ("--group-name=AI Agents", ""),
        ("--group-role=Developer", ""),
    ] {
        let mut args = vec![
            "--provider".to_owned(),
            "redmine".to_owned(),
            "workflow".to_owned(),
            "bootstrap".to_owned(),
        ];
        if value.is_empty() {
            args.push(flag.to_owned());
        } else {
            args.push(flag.to_owned());
            args.push(value.to_owned());
        }
        let error = command::parse_with_role_env(&args, Some("admin"))
            .expect_err("legacy group flag must be rejected");
        assert!(
            error.contains("is no longer supported"),
            "unexpected error for {flag}: {error}"
        );
    }
}

#[test]
fn redmine_dispatches_as_the_default_provider() {
    let redmine = provider("http://redmine.test".to_owned());
    let dispatcher = ProviderDispatcher::Redmine(provider("http://redmine.test".to_owned()));
    assert_eq!(dispatcher.kind(), ProviderKind::Redmine);
    assert_eq!(ProviderKind::default(), ProviderKind::Redmine);
    assert!(redmine.supports(Capability::IssueRead));
    assert!(!redmine.supports(Capability::IssueAttachmentUpload));
}

#[test]
fn project_creation_is_admin_only_and_status_catalogue_is_available() {
    assert!(Role::Admin.allows(Capability::ProjectCreate));
    assert!(Role::Admin.allows(Capability::ProjectRead));
    assert!(Role::Admin.allows(Capability::IssueStatusRead));
    assert!(!Role::Executor.allows(Capability::ProjectCreate));
    assert!(!Role::Reviewer.allows(Capability::ProjectCreate));
    for role in [Role::Executor, Role::Reviewer] {
        assert!(role.allows(Capability::ProjectRead));
        assert!(role.allows(Capability::IssueStatusRead));
    }

    // A retired provider name is rejected at argument parsing (exit 2)
    // before any provider build or network access.
    assert_eq!(
        crate::cli::run_with_role(
            strings(["--provider", "forgejo", "project", "list"]),
            Some("orchestrator")
        ),
        2
    );
    assert_eq!(
        crate::cli::run_with_role(
            strings(["--provider", "gitlab", "project", "list"]),
            Some("orchestrator")
        ),
        2
    );
    assert_eq!(
        crate::cli::run_with_role(
            strings([
                "--provider",
                "redmine",
                "project",
                "create",
                "--name",
                "Workflow",
                "--identifier",
                "workflow",
                "--confirm",
            ]),
            Some("executor")
        ),
        3
    );
    for role in ["executor", "reviewer"] {
        assert_eq!(
            crate::cli::run_with_role(
                strings([
                    "--provider",
                    "redmine",
                    "admin",
                    "workflow",
                    "bootstrap",
                    "--repository",
                    "owner/repo",
                ]),
                Some(role)
            ),
            3
        );
    }
}

#[test]
fn status_set_and_tracker_selection_enforce_role_and_provider_boundaries() {
    // Non-orchestrator roles cannot move an issue's status; the permission
    // error fires before any provider or network access.
    for role in ["admin", "executor", "reviewer"] {
        assert_eq!(
            crate::cli::run_with_role(
                strings([
                    "--provider",
                    "redmine",
                    "status",
                    "set",
                    "3",
                    "--status",
                    "New",
                ]),
                Some(role)
            ),
            3,
            "expected exit 3 for {role} status set"
        );
    }

    // status set is Redmine-only: local has its own status surface, and a
    // retired provider name is rejected before any provider build.
    assert_eq!(
        crate::cli::run_with_role(
            strings([
                "--provider",
                "forgejo",
                "status",
                "set",
                "3",
                "--status",
                "New",
            ]),
            Some("orchestrator")
        ),
        2
    );

    // A retired provider name on a tracker create is rejected at argument
    // parsing before tracker resolution or any provider build.
    assert_eq!(
        crate::cli::run_with_role(
            strings([
                "--provider",
                "gitlab",
                "issue",
                "create",
                "--title",
                "Plan",
                "--tracker",
                "Bug",
            ]),
            Some("orchestrator")
        ),
        2
    );

    let _ = (Role::Orchestrator, Capability::IssueCreate);
}

#[test]
fn issue_attachment_upload_is_uniformly_not_supported() {
    // The uniform `IssueAttachmentUpload = false` row lives on every
    // inherent provider's `supports`. The capability stays in the matrix
    // so a future phase may re-enable the underlying upload path, and the
    // role gate remains (orchestrator / reviewer). The underlying
    // `upload_attachment` inherent method stays compiled for the legacy
    // `contract_tests/attachments.rs` wire-shape tests; no CLI path
    // reaches it because every entry point is gated by
    // `provider.supports(...)`.

    // Role gates stay.
    assert!(Role::Orchestrator.allows(Capability::IssueAttachmentUpload));
    assert!(Role::Reviewer.allows(Capability::IssueAttachmentUpload));
    assert!(!Role::Admin.allows(Capability::IssueAttachmentUpload));
    assert!(!Role::Executor.allows(Capability::IssueAttachmentUpload));

    // Inherent provider surface.
    let redmine = provider("http://redmine.test".to_owned());
    assert!(!redmine.supports(Capability::IssueAttachmentUpload));

    // Dispatcher surface: thin forwarder; the uniform row is enforced by
    // the inherent providers.
    let redmine_dispatcher =
        ProviderDispatcher::Redmine(provider("http://redmine.test".to_owned()));
    assert!(!redmine_dispatcher.supports(Capability::IssueAttachmentUpload));

    // Role gate still fires before the dispatcher guard.
    for role in ["admin", "executor"] {
        assert_eq!(
            crate::cli::run_with_role(
                strings([
                    "--provider",
                    "redmine",
                    "issue",
                    "upload-attachment",
                    "5",
                    "--path",
                    "/tmp/any.txt"
                ]),
                Some(role)
            ),
            3,
            "role {role} must hit the permission gate first"
        );
    }

    // The reviewer is allowed by the role gate but the inherent provider
    // rejects uniformly.
    assert_eq!(
        crate::cli::run_with_role(
            strings([
                "--provider",
                "redmine",
                "issue",
                "upload-attachment",
                "5",
                "--path",
                "/tmp/any.txt",
            ]),
            Some("reviewer"),
        ),
        1,
        "reviewer upload-attachment must be the uniform not-supported result"
    );
}

#[test]
fn reviewer_least_privilege_matrix() {
    assert!(Role::Reviewer.allows(Capability::IssueRead));
    assert!(Role::Reviewer.allows(Capability::CommentRead));
    assert!(Role::Reviewer.allows(Capability::CommentFindMarker));
    assert!(Role::Reviewer.allows(Capability::CommentCreate));
    assert!(Role::Reviewer.allows(Capability::IssueAttachmentUpload));
    assert!(!Role::Reviewer.allows(Capability::IssueSearch));
    assert!(!Role::Reviewer.allows(Capability::IssueCreate));
    assert!(!Role::Reviewer.allows(Capability::IssueUpdateBody));
    assert!(!Role::Reviewer.allows(Capability::IssueClose));
    assert!(!Role::Reviewer.allows(Capability::ProjectCreate));
    assert!(!Role::Reviewer.allows(Capability::RelationCreate));
    assert!(!Role::Reviewer.allows(Capability::RelationDelete));
    // CLI enforcement: reviewer cannot search/create/update/close or bootstrap or repo create
    for (args, expected) in [
        (strings(["--provider", "redmine", "issue", "search"]), 3),
        (
            strings([
                "--provider",
                "redmine",
                "issue",
                "create",
                "--title",
                "t",
                "--body",
                "b",
            ]),
            3,
        ),
        (
            strings([
                "--provider",
                "redmine",
                "issue",
                "update",
                "1",
                "--body",
                "x",
            ]),
            3,
        ),
        (strings(["--provider", "redmine", "issue", "close", "1"]), 3),
        (
            strings([
                "--provider",
                "redmine",
                "admin",
                "workflow",
                "bootstrap",
                "--repository",
                "owner/repo",
            ]),
            3,
        ),
    ] {
        assert_eq!(crate::cli::run_with_role(args, Some("reviewer")), expected);
    }
    // Reviewer can read issues/comments
    assert_eq!(
        crate::cli::run_with_role(
            strings([
                "--provider",
                "redmine",
                "comment",
                "create",
                "1",
                "--body",
                "<!-- m --> hi",
                "--marker",
                "<!-- m -->"
            ]),
            Some("reviewer")
        ),
        2,
        "reviewer comment without --authorized must be authorization error, not permission"
    );
}
