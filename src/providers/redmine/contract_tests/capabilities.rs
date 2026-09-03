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
    ProviderDispatcher, ProviderKind, RedmineConfig, RedmineIssueStatus, RedmineMetadataProvider,
    RedmineProvider,
};
use std::str::FromStr;
use std::{fs, time};

#[test]
fn metadata_parser_requires_confirmation_and_required_fields() {
    let list = command::parse(&strings(["--role", "executor", "project", "list"])).unwrap();
    assert!(matches!(
        list.command,
        Command::Project(ProjectCommand::List)
    ));

    let status = command::parse(&strings(["--role", "reviewer", "status", "list"])).unwrap();
    assert!(matches!(
        status.command,
        Command::Status(StatusCommand::List)
    ));

    for args in [
        strings([
            "--role",
            "orchestrator",
            "project",
            "create",
            "--name",
            "Workflow",
            "--identifier",
            "workflow",
        ]),
        strings([
            "--role",
            "orchestrator",
            "project",
            "create",
            "--name",
            "Workflow",
            "--confirm",
        ]),
    ] {
        assert!(command::parse(&args).is_err());
    }

    let create = command::parse(&strings([
        "--role",
        "orchestrator",
        "project",
        "create",
        "--name",
        "Workflow",
        "--identifier",
        "workflow",
        "--description",
        "Tracking project",
        "--confirm",
    ]))
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

    let bootstrap = command::parse(&strings([
        "--role",
        "admin",
        "--provider",
        "redmine",
        "workflow",
        "bootstrap",
        "--repository",
        "Cloud1ful/repo",
        "--close-status-name",
        "Closed",
    ]))
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
            "--role".to_owned(),
            "admin".to_owned(),
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
        let error = command::parse(&args).expect_err("legacy group flag must be rejected");
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
fn project_creation_is_admin_only_and_forgejo_metadata_is_unsupported() {
    assert!(Role::Admin.allows(Capability::ProjectCreate));
    assert!(Role::Admin.allows(Capability::ProjectRead));
    assert!(Role::Admin.allows(Capability::IssueStatusRead));
    assert!(!Role::Executor.allows(Capability::ProjectCreate));
    assert!(!Role::Reviewer.allows(Capability::ProjectCreate));
    for role in [Role::Executor, Role::Reviewer] {
        assert!(role.allows(Capability::ProjectRead));
        assert!(role.allows(Capability::IssueStatusRead));
    }

    let forgejo = crate::providers::forgejo::ForgejoProvider::new(
        crate::providers::forgejo::ForgejoConfig::new("http://forgejo.test", "owner", "repo"),
        "token".to_owned(),
    )
    .unwrap();
    assert_eq!(
        forgejo.list_projects().unwrap_err().json()["kind"],
        "not_supported"
    );
    assert_eq!(
        forgejo.list_issue_statuses().unwrap_err().json()["kind"],
        "not_supported"
    );

    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "forgejo",
            "project",
            "list"
        ])),
        1
    );
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "executor",
            "--provider",
            "redmine",
            "project",
            "create",
            "--name",
            "Workflow",
            "--identifier",
            "workflow",
            "--confirm",
        ])),
        3
    );
    for role in ["executor", "reviewer"] {
        assert_eq!(
            crate::cli::run(strings([
                "--role",
                role,
                "--provider",
                "redmine",
                "workflow",
                "bootstrap",
                "--repository",
                "owner/repo",
            ])),
            3
        );
    }
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "forgejo",
            "workflow",
            "bootstrap",
            "--repository",
            "owner/repo",
        ])),
        3
    );
}

#[test]
fn status_set_and_tracker_selection_enforce_role_and_provider_boundaries() {
    // Non-orchestrator roles cannot move an issue's status; the permission
    // error fires before any provider or network access.
    for role in ["admin", "executor", "reviewer"] {
        assert_eq!(
            crate::cli::run(strings([
                "--role",
                role,
                "--provider",
                "redmine",
                "status",
                "set",
                "3",
                "--status",
                "New",
            ])),
            3,
            "expected exit 3 for {role} status set"
        );
    }

    // status set is Redmine-only: Forgejo is rejected as unsupported.
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "forgejo",
            "status",
            "set",
            "3",
            "--status",
            "New",
        ])),
        1
    );

    // Tracker selection on create/update-body is Redmine-only. A stored
    // forgejo token lets the dispatcher build so the rejection comes from
    // tracker resolution, not from missing credentials; no request is made.
    let _environment_lock = lock_workflow_tests();
    let directory = std::env::temp_dir().join(format!(
        "phasegent-tracker-boundary-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db_path = directory.join(crate::infra::storage::DB_FILENAME);
    let _db_path_guard = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
    let storage = Storage::open_at(&db_path).unwrap();
    storage
        .save_credential(Role::Orchestrator, "forgejo", "test-forgejo-token")
        .unwrap();

    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--api-base",
            "http://forgejo.test",
            "--repository",
            "owner/repo",
            "issue",
            "create",
            "--title",
            "Plan",
            "--tracker",
            "Bug",
        ])),
        1
    );
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--api-base",
            "http://forgejo.test",
            "--repository",
            "owner/repo",
            "issue",
            "update-body",
            "9",
            "--body",
            "Updated",
            "--tracker",
            "Bug",
        ])),
        1
    );
}
