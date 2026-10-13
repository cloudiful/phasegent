use super::{real_origin, save_orchestrator, temp_storage};
use crate::auth;
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::policy::Role;
use crate::providers::redmine::contract_tests::support::{
    MockResponse, assert_request_with_key, git_mirror_response, issue_response, project_collection,
    sequence, strings,
};
use std::fs;

fn save_admin(storage: &crate::infra::storage::Storage, api_base: Option<String>) {
    storage
        .save_credential(Role::Admin, "redmine", "admin-redmine-key")
        .unwrap();
    storage
        .save_redmine_config(
            Role::Admin,
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

#[test]
fn discovery_error_for_version_list_is_not_swallowed() {
    let _lock = lock_workflow_tests();
    let (s, _g, d) = temp_storage();
    let _m = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    let (b, req, srv) = sequence(vec![
        MockResponse::ok(project_collection(1, 100, &[(44, "Workflow", "workflow")])),
        MockResponse::error(401, r#"{"errors":["unauthorized"]}"#),
    ]);
    save_orchestrator(&s, Some(b.clone()));
    let c = crate::cli::run_with_role(
        strings(["--provider", "redmine", "--api-base", &b, "version", "list"]),
        Some(Role::Orchestrator.as_str()),
    );
    assert_eq!(c, 1);
    let r = req.recv().unwrap();
    assert_eq!(r.len(), 2);
    srv.join().unwrap();
    let _ = fs::remove_dir_all(d);
}

/// P1 Operator single-match discovery bypasses bootstrap with the admin
/// credential and no fallback across keys.
#[test]
fn operator_single_match_bypasses_bootstrap_with_admin_credential() {
    let _lock = lock_workflow_tests();
    crate::workflow::clear_completed_bootstraps_for_tests();
    let origin = real_origin();
    let (owner, repo) = origin.repository.split_once('/').unwrap();
    let identifier = format!(
        "mirror_{}_{}_{}",
        44,
        owner.to_ascii_lowercase(),
        repo.to_ascii_lowercase()
    );

    let (storage, _guard, dir) = temp_storage();
    save_admin(&storage, None);
    let _mirror = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(project_collection(
            1,
            100,
            &[(44, "tools/phasegent", "tools-phasegent")],
        )),
        MockResponse::ok(git_mirror_response(
            901,
            44,
            &identifier,
            "ready",
            Some(&origin.repository_url),
            Some("/path"),
            None,
        )),
        MockResponse::ok(issue_response(91, "Discovered", "Body", false, &[])),
    ]);
    // Point the admin config at the mock base after sequencing.
    storage
        .save_redmine_config(
            Role::Admin,
            &auth::RedmineStoredConfig {
                api_base: Some(base.clone()),
                project_id: None,
                close_status_id: Some(5),
                group_name: None,
                group_role: None,
            },
        )
        .unwrap();
    let code = crate::cli::run_with_role(
        strings([
            "--provider",
            "redmine",
            "--api-base",
            &base,
            "issue",
            "create",
            "--title",
            "Discovered",
            "--body",
            "Body",
        ]),
        None,
    );
    assert_eq!(code, 0, "operator single-match create must succeed");
    let reqs = requests.recv().unwrap();
    assert_eq!(reqs.len(), 3, "single-match run: {reqs:?}");
    assert!(reqs[2].contains(r#""project_id":44"#));
    assert_request_with_key(
        &reqs[0],
        "GET",
        "/projects.json?",
        None,
        "admin-redmine-key",
    );
    server.join().unwrap();
    let _ = fs::remove_dir_all(dir);
}

/// P1 Operator multiple matches fail before any write with no fallback.
#[test]
fn operator_multiple_matches_fail_before_writes() {
    let _lock = lock_workflow_tests();
    crate::workflow::clear_completed_bootstraps_for_tests();
    let origin = real_origin();
    let (owner, repo) = origin.repository.split_once('/').unwrap();
    let id1 = format!(
        "mirror_{}_{}_{}",
        44,
        owner.to_ascii_lowercase(),
        repo.to_ascii_lowercase()
    );
    let id2 = format!(
        "mirror_{}_{}_{}",
        45,
        owner.to_ascii_lowercase(),
        repo.to_ascii_lowercase()
    );

    let (storage, _guard, dir) = temp_storage();
    save_admin(&storage, None);
    let _mirror = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(project_collection(
            2,
            100,
            &[(44, "A", "a"), (45, "B", "b")],
        )),
        MockResponse::ok(git_mirror_response(
            901,
            44,
            &id1,
            "ready",
            Some(&origin.repository_url),
            Some("/a"),
            None,
        )),
        MockResponse::ok(git_mirror_response(
            902,
            45,
            &id2,
            "ready",
            Some(&origin.repository_url),
            Some("/b"),
            None,
        )),
    ]);
    storage
        .save_redmine_config(
            Role::Admin,
            &auth::RedmineStoredConfig {
                api_base: Some(base.clone()),
                project_id: None,
                close_status_id: None,
                group_name: None,
                group_role: None,
            },
        )
        .unwrap();
    let code = crate::cli::run_with_role(
        strings([
            "--provider",
            "redmine",
            "--api-base",
            &base,
            "issue",
            "create",
            "--title",
            "X",
            "--body",
            "Y",
        ]),
        None,
    );
    assert_eq!(code, 1, "operator multiple-match must fail");
    let reqs = requests.recv().unwrap();
    assert_eq!(reqs.len(), 3, "multiple-match run: {reqs:?}");
    assert!(!reqs.iter().any(|r| r.starts_with("POST")));
    server.join().unwrap();
    let _ = fs::remove_dir_all(dir);
}

/// P1 Operator explicit `--project-id` skips discovery entirely.
#[test]
fn operator_explicit_project_skips_discovery() {
    let _lock = lock_workflow_tests();
    crate::workflow::clear_completed_bootstraps_for_tests();
    let (storage, _guard, dir) = temp_storage();
    save_admin(&storage, None);
    let _mirror = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    let (base, requests, server) = sequence(vec![MockResponse::ok(issue_response(
        90,
        "Explicit",
        "Body",
        false,
        &[],
    ))]);
    storage
        .save_redmine_config(
            Role::Admin,
            &auth::RedmineStoredConfig {
                api_base: Some(base.clone()),
                project_id: None,
                close_status_id: Some(5),
                group_name: None,
                group_role: None,
            },
        )
        .unwrap();
    let code = crate::cli::run_with_role(
        strings([
            "--provider",
            "redmine",
            "--api-base",
            &base,
            "--project-id",
            "99",
            "issue",
            "create",
            "--title",
            "Explicit",
            "--body",
            "Body",
        ]),
        None,
    );
    assert_eq!(code, 0);
    let reqs = requests.recv().unwrap();
    assert_eq!(reqs.len(), 1, "explicit run must skip discovery: {reqs:?}");
    assert!(reqs[0].contains(r#""project_id":99"#));
    server.join().unwrap();
    let _ = fs::remove_dir_all(dir);
}
