//! Distinct-identity guard for bootstrap provisioning.
//!
//! [`collisions`] covers the abort paths, [`success`] the four-role happy
//! path where every persisted identity stays distinct.

mod collisions;
mod success;

use crate::auth;
use crate::infra::storage::Storage;
use crate::infra::storage::test_support::EnvGuard;
use crate::policy::Role;
use crate::providers::redmine::contract_tests::support::MockResponse;
use std::time;

/// Deterministic service identities in provisioning order.
pub(super) const SERVICE_ROLES: [(Role, u64, &str); 4] = [
    (Role::Orchestrator, 11, "phasegent-orchestrator"),
    (Role::Executor, 22, "phasegent-executor"),
    (Role::Reviewer, 33, "phasegent-reviewer"),
    (Role::Explore, 44, "phasegent-explore"),
];

pub(super) fn temp_db(label: &str) -> (std::path::PathBuf, EnvGuard, Storage) {
    let directory = crate::test_scratch::root().join(format!(
        "phasegent-redmine-distinct-{label}-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db_path = directory.join(crate::infra::storage::DB_FILENAME);
    let guard = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
    let storage = Storage::open_at(&db_path).unwrap();
    (directory, guard, storage)
}

pub(super) fn seed_admin(storage: &Storage, base: &str) {
    storage
        .save_credential(Role::Admin, "redmine", "admin-redmine-key")
        .unwrap();
    storage
        .save_redmine_config(
            Role::Admin,
            &auth::RedmineStoredConfig {
                api_base: Some(base.to_owned()),
                project_id: None,
                close_status_id: None,
                group_name: None,
                group_role: None,
            },
        )
        .unwrap();
}

pub(super) fn seed_persisted(storage: &Storage, role: Role, id: u64, login: &str, key: &str) {
    storage.save_redmine_user(role, id, login).unwrap();
    storage.save_credential(role, "redmine", key).unwrap();
}

/// The three responses every collision aborts on: the project lookup miss,
/// the issue-status list, and the project creation.
pub(super) fn project_bootstrap_responses(project: MockResponse) -> Vec<MockResponse> {
    vec![
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        MockResponse::ok(
            serde_json::json!({"issue_statuses": [{"id": 5, "name": "Closed", "is_closed": true}]})
                .to_string(),
        ),
        project,
    ]
}

pub(super) fn assert_no_membership_or_user_calls(requests: &[String]) {
    for (index, request) in requests.iter().enumerate() {
        assert!(
            !request.starts_with("POST /projects/44/memberships.json"),
            "no membership POST should fire on distinct-user failure (index {index}): {request}"
        );
        assert!(
            !request.starts_with("PUT /memberships/"),
            "no membership PUT should fire on distinct-user failure (index {index}): {request}"
        );
        assert!(
            !request.contains("/users.json") && !request.contains("/users/"),
            "no user API should fire when persisted users are reused (index {index}): {request}"
        );
    }
}
