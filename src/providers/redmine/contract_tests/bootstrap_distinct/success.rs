use super::{SERVICE_ROLES, seed_admin, seed_persisted, temp_db};
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::policy::Role;
use crate::providers::redmine::contract_tests::support::{
    MockResponse, git_mirror_response, membership_collection, project_response, role_collection,
    sequence,
};
use std::fs;

#[test]
fn bootstrap_succeeds_with_four_distinct_persisted_users() {
    let _environment_lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("service-roles-ok");
    let _mirror_key = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    let _mirror_url = EnvGuard::set(
        "PHASEGENT_REDMINE_REPOSITORY_URL",
        "https://git.example.com/owner/repo.git",
    );
    // Distinct persisted users: no user HTTP, only membership + mirror.
    let mut responses = vec![
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        MockResponse::ok(
            serde_json::json!({"issue_statuses": [{"id": 5, "name": "Closed", "is_closed": true}]})
                .to_string(),
        ),
        MockResponse::ok(project_response(44, "owner/repo", "owner-repo", "Workflow")),
    ];
    for _ in 0..4 {
        responses.extend([
            MockResponse::ok(role_collection(&[
                (3, "Maintainer"),
                (4, "Developer"),
                (5, "Reporter"),
            ])),
            MockResponse::ok(membership_collection(None)),
            MockResponse::ok("{}"),
        ]);
    }
    responses.extend([
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
    ]);
    let (base, requests, server) = sequence(responses);
    seed_admin(&storage, &base);
    for (role, id, login) in SERVICE_ROLES {
        seed_persisted(&storage, role, id, login, &format!("{role}-key"));
    }
    let result =
        crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None).unwrap();
    assert_eq!(
        result.user_memberships.len(),
        4,
        "four distinct persisted users must yield four memberships"
    );
    assert!(
        result
            .user_memberships
            .iter()
            .any(|m| m.user_id == 33 && m.role_name == "Reporter")
    );
    assert!(
        result
            .user_memberships
            .iter()
            .any(|m| m.user_id == 44 && m.role_name == "Reporter")
    );
    let reqs = requests.recv().unwrap();
    assert_eq!(
        reqs.len(),
        17,
        "3 project + 12 membership + 2 mirror, no user HTTP: {reqs:?}"
    );
    assert!(
        !reqs
            .iter()
            .any(|req| req.contains("/users.json") || req.contains("/users/")),
        "persisted identities must skip the user APIs: {reqs:?}"
    );
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}
