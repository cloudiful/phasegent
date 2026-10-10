//! Explicit all-projects bootstrap contract tests.
//!
//! Children cover complete pagination, read-only preview, additive apply,
//! shared-identity rejection, and the empty no-op. Every scenario drives the
//! real `workflow::bootstrap_all_projects` entry point against the local mock
//! server and a disposable initialized store.

mod apply;
mod apply_errors;
mod apply_resilience;
mod empty;
mod identity;
mod pagination;
mod preview;
mod preview_outcomes;

use crate::providers::redmine::contract_tests::support::{
    MockResponse, membership_collection, project_collection, role_collection,
};

/// The three Redmine role names the agent defaults resolve to.
pub(super) fn service_roles_payload() -> String {
    role_collection(&[(3, "Maintainer"), (4, "Developer"), (5, "Reporter")])
}

pub(super) fn projects_response(
    total_count: usize,
    limit: usize,
    projects: &[(u64, &str, &str)],
) -> MockResponse {
    MockResponse::ok(project_collection(total_count, limit, projects))
}

pub(super) fn empty_memberships() -> MockResponse {
    MockResponse::ok(membership_collection(None))
}

pub(super) fn membership_with(
    id: u64,
    user_id: u64,
    login: &str,
    role_ids: Vec<u64>,
) -> MockResponse {
    MockResponse::ok(membership_collection(Some((id, user_id, login, role_ids))))
}
