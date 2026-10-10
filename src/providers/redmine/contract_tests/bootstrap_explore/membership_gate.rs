use super::{
    SERVICE_LOGINS, SERVICE_ROLES, existing_role, membership_added, mirror_env,
    mirror_registration, project_created, roles_without_reporter, seed_admin, seed_persisted,
    temp_db,
};
use crate::infra::storage::test_support::lock_workflow_tests;
use crate::policy::Role;
use crate::providers::redmine::contract_tests::support::{
    MockResponse, assert_request_with_key, membership_collection, sequence,
};
use std::fs;

#[test]
fn explore_membership_warning_blocks_bootstrap_setting_persistence() {
    let _environment_lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("warning-gate");
    let (_key, _url) = mirror_env();
    let mut responses = project_created();
    for _ in 0..3 {
        responses.extend(membership_added());
    }
    // The explore membership resolves a role list without `Reporter`, so the
    // existing missing-role gate warns instead of writing a membership.
    responses.push(MockResponse::ok(roles_without_reporter()));
    responses.extend(mirror_registration());
    let (base, requests, server) = sequence(responses);
    seed_admin(&storage, &base);
    for (role, id, login) in SERVICE_ROLES {
        seed_persisted(&storage, role, id, login, &format!("{role}-key"));
    }

    let result =
        crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None).unwrap();
    assert_eq!(result.user_memberships.len(), 4);
    assert!(!result.ready(), "a warning membership must block readiness");
    let explore = &result.user_memberships[3];
    assert_eq!(explore.user_login, "phasegent-explore");
    assert_eq!(explore.status, "warning");
    assert!(
        explore
            .warning
            .as_deref()
            .is_some_and(|warning| warning.contains("role was not found")),
        "explore warning: {:?}",
        explore.warning
    );

    let reqs = requests.recv().unwrap();
    // 3 project + 3 memberships (9) + 1 explore role list + 2 mirror = 15.
    assert_eq!(reqs.len(), 15, "warning run: {reqs:?}");
    assert!(
        !reqs
            .iter()
            .any(|req| req.contains("POST /projects/44/memberships.json")
                && req.contains(r#""user_id":44"#)),
        "a warned membership must never be written: {reqs:?}"
    );

    // The blocked gate also blocks every role's settings persistence.
    for (role, _, login) in SERVICE_ROLES {
        let stored = crate::auth::load_redmine_config(role, &storage)
            .unwrap()
            .unwrap_or_default();
        assert_eq!(
            stored.close_status_id, None,
            "{login} settings must stay unwritten while a membership warns"
        );
    }
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn explore_membership_write_uses_its_own_identity_and_role() {
    let _environment_lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("membership-write");
    let (_key, _url) = mirror_env();
    let list = super::user_list_with(&SERVICE_LOGINS);
    let mut responses = project_created();
    for (role, id, login) in SERVICE_ROLES {
        responses.extend(existing_role(&list, id, login, &format!("{role}-key")));
    }
    for _ in 0..4 {
        responses.extend(membership_added());
    }
    responses.extend(mirror_registration());
    let (base, requests, server) = sequence(responses);
    seed_admin(&storage, &base);

    let result =
        crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None).unwrap();
    assert_eq!(result.user_memberships.len(), 4);
    let reqs = requests.recv().unwrap();
    // 3 project + 4 lookups + 4 key reads + 12 membership + 2 mirror = 25.
    assert_eq!(reqs.len(), 25, "explore membership run: {reqs:?}");
    let writes: Vec<&String> = reqs
        .iter()
        .filter(|req| req.starts_with("POST /projects/44/memberships.json"))
        .collect();
    assert_eq!(writes.len(), 4, "one membership write per role: {reqs:?}");
    assert!(
        writes[3].contains(r#""user_id":44,"role_ids":[5]"#),
        "{}",
        writes[3]
    );
    // Role list and admin key reads stay on the administrator credential.
    assert_request_with_key(&reqs[3], "GET", "/users.json?", None, "admin-redmine-key");
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn explore_membership_is_not_duplicated_when_already_present() {
    let _environment_lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("existing-membership");
    let (_key, _url) = mirror_env();
    let mut responses = project_created();
    for _ in 0..3 {
        responses.extend(membership_added());
    }
    responses.extend([
        MockResponse::ok(super::service_roles_payload()),
        MockResponse::ok(membership_collection(Some((
            71,
            44,
            "phasegent-explore",
            vec![5],
        )))),
    ]);
    responses.extend(mirror_registration());
    let (base, requests, server) = sequence(responses);
    seed_admin(&storage, &base);
    for (role, id, login) in SERVICE_ROLES {
        seed_persisted(&storage, role, id, login, &format!("{role}-key"));
    }

    let result =
        crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None).unwrap();
    let explore = result
        .user_memberships
        .iter()
        .find(|m| m.user_login == "phasegent-explore")
        .expect("explore membership");
    assert_eq!(explore.status, "existing");
    assert!(result.ready());
    let reqs = requests.recv().unwrap();
    // 3 project + 3 memberships (9) + 2 explore reads + 2 mirror = 16.
    assert_eq!(reqs.len(), 16, "existing explore membership: {reqs:?}");
    assert!(
        !reqs
            .iter()
            .any(|req| req.contains("POST /projects/44/memberships.json")
                && req.contains(r#""user_id":44"#)),
        "an existing explore membership must not be re-added: {reqs:?}"
    );
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}
