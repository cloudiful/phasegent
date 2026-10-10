//! Automatic workflow bootstrap contract tests.
//!
//! [`issue_flow`] covers the implicit bootstrap behind `issue create`,
//! [`reuse`] the login-based reuse of already existing service users,
//! [`rerun`] the fully cached second run, and [`guardrails`] the credential
//! and identity boundaries. The HTTP script helpers here are shared so every
//! child describes the same four-role provisioning order.

mod guardrails;
mod issue_flow;
mod rerun;
mod reuse;

use crate::auth;
use crate::infra::storage::Storage;
use crate::infra::storage::test_support::EnvGuard;
use crate::policy::Role;
use crate::providers::redmine::contract_tests::support::{
    MockResponse, git_mirror_response, membership_collection, project_response, role_collection,
};
use std::time;

/// Roles in provisioning order with their deterministic ids, mirroring
/// `provisioned_roles()`.
pub(super) const SERVICE_ROLES: [(Role, u64, &str); 4] = [
    (Role::Orchestrator, 11, "phasegent-orchestrator"),
    (Role::Executor, 22, "phasegent-executor"),
    (Role::Reviewer, 33, "phasegent-reviewer"),
    (Role::Explore, 44, "phasegent-explore"),
];

pub(super) fn user_list_empty() -> String {
    serde_json::json!({
        "users": [],
        "total_count": 0,
        "limit": 100,
    })
    .to_string()
}

pub(super) fn user_list_with(users: &[(u64, &str)]) -> String {
    serde_json::json!({
        "users": users.iter().map(|(id, login)| serde_json::json!({
            "id": id,
            "login": login,
            "firstname": "Phasegent",
            "lastname": login,
            "mail": format!("{login}@phasegent.local"),
        })).collect::<Vec<_>>(),
        "total_count": users.len(),
        "limit": 100,
    })
    .to_string()
}

fn user_create_response(id: u64, login: &str) -> String {
    serde_json::json!({
        "user": {
            "id": id,
            "login": login,
            "firstname": "Phasegent",
            "lastname": login,
            "mail": format!("{login}@phasegent.local"),
        }
    })
    .to_string()
}

pub(super) fn user_get_with_key(id: u64, login: &str, api_key: &str) -> String {
    serde_json::json!({
        "user": {
            "id": id,
            "login": login,
            "firstname": "Phasegent",
            "lastname": login,
            "mail": format!("{login}@phasegent.local"),
            "api_key": api_key,
        }
    })
    .to_string()
}

pub(super) fn closed_status() -> String {
    serde_json::json!({
        "issue_statuses": [{"id": 5, "name": "Closed", "is_closed": true}]
    })
    .to_string()
}

pub(super) fn bootstrap_project() -> String {
    project_response(
        44,
        "owner/repo",
        "owner-repo",
        "Workflow issues for owner/repo",
    )
}

/// Responses for provisioning one role from scratch: empty login lookup,
/// service-user create, then the admin API-key read.
pub(super) fn create_role(id: u64, login: &str, api_key: &str) -> Vec<MockResponse> {
    vec![
        MockResponse::ok(user_list_empty()),
        MockResponse::status(201, user_create_response(id, login)),
        MockResponse::ok(user_get_with_key(id, login, api_key)),
    ]
}

/// Responses for provisioning one role that already exists server-side: the
/// login lookup hits and only the admin API-key read follows.
pub(super) fn existing_role(list: &str, id: u64, login: &str, api_key: &str) -> Vec<MockResponse> {
    vec![
        MockResponse::ok(list.to_owned()),
        MockResponse::ok(user_get_with_key(id, login, api_key)),
    ]
}

/// Mock responses for one fresh membership write.
pub(super) fn membership_added() -> Vec<MockResponse> {
    vec![
        MockResponse::ok(service_roles_payload()),
        MockResponse::ok(membership_collection(None)),
        MockResponse::ok("{}"),
    ]
}

fn service_roles_payload() -> String {
    role_collection(&[(3, "Maintainer"), (4, "Developer"), (5, "Reporter")])
}

/// Responses for one already-present direct membership: role list,
/// membership list reporting the existing role assignment.
pub(super) fn membership_existing(
    id: u64,
    user_id: u64,
    login: &str,
    role_id: u64,
) -> Vec<MockResponse> {
    vec![
        MockResponse::ok(service_roles_payload()),
        MockResponse::ok(membership_collection(Some((
            id,
            user_id,
            login,
            vec![role_id],
        )))),
    ]
}

/// Mirror registration responses shared by every full bootstrap sequence:
/// the idempotent lookup miss followed by the plugin POST.
pub(super) fn mirror_registration() -> Vec<MockResponse> {
    vec![
        MockResponse::error(404, r#"{"errors":["mirror not found"]}"#),
        MockResponse::status(
            202,
            git_mirror_response(
                901,
                44,
                "mirror_44_owner_repo",
                "pending",
                Some("https://git.example.com/owner/repo.git"),
                Some("/var/redmine/repos/owner_repo.git"),
                None,
            ),
        ),
    ]
}

pub(super) fn temp_db(label: &str) -> (std::path::PathBuf, EnvGuard, Storage) {
    let directory = crate::test_scratch::root().join(format!(
        "phasegent-redmine-{label}-{}-{}",
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

/// Seed only the administrator credential and endpoint: it alone must be
/// enough for a complete bootstrap.
pub(super) fn seed_admin_only(storage: &Storage, base: &str) {
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
