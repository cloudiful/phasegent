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
fn relation_parser_accepts_canonical_types_and_rejects_invalid() {
    let parsed = command::parse(&strings([
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "relation",
        "create",
        "10",
        "--to",
        "20",
        "--type",
        "blocks",
    ]))
    .expect("valid relation create should parse");
    assert!(matches!(
        parsed.command,
        Command::Relation(RelationCommand::Create {
            issue: 10,
            to: 20,
            relation_type: RedmineRelationType::Blocks,
            delay: None
        })
    ));

    let with_delay = command::parse(&strings([
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "relation",
        "create",
        "10",
        "--to",
        "20",
        "--type",
        "precedes",
        "--delay",
        "5",
    ]))
    .expect("valid relation create with delay should parse");
    assert!(matches!(
        with_delay.command,
        Command::Relation(RelationCommand::Create {
            issue: 10,
            to: 20,
            relation_type: RedmineRelationType::Precedes,
            delay: Some(5)
        })
    ));

    // Inverse names are rejected as CLI input.
    assert!(
        command::parse(&strings([
            "--role",
            "orchestrator",
            "--provider",
            "redmine",
            "relation",
            "create",
            "10",
            "--to",
            "20",
            "--type",
            "blocked",
        ]))
        .is_err()
    );

    // Unknown type is rejected.
    assert!(
        command::parse(&strings([
            "--role",
            "orchestrator",
            "--provider",
            "redmine",
            "relation",
            "create",
            "10",
            "--to",
            "20",
            "--type",
            "weird",
        ]))
        .is_err()
    );

    // Missing --to and missing --type are rejected.
    assert!(
        command::parse(&strings([
            "--role",
            "orchestrator",
            "--provider",
            "redmine",
            "relation",
            "create",
            "10",
            "--type",
            "blocks",
        ]))
        .is_err()
    );
}

#[test]
fn relation_help_prints_usage_and_exits_cleanly() {
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "redmine",
            "relation",
            "--help"
        ])),
        0
    );
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "redmine",
            "relation",
            "create",
            "--help"
        ])),
        0
    );
}

#[test]
fn relation_commands_enforce_role_and_provider_boundaries() {
    // relation list is allowed for orchestrator/executor/reviewer (the
    // permission check passes and parsing succeeds); admin is denied and
    // Forgejo is rejected before any provider is built.
    for role in ["orchestrator", "executor", "reviewer"] {
        let parsed = command::parse(&strings([
            "--role",
            role,
            "--provider",
            "redmine",
            "relation",
            "list",
            "10",
        ]))
        .unwrap();
        assert!(matches!(
            parsed.command,
            Command::Relation(RelationCommand::List { issue: 10 })
        ));
    }
    // admin is denied relation list before any network/provider access.
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "admin",
            "--provider",
            "redmine",
            "relation",
            "list",
            "10",
        ])),
        3
    );
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "forgejo",
            "relation",
            "list",
            "10",
        ])),
        1
    );

    // relation create/delete are orchestrator-only; non-orchestrator roles
    // are denied before any provider is built.
    for role in ["admin", "executor", "reviewer"] {
        assert_eq!(
            crate::cli::run(strings([
                "--role",
                role,
                "--provider",
                "redmine",
                "relation",
                "create",
                "10",
                "--to",
                "20",
                "--type",
                "blocks",
            ])),
            3,
            "expected permission error for {role} relation create"
        );
        assert_eq!(
            crate::cli::run(strings([
                "--role",
                role,
                "--provider",
                "redmine",
                "relation",
                "delete",
                "5",
            ])),
            3,
            "expected permission error for {role} relation delete"
        );
    }
    // Forgejo rejects relation create/delete with a structured not-supported
    // error before any network access.
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "forgejo",
            "relation",
            "create",
            "10",
            "--to",
            "20",
            "--type",
            "blocks",
        ])),
        1
    );
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "forgejo",
            "relation",
            "delete",
            "5",
        ])),
        1
    );
}

#[test]
fn relation_create_denies_delay_for_non_precedes_types() {
    let _environment_lock = lock_workflow_tests();
    let directory = std::env::temp_dir().join(format!(
        "phasegent-redmine-relation-delay-{}-{}",
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
        .save_credential(Role::Orchestrator, "redmine", TEST_API_KEY)
        .unwrap();
    // `--delay` with `--type blocks` must fail locally before any request.
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "redmine",
            "--api-base",
            "http://127.0.0.1:1",
            "relation",
            "create",
            "10",
            "--to",
            "20",
            "--type",
            "blocks",
            "--delay",
            "3",
        ])),
        1
    );
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn relation_create_and_list_hit_redmine_endpoints_end_to_end() {
    let _environment_lock = lock_workflow_tests();
    let directory = std::env::temp_dir().join(format!(
        "phasegent-redmine-relation-e2e-{}-{}",
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
        .save_credential(Role::Orchestrator, "redmine", TEST_API_KEY)
        .unwrap();

    let (base, requests, server) = sequence(vec![
        MockResponse::ok(
            serde_json::json!({
                "relation": {"id": 9, "issue_id": 10, "issue_to_id": 20, "relation_type": "blocks", "delay": 0}
            })
            .to_string(),
        ),
        MockResponse::ok(
            serde_json::json!({
                "relations": [
                    {"id": 9, "issue_id": 10, "issue_to_id": 20, "relation_type": "blocks", "delay": 0}
                ]
            })
            .to_string(),
        ),
    ]);

    // Create then list, both against the mock Redmine server.
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "redmine",
            "--api-base",
            &base,
            "relation",
            "create",
            "10",
            "--to",
            "20",
            "--type",
            "blocks",
        ])),
        0
    );
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "redmine",
            "--api-base",
            &base,
            "relation",
            "list",
            "10",
        ])),
        0
    );

    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    support::assert_request(&requests[0], "POST", "/issues/10/relations.json", None);
    support::assert_request(&requests[1], "GET", "/issues/10/relations.json", None);
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}
