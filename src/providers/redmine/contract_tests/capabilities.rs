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
fn redmine_keeps_repo_command_unsupported() {
    let redmine = provider("http://redmine.test".to_owned());
    assert!(!redmine.supports(Capability::RepoCreate));
    assert_eq!(
        redmine
            .create_repo("owner/repo", true, "", false)
            .unwrap_err()
            .json()["kind"],
        "not_supported"
    );
    let dispatcher = ProviderDispatcher::Redmine(provider("http://redmine.test".to_owned()));
    assert_eq!(dispatcher.kind(), ProviderKind::Redmine);
}

#[test]
fn project_creation_is_admin_only_and_read_is_role_invariant() {
    assert!(Role::Admin.allows(Capability::ProjectCreate));
    assert!(Role::Admin.allows(Capability::ProjectRead));
    assert!(Role::Admin.allows(Capability::IssueStatusRead));
    assert!(!Role::Executor.allows(Capability::ProjectCreate));
    assert!(!Role::Reviewer.allows(Capability::ProjectCreate));
    for role in [Role::Executor, Role::Reviewer] {
        assert!(role.allows(Capability::ProjectRead));
        assert!(role.allows(Capability::IssueStatusRead));
    }

    // A removed `--provider` value never reaches execution: the parser
    // rejects it with a usage error before any provider or role check.
    assert_eq!(
        crate::cli::run_with_role(
            strings(["--provider", "forgejo", "project", "list"]),
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
    // A removed `--provider` value is rejected at parse time (exit 2),
    // before the admin role gate.
    assert_eq!(
        crate::cli::run_with_role(
            strings([
                "--provider",
                "forgejo",
                "admin",
                "workflow",
                "bootstrap",
                "--repository",
                "owner/repo",
            ]),
            Some("orchestrator")
        ),
        2
    );
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

    // A removed `--provider` value is rejected at parse time (exit 2),
    // before any role or provider check.
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

    // A stale selection through the resolver default (an environment or
    // persisted stale default) fails closed with an actionable
    // config error: no other provider is selected implicitly and no
    // credential is read. The stored row is left for explicit
    // clear/replace.
    let _environment_lock = lock_workflow_tests();
    let _default_env = EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", "forgejo");
    let error = match crate::cli::provider_for(Role::Orchestrator, None, None, None, None) {
        Ok(_) => panic!("a defaulted stale selection must fail closed"),
        Err(error) => error,
    };
    assert_eq!(error.json()["kind"], "config");
    assert!(
        error.json()["message"]
            .as_str()
            .unwrap_or_default()
            .contains("forgejo"),
        "stale default must name the value: {error:?}"
    );
}

#[test]
fn issue_attachment_upload_is_uniformly_not_supported_at_phase_4_sink() {
    // Phase 4 parity matrix (issue 257): the uniform
    // `IssueAttachmentUpload = false` row now lives on every inherent
    // provider's `supports`, including Redmine. The dispatcher arm is
    // a thin forwarder and no longer carries a separate guard. The
    // capability stays in the matrix so a future phase may re-enable
    // the underlying upload path, and the role gate remains
    // (orchestrator / tester). The underlying `upload_attachment`
    // inherent method stays compiled for the legacy
    // `contract_tests/attachments.rs` wire-shape tests; no CLI/MCP
    // path reaches it because every entry point is gated by
    // `provider.supports(...)`.

    // Role gates stay.
    assert!(Role::Orchestrator.allows(Capability::IssueAttachmentUpload));
    assert!(Role::Tester.allows(Capability::IssueAttachmentUpload));
    assert!(!Role::Admin.allows(Capability::IssueAttachmentUpload));
    assert!(!Role::Executor.allows(Capability::IssueAttachmentUpload));
    assert!(!Role::Reviewer.allows(Capability::IssueAttachmentUpload));

    // Inherent provider surfaces (Phase 4 sinking): every provider
    // reports `false` so the dispatcher arm does not need a separate
    // override.
    let redmine = provider("http://redmine.test".to_owned());
    assert!(!redmine.supports(Capability::IssueAttachmentUpload));
    let gitlab = crate::providers::gitlab::GitlabProvider::new(
        crate::providers::config::GitlabConfig::new("https://gitlab.example/api/v4", 42),
        "token".to_owned(),
    )
    .unwrap();
    assert!(!gitlab.supports(Capability::IssueAttachmentUpload));

    // Dispatcher surface: thin forwarder; the uniform row is enforced
    // by the inherent providers.
    let redmine_dispatcher =
        ProviderDispatcher::Redmine(provider("http://redmine.test".to_owned()));
    assert!(!redmine_dispatcher.supports(Capability::IssueAttachmentUpload));
    let gitlab_dispatcher = ProviderDispatcher::Gitlab(
        crate::providers::gitlab::GitlabProvider::new(
            crate::providers::config::GitlabConfig::new("https://gitlab.example/api/v4", 42),
            "token".to_owned(),
        )
        .unwrap(),
    );
    assert!(!gitlab_dispatcher.supports(Capability::IssueAttachmentUpload));

    // Role gate still fires before the dispatcher guard.
    for role in ["admin", "executor", "reviewer"] {
        for provider in ["redmine", "gitlab"] {
            assert_eq!(
                crate::cli::run_with_role(
                    strings([
                        "--provider",
                        provider,
                        "issue",
                        "upload-attachment",
                        "5",
                        "--path",
                        "/tmp/any.txt"
                    ]),
                    Some(role)
                ),
                3,
                "role {role} on {provider} must hit the permission gate first"
            );
        }
    }

    // Every provider reports not-supported (exit 1). Tester is
    // allowed by the role gate but the inherent provider rejects
    // uniformly.
    for provider in ["redmine", "gitlab"] {
        let exit = crate::cli::run_with_role(
            strings([
                "--provider",
                provider,
                "issue",
                "upload-attachment",
                "5",
                "--path",
                "/tmp/any.txt",
            ]),
            Some("tester"),
        );
        assert_eq!(
            exit, 1,
            "tester upload-attachment on {provider} must be not_supported (Phase 4 sinking)"
        );
    }
}

#[test]
fn tester_least_privilege_matrix() {
    assert!(Role::Tester.allows(Capability::IssueRead));
    assert!(Role::Tester.allows(Capability::CommentRead));
    assert!(Role::Tester.allows(Capability::CommentFindMarker));
    assert!(Role::Tester.allows(Capability::CommentCreate));
    assert!(Role::Tester.allows(Capability::IssueAttachmentUpload));
    assert!(!Role::Tester.allows(Capability::IssueSearch));
    assert!(!Role::Tester.allows(Capability::IssueCreate));
    assert!(!Role::Tester.allows(Capability::IssueUpdateBody));
    assert!(!Role::Tester.allows(Capability::IssueClose));
    assert!(!Role::Tester.allows(Capability::RepoCreate));
    assert!(!Role::Tester.allows(Capability::ProjectRead));
    assert!(!Role::Tester.allows(Capability::ProjectCreate));
    assert!(!Role::Tester.allows(Capability::IssueStatusRead));
    assert!(!Role::Tester.allows(Capability::VersionRead));
    assert!(!Role::Tester.allows(Capability::RelationRead));
    assert!(!Role::Tester.allows(Capability::RelationCreate));
    assert!(!Role::Tester.allows(Capability::RelationDelete));
    // CLI enforcement: tester cannot search/create/update/close or bootstrap or repo create
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
        (strings(["repo", "create", "owner/repo", "--private"]), 3),
    ] {
        assert_eq!(crate::cli::run_with_role(args, Some("tester")), expected);
    }
    // Tester can read issues/comments
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
            Some("tester")
        ),
        2,
        "tester comment without --authorized must be authorization error, not permission"
    );
}
