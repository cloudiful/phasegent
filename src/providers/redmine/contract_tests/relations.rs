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
fn relation_list_uses_redmine_relations_endpoint_and_resolves_viewpoint() {
    let body = serde_json::json!({
        "relations": [
            {"id": 1, "issue_id": 10, "issue_to_id": 20, "relation_type": "blocks", "delay": 0},
            {"id": 2, "issue_id": 30, "issue_to_id": 10, "relation_type": "blocks", "delay": 0},
            {"id": 3, "issue_id": 10, "issue_to_id": 40, "relation_type": "precedes", "delay": 3},
            {"id": 4, "issue_id": 50, "issue_to_id": 10, "relation_type": "relates", "delay": 0}
        ]
    })
    .to_string();
    let (result, request) = one(MockResponse::ok(body), |redmine| redmine.list_relations(10));
    let relations = result.unwrap();
    assert_eq!(relations.len(), 4);
    // The queried issue is the relation's source: type stays canonical.
    assert_eq!(relations[0].id, 1);
    assert_eq!(relations[0].relation_type, "blocks");
    assert_eq!(relations[0].issue_id, 10);
    assert_eq!(relations[0].issue_to_id, 20);
    // The queried issue is the relation's target: blocks reads as blocked.
    assert_eq!(relations[1].id, 2);
    assert_eq!(relations[1].relation_type, "blocked");
    assert_eq!(relations[1].issue_id, 30);
    // precedes keeps its delay from the queried (source) side.
    assert_eq!(relations[2].id, 3);
    assert_eq!(relations[2].relation_type, "precedes");
    assert_eq!(relations[2].delay, Some(3));
    // relates is symmetric.
    assert_eq!(relations[3].id, 4);
    assert_eq!(relations[3].relation_type, "relates");
    support::assert_request(&request, "GET", "/issues/10/relations.json", None);
}

#[test]
fn relation_create_posts_canonical_type_and_omits_delay_for_blocks() {
    let body = serde_json::json!({
        "relation": {"id": 5, "issue_id": 10, "issue_to_id": 20, "relation_type": "blocks", "delay": 0}
    })
    .to_string();
    let (result, request) = one(MockResponse::ok(body), |redmine| {
        redmine.create_relation(10, 20, RedmineRelationType::Blocks, None)
    });
    let summary = result.unwrap();
    assert_eq!(summary.id, 5);
    assert_eq!(summary.relation_type, "blocks");
    support::assert_request(&request, "POST", "/issues/10/relations.json", None);
    assert!(
        request.contains(r#""relation":{"issue_to_id":20,"relation_type":"blocks"}"#),
        "unexpected relation create body: {request}"
    );
    assert!(
        !request.contains("delay"),
        "delay must be omitted for blocks"
    );
}

#[test]
fn relation_create_serializes_delay_only_for_precedes() {
    let body = serde_json::json!({
        "relation": {"id": 6, "issue_id": 10, "issue_to_id": 20, "relation_type": "precedes", "delay": 5}
    })
    .to_string();
    let (result, request) = one(MockResponse::ok(body), |redmine| {
        redmine.create_relation(10, 20, RedmineRelationType::Precedes, Some(5))
    });
    let summary = result.unwrap();
    assert_eq!(summary.relation_type, "precedes");
    assert_eq!(summary.delay, Some(5));
    support::assert_request(&request, "POST", "/issues/10/relations.json", None);
    assert!(
        request.contains(r#""relation":{"issue_to_id":20,"relation_type":"precedes","delay":5}"#),
        "unexpected relation create body: {request}"
    );
}

#[test]
fn relation_delete_uses_delete_on_the_relation_endpoint() {
    let (base, requests, server) = sequence(vec![MockResponse::ok("")]);
    let redmine = provider(base);
    assert!(redmine.delete_relation(7).is_ok());
    let request = requests.recv().unwrap().remove(0);
    support::assert_request(&request, "DELETE", "/relations/7.json", None);
    server.join().unwrap();
}

#[test]
fn relation_type_parse_input_accepts_only_canonical_names() {
    assert_eq!(
        RedmineRelationType::parse_input("blocks").unwrap(),
        RedmineRelationType::Blocks
    );
    assert_eq!(
        RedmineRelationType::parse_input("precedes").unwrap(),
        RedmineRelationType::Precedes
    );
    assert_eq!(
        RedmineRelationType::parse_input("relates").unwrap(),
        RedmineRelationType::Relates
    );
    // Inverse names are rejected as input so a relation can never be created
    // backwards.
    assert!(RedmineRelationType::parse_input("blocked").is_err());
    assert!(RedmineRelationType::parse_input("follows").is_err());
    assert!(RedmineRelationType::parse_input("weird").is_err());
}

#[test]
fn relation_type_parse_decodes_server_inverse_names() {
    assert_eq!(
        RedmineRelationType::parse("blocked").unwrap(),
        RedmineRelationType::Blocked
    );
    assert_eq!(
        RedmineRelationType::parse("follows").unwrap(),
        RedmineRelationType::Follows
    );
    assert!(RedmineRelationType::parse("unknown").is_err());
}

#[test]
fn relation_type_inverse_is_symmetric() {
    assert_eq!(
        RedmineRelationType::Blocks.inverse(),
        RedmineRelationType::Blocked
    );
    assert_eq!(
        RedmineRelationType::Blocked.inverse(),
        RedmineRelationType::Blocks
    );
    assert_eq!(
        RedmineRelationType::Precedes.inverse(),
        RedmineRelationType::Follows
    );
    assert_eq!(
        RedmineRelationType::Follows.inverse(),
        RedmineRelationType::Precedes
    );
    assert_eq!(
        RedmineRelationType::Relates.inverse(),
        RedmineRelationType::Relates
    );
}
