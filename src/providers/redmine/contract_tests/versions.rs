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
use crate::ci_model::CiRunsFilter;
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
fn version_list_decodes_redmine_wrapper_within_project_scope() {
    let response = support::version_collection(&[
        (12, "Sprint 1", "open", Some("2026-09-30")),
        (13, "Backlog", "open", None),
    ]);
    let (result, request) = one(MockResponse::ok(response), |redmine| {
        redmine.list_versions()
    });
    let versions = result.unwrap();
    assert_eq!(
        versions
            .iter()
            .map(|version| (version.id, version.name.as_str()))
            .collect::<Vec<_>>(),
        [(12, "Sprint 1"), (13, "Backlog")]
    );
    assert_eq!(versions[0].status, "open");
    assert_eq!(versions[0].due_date.as_deref(), Some("2026-09-30"));
    support::assert_request(&request, "GET", "/projects/42/versions.json?", None);
    assert!(request.contains("limit=100"));
}

#[test]
fn version_selection_resolves_name_id_and_rejects_bad_values() {
    let versions = vec![
        crate::providers::redmine::model::RedmineVersion {
            id: 12,
            name: "Sprint 1".to_owned(),
            status: "open".to_owned(),
            due_date: None,
        },
        crate::providers::redmine::model::RedmineVersion {
            id: 14,
            name: "Sprint 1".to_owned(),
            status: "closed".to_owned(),
            due_date: None,
        },
    ];
    assert_eq!(
        RedmineProvider::select_version(&[versions[0].clone()], "12")
            .unwrap()
            .id,
        12
    );
    // Duplicate names are ambiguous even though ids are unique.
    assert_eq!(
        RedmineProvider::select_version(&versions, "Sprint 1")
            .unwrap_err()
            .json()["kind"],
        "config"
    );
    assert_eq!(
        RedmineProvider::select_version(&versions, "Missing")
            .unwrap_err()
            .json()["kind"],
        "config"
    );
    assert_eq!(
        RedmineProvider::select_version(&versions, "0")
            .unwrap_err()
            .json()["kind"],
        "config"
    );
    assert_eq!(
        RedmineProvider::select_version(&versions, "99")
            .unwrap_err()
            .json()["kind"],
        "config"
    );
}

#[test]
fn version_list_paginates_and_selects_versions_on_later_pages() {
    let first_page = support::version_collection_page(
        3,
        2,
        &[
            (12, "Sprint 1", "open", None),
            (13, "Sprint 2", "open", None),
        ],
    );
    let second_page = support::version_collection_page(3, 2, &[(14, "Backlog", "closed", None)]);
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(first_page),
        MockResponse::ok(second_page),
    ]);
    let redmine = provider(base);
    let versions = redmine.list_versions().unwrap();
    assert_eq!(
        versions
            .iter()
            .map(|version| (version.id, version.name.as_str()))
            .collect::<Vec<_>>(),
        [(12, "Sprint 1"), (13, "Sprint 2"), (14, "Backlog")]
    );
    // A version that only exists on the second page must be selectable so
    // --fixed-version resolution cannot falsely fail on large roadmaps.
    assert_eq!(
        RedmineProvider::select_version(&versions, "14").unwrap().id,
        14
    );
    assert_eq!(
        RedmineProvider::select_version(&versions, "Backlog")
            .unwrap()
            .id,
        14
    );

    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    for (request, offset) in requests.iter().zip(["0", "2"]) {
        support::assert_request(request, "GET", "/projects/42/versions.json?", None);
        assert!(request.contains(&format!("offset={offset}")));
        // The client always requests its own page size; pagination advances
        // by the number of items actually returned.
        assert!(request.contains("limit=100"));
    }
    server.join().unwrap();
}

#[test]
fn version_list_enforces_role_and_provider_boundaries() {
    // Every role may list versions on Redmine...
    for role in ["admin", "orchestrator", "executor", "reviewer"] {
        let parsed = command::parse(&strings([
            "--role",
            role,
            "--provider",
            "redmine",
            "version",
            "list",
        ]))
        .unwrap();
        assert!(matches!(
            parsed.command,
            Command::VersionCommand(crate::command::VersionCommand::List)
        ));
    }
    // ...while Forgejo is rejected with a not-supported error before any
    // provider is built.
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "forgejo",
            "version",
            "list",
        ])),
        1
    );
}
