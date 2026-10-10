//! Project-discovery bootstrap paths.
//!
//! [`paths`] covers the two full issue-create flows that fall back to
//! bootstrap (discovery miss and explicit-repository mismatch);
//! [`discovery`] the discovery-error surface.

mod discovery;
mod paths;

use crate::auth;
use crate::infra::storage::Storage;
use crate::infra::storage::test_support::EnvGuard;
use crate::policy::Role;
use std::time;

pub(super) fn real_origin() -> crate::remote::RemoteRepository {
    crate::remote::resolve_origin().expect("origin must exist")
}

pub(super) fn temp_storage() -> (Storage, EnvGuard, std::path::PathBuf) {
    let dir = crate::test_scratch::root().join(format!(
        "phasegent-projres-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db_path = dir.join(crate::infra::storage::DB_FILENAME);
    let guard = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
    let storage = Storage::open_at(&db_path).unwrap();
    (storage, guard, dir)
}

pub(super) fn save_orchestrator(storage: &Storage, api_base: Option<String>) {
    storage
        .save_credential(Role::Orchestrator, "redmine", "test-redmine-key")
        .unwrap();
    storage
        .save_redmine_config(
            Role::Orchestrator,
            &auth::RedmineStoredConfig {
                api_base,
                project_id: None,
                close_status_id: Some(5),
                group_name: None,
                group_role: None,
            },
        )
        .unwrap();
}

pub(super) fn user_list_empty() -> String {
    serde_json::json!({"users": [], "total_count": 0, "limit": 100}).to_string()
}

pub(super) fn user_create_response(id: u64, login: &str) -> String {
    serde_json::json!({"user": {"id": id, "login": login, "firstname": "Phasegent", "lastname": login, "mail": format!("{login}@phasegent.local")}}).to_string()
}

pub(super) fn user_get_with_key(id: u64, login: &str, api_key: &str) -> String {
    serde_json::json!({"user": {"id": id, "login": login, "firstname": "Phasegent", "lastname": login, "mail": format!("{login}@phasegent.local"), "api_key": api_key}}).to_string()
}

pub(super) fn closed_status() -> String {
    serde_json::json!({"issue_statuses": [{"id": 5, "name": "Closed", "is_closed": true}]})
        .to_string()
}

pub(super) fn service_roles() -> String {
    crate::providers::redmine::contract_tests::support::role_collection(&[
        (3, "Maintainer"),
        (4, "Developer"),
        (5, "Reporter"),
    ])
}

pub(super) fn membership_added()
-> Vec<crate::providers::redmine::contract_tests::support::MockResponse> {
    use crate::providers::redmine::contract_tests::support::{MockResponse, membership_collection};
    vec![
        MockResponse::ok(service_roles()),
        MockResponse::ok(membership_collection(None)),
        MockResponse::ok("{}"),
    ]
}
