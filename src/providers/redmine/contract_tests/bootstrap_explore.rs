//! Bootstrap contract tests for the `explore` service user.
//!
//! [`supplement`] covers provisioning a missing explore account alongside
//! existing ones and the fully cached rerun, [`collision`] the
//! distinct-identity abort that names explore, [`membership_gate`] the
//! membership readiness gate, and [`credential_flow`] explore's own-key
//! requirement with no credential fallback.

mod collision;
mod credential_flow;
mod membership_gate;
mod supplement;

use crate::auth;
use crate::infra::storage::Storage;
use crate::infra::storage::test_support::EnvGuard;
use crate::policy::Role;
use crate::providers::redmine::contract_tests::support::{
    MockResponse, git_mirror_response, membership_collection, project_response, role_collection,
};
use std::time;

/// Deterministic service identities in provisioning order.
pub(super) const SERVICE_ROLES: [(Role, u64, &str); 4] = [
    (Role::Orchestrator, 11, "phasegent-orchestrator"),
    (Role::Executor, 22, "phasegent-executor"),
    (Role::Reviewer, 33, "phasegent-reviewer"),
    (Role::Explore, 44, "phasegent-explore"),
];

/// Redmine roles the four service roles resolve to, in provisioning order.
pub(super) const EXPECTED_ROLE_IDS: [u64; 4] = [3, 4, 5, 5];

/// The deterministic logins a fully provisioned server already holds.
pub(super) const SERVICE_LOGINS: [(u64, &str); 4] = [
    (11, "phasegent-orchestrator"),
    (22, "phasegent-executor"),
    (33, "phasegent-reviewer"),
    (44, "phasegent-explore"),
];

pub(super) fn user_list_empty() -> String {
    serde_json::json!({"users": [], "total_count": 0, "limit": 100}).to_string()
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
    serde_json::json!({"user": {"id": id, "login": login, "firstname": "Phasegent", "lastname": login, "mail": format!("{login}@phasegent.local")}}).to_string()
}

pub(super) fn user_get_with_key(id: u64, login: &str, api_key: &str) -> String {
    serde_json::json!({"user": {"id": id, "login": login, "firstname": "Phasegent", "lastname": login, "mail": format!("{login}@phasegent.local"), "api_key": api_key}}).to_string()
}

pub(super) fn closed_status() -> String {
    serde_json::json!({"issue_statuses": [{"id": 5, "name": "Closed", "is_closed": true}]})
        .to_string()
}

pub(super) fn service_roles_payload() -> String {
    role_collection(&[(3, "Maintainer"), (4, "Developer"), (5, "Reporter")])
}

/// Roles payload without `Reporter`, used to exercise the existing
/// missing-role warning gate for the explore membership.
pub(super) fn roles_without_reporter() -> String {
    role_collection(&[(3, "Maintainer"), (4, "Developer")])
}

pub(super) fn bootstrap_project() -> String {
    project_response(
        44,
        "owner/repo",
        "owner-repo",
        "Workflow issues for owner/repo",
    )
}

pub(super) fn project_created() -> Vec<MockResponse> {
    vec![
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        MockResponse::ok(closed_status()),
        MockResponse::ok(bootstrap_project()),
    ]
}

/// Provision one role from scratch: empty lookup, create, admin key read.
pub(super) fn create_role(id: u64, login: &str, api_key: &str) -> Vec<MockResponse> {
    vec![
        MockResponse::ok(user_list_empty()),
        MockResponse::status(201, user_create_response(id, login)),
        MockResponse::ok(user_get_with_key(id, login, api_key)),
    ]
}

/// Provision one role that already exists server-side: the login lookup hits
/// and only the admin key read follows.
pub(super) fn existing_role(list: &str, id: u64, login: &str, api_key: &str) -> Vec<MockResponse> {
    vec![
        MockResponse::ok(list.to_owned()),
        MockResponse::ok(user_get_with_key(id, login, api_key)),
    ]
}

/// One membership reconciliation that adds the role.
pub(super) fn membership_added() -> Vec<MockResponse> {
    vec![
        MockResponse::ok(service_roles_payload()),
        MockResponse::ok(membership_collection(None)),
        MockResponse::ok("{}"),
    ]
}

/// One membership reconciliation that finds the role already assigned.
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
        "phasegent-redmine-explore-{label}-{}-{}",
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

pub(super) fn mirror_env() -> (EnvGuard, EnvGuard) {
    (
        EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key"),
        EnvGuard::set(
            "PHASEGENT_REDMINE_REPOSITORY_URL",
            "https://git.example.com/owner/repo.git",
        ),
    )
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

/// Persisted identity and key for every role except `Role::Explore`, which is
/// what an install provisioned before explore existed looks like.
pub(super) fn seed_three_account_install(storage: &Storage) {
    for (role, id, login) in SERVICE_ROLES.iter().take(3) {
        seed_persisted(
            storage,
            *role,
            *id,
            login,
            &format!("{}-existing-key", role.as_str()),
        );
    }
}
