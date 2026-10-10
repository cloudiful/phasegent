use super::{
    EXPECTED_ROLE_IDS, SERVICE_ROLES, create_role, membership_added, membership_existing,
    mirror_env, mirror_registration, project_created, seed_admin, seed_persisted,
    seed_three_account_install, temp_db,
};
use crate::infra::storage::test_support::lock_workflow_tests;
use crate::policy::Role;
use crate::providers::redmine::contract_tests::support::{MockResponse, sequence};
use std::fs;

#[test]
fn existing_three_account_install_gains_only_explore() {
    let _environment_lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("supplement");
    let (_key, _url) = mirror_env();
    seed_three_account_install(&storage);
    let mut responses = project_created();
    responses.extend(create_role(
        44,
        "phasegent-explore",
        "explore-provisioned-key",
    ));
    for _ in 0..4 {
        responses.extend(membership_added());
    }
    responses.extend(mirror_registration());
    let (base, requests, server) = sequence(responses);
    seed_admin(&storage, &base);

    let result =
        crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None).unwrap();
    assert_eq!(result.user_memberships.len(), 4);
    assert!(
        result.ready(),
        "every membership must be actionable: {:?}",
        result.user_memberships
    );

    let reqs = requests.recv().unwrap();
    // 3 project + 3 explore provisioning + 12 membership + 2 mirror.
    assert_eq!(reqs.len(), 20, "supplement run: {reqs:?}");
    let creates: Vec<&String> = reqs
        .iter()
        .filter(|req| req.starts_with("POST /users.json"))
        .collect();
    assert_eq!(creates.len(), 1, "only explore may be created: {reqs:?}");
    assert!(
        creates[0].contains(r#""login":"phasegent-explore""#),
        "{}",
        creates[0]
    );
    assert!(
        !reqs.iter().any(|req| req.contains("GET /users/11.json"))
            && !reqs.iter().any(|req| req.contains("GET /users/22.json"))
            && !reqs.iter().any(|req| req.contains("GET /users/33.json")),
        "the three cached identities must not be re-read: {reqs:?}"
    );

    // The three pre-existing keys stay untouched; explore gets its own.
    for (role, _, login) in SERVICE_ROLES.iter().take(3) {
        assert_eq!(
            storage
                .load_credential(*role, "redmine")
                .unwrap()
                .as_deref(),
            Some(format!("{}-existing-key", role.as_str()).as_str()),
            "{login} key must be reused"
        );
    }
    assert_eq!(
        storage
            .load_credential(Role::Explore, "redmine")
            .unwrap()
            .as_deref(),
        Some("explore-provisioned-key")
    );
    assert_eq!(
        storage.load_redmine_user(Role::Explore).unwrap(),
        Some((44, "phasegent-explore".to_owned()))
    );
    assert_eq!(result.user_memberships[3].user_login, "phasegent-explore");
    assert_eq!(result.user_memberships[3].role_name, "Reporter");
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn rerun_after_explore_is_provisioned_creates_nothing_and_keeps_own_keys() {
    let _environment_lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("cached-rerun");
    let (_key, _url) = mirror_env();
    let mut responses = project_created();
    for (index, (_, id, login)) in SERVICE_ROLES.iter().enumerate() {
        responses.extend(membership_existing(
            60 + index as u64,
            *id,
            login,
            EXPECTED_ROLE_IDS[index],
        ));
    }
    responses.extend(mirror_registration());
    responses.push(MockResponse::ok(super::bootstrap_project()));
    responses.push(MockResponse::ok(super::closed_status()));
    for (index, (_, id, login)) in SERVICE_ROLES.iter().enumerate() {
        responses.extend(membership_existing(
            60 + index as u64,
            *id,
            login,
            EXPECTED_ROLE_IDS[index],
        ));
    }
    responses.extend(mirror_registration());
    let (base, requests, server) = sequence(responses);
    seed_admin(&storage, &base);
    for (role, id, login) in SERVICE_ROLES {
        seed_persisted(
            &storage,
            role,
            id,
            login,
            &format!("{}-cached-key", role.as_str()),
        );
    }

    let first =
        crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None).unwrap();
    let second =
        crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None).unwrap();
    assert_eq!(first.user_memberships.len(), 4);
    assert_eq!(second.user_memberships.len(), 4);
    assert!(
        second
            .user_memberships
            .iter()
            .all(|m| m.status == "existing"),
        "rerun must observe existing memberships: {:?}",
        second.user_memberships
    );

    let reqs = requests.recv().unwrap();
    // Run 1: 3 project + 8 membership reads + 2 mirror = 13.
    // Run 2: 2 project + 8 membership reads + 2 mirror = 12.
    assert_eq!(reqs.len(), 25, "cached rerun: {reqs:?}");
    assert!(
        !reqs
            .iter()
            .any(|req| req.contains("/users.json") || req.contains("/users/")),
        "a fully provisioned rerun must not touch user APIs: {reqs:?}"
    );
    assert!(
        !reqs
            .iter()
            .any(|req| req.contains("POST /projects/44/memberships.json")),
        "a fully provisioned rerun must not duplicate memberships: {reqs:?}"
    );
    for (role, _, login) in SERVICE_ROLES {
        assert_eq!(
            storage.load_credential(role, "redmine").unwrap().as_deref(),
            Some(format!("{}-cached-key", role.as_str()).as_str()),
            "{login} must keep its own key"
        );
    }
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}
