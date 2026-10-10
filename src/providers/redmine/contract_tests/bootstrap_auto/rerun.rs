use super::{
    SERVICE_ROLES, bootstrap_project, closed_status, create_role, membership_added,
    membership_existing, mirror_registration, seed_admin_only, temp_db,
};
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::policy::Role;
use crate::providers::redmine::contract_tests::support::{MockResponse, sequence};
use std::fs;

/// Membership role ids the four service roles are expected to receive.
const EXPECTED_ROLE_IDS: [u64; 4] = [3, 4, 5, 5];

#[test]
fn bootstrap_rerun_reuses_persisted_users_without_user_api_calls() {
    let _environment_lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("rerun-reuse");
    let _mirror_key = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    let _mirror_url = EnvGuard::set(
        "PHASEGENT_REDMINE_REPOSITORY_URL",
        "https://git.example.com/owner/repo.git",
    );
    let (base, requests, server) = sequence(rerun_sequence());
    seed_admin_only(&storage, &base);
    let first =
        crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None).unwrap();
    assert_eq!(first.user_memberships.len(), 4);
    let second =
        crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None).unwrap();
    assert_eq!(second.user_memberships.len(), 4);
    assert!(
        second
            .user_memberships
            .iter()
            .all(|m| m.status == "existing"),
        "second run must observe existing memberships: {:?}",
        second.user_memberships
    );
    let reqs = requests.recv().unwrap();
    // First run 29 (3 project + 12 provisioning + 12 membership + 2 mirror)
    // plus second run 12 (2 project + 8 membership lookups + 2 mirror): the
    // second run only reads, so it writes no membership.
    assert_eq!(reqs.len(), 41, "rerun must be 41: {reqs:?}");
    let second_run = &reqs[29..];
    assert!(
        !second_run
            .iter()
            .any(|r| r.contains("/users.json") || r.contains("/users/")),
        "rerun must not touch user APIs: {second_run:?}"
    );
    // Persisted keys reused verbatim.
    for (role, _, login) in SERVICE_ROLES {
        assert_eq!(
            storage.load_credential(role, "redmine").unwrap().as_deref(),
            Some(format!("{}-rerun-key", role.as_str()).as_str()),
            "rerun must keep the {login} key"
        );
    }
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

/// First run creates all four identities; second run finds the project and
/// every membership already in place.
fn rerun_sequence() -> Vec<MockResponse> {
    let mut responses = vec![
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        MockResponse::ok(closed_status()),
        MockResponse::ok(bootstrap_project()),
    ];
    for (role, id, login) in SERVICE_ROLES {
        responses.extend(create_role(
            id,
            login,
            &format!("{}-rerun-key", role.as_str()),
        ));
    }
    for _ in 0..4 {
        responses.extend(membership_added());
    }
    responses.extend(mirror_registration());

    responses.push(MockResponse::ok(bootstrap_project()));
    responses.push(MockResponse::ok(closed_status()));
    for (index, (_, id, login)) in SERVICE_ROLES.iter().enumerate() {
        responses.extend(membership_existing(
            55 + index as u64,
            *id,
            login,
            EXPECTED_ROLE_IDS[index],
        ));
    }
    responses.extend(mirror_registration());
    responses
}
