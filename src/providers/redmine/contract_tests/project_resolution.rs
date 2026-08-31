#![allow(unused_imports)]
use super::support;
use super::support::*;
use crate::auth;
use crate::infra::storage::Storage;
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::policy::Role;
use std::{fs, time};

fn real_origin() -> crate::remote::RemoteRepository {
    crate::remote::resolve_origin().expect("origin must exist")
}

fn temp_storage() -> (Storage, EnvGuard, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
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

fn save_orchestrator(storage: &Storage, api_base: Option<String>) {
    storage
        .save_credential(Role::Orchestrator, "redmine", TEST_API_KEY)
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

#[test]
fn explicit_project_id_bypasses_discovery_for_issue_create() {
    let _lock = lock_workflow_tests();
    let (storage, _guard, dir) = temp_storage();
    storage
        .save_credential(Role::Admin, "redmine", "admin-redmine-key")
        .unwrap();
    let _mirror = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    let (base, requests, server) = sequence(vec![MockResponse::ok(issue_response(
        90,
        "Explicit",
        "Body",
        false,
        &[],
    ))]);
    save_orchestrator(&storage, Some(base.clone()));
    let code = crate::cli::run(strings([
        "--role",
        "orchestrator",
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
    ]));
    assert_eq!(code, 0);
    let reqs = requests.recv().unwrap();
    assert_eq!(reqs.len(), 1);
    assert!(reqs[0].contains(r#""project_id":99"#));
    server.join().unwrap();
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn unique_match_bypasses_bootstrap_for_issue_create_and_version_list() {
    let _lock = lock_workflow_tests();
    let origin = real_origin();
    let (owner, repo) = origin.repository.split_once('/').unwrap();
    let identifier = format!(
        "mirror_{}_{}_{}",
        44,
        owner.to_ascii_lowercase(),
        repo.to_ascii_lowercase()
    );

    // Issue create
    let (storage, _guard, dir) = temp_storage();
    storage
        .save_credential(Role::Admin, "redmine", "admin-redmine-key")
        .unwrap();
    let _mirror = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    save_orchestrator(&storage, None);
    storage
        .save_redmine_config(
            Role::Admin,
            &auth::RedmineStoredConfig {
                api_base: None,
                project_id: None,
                close_status_id: None,
                group_name: None,
                group_role: None,
            },
        )
        .unwrap();
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
    // Update storage to use base
    storage
        .save_redmine_config(
            Role::Orchestrator,
            &auth::RedmineStoredConfig {
                api_base: Some(base.clone()),
                project_id: None,
                close_status_id: Some(5),
                group_name: None,
                group_role: None,
            },
        )
        .unwrap();
    let code = crate::cli::run(strings([
        "--role",
        "orchestrator",
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
    ]));
    assert_eq!(code, 0);
    let reqs = requests.recv().unwrap();
    assert_eq!(reqs.len(), 3);
    assert!(reqs[2].contains(r#""project_id":44"#));
    assert_eq!(
        crate::auth::load_redmine_config(Role::Orchestrator, &storage)
            .unwrap()
            .unwrap()
            .project_id,
        None
    );
    server.join().unwrap();
    let _ = fs::remove_dir_all(dir);

    // Version list
    let (storage2, _guard2, dir2) = temp_storage();
    let _mirror2 = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    let (base2, requests2, server2) = sequence(vec![
        MockResponse::ok(project_collection(
            1,
            100,
            &[(44, "tools/phasegent", "tools-phasegent")],
        )),
        MockResponse::ok(git_mirror_response(
            902,
            44,
            &identifier,
            "ready",
            Some(&origin.repository_url),
            Some("/path"),
            None,
        )),
        MockResponse::ok(version_collection(&[(12, "Sprint 1", "open", None)])),
    ]);
    save_orchestrator(&storage2, Some(base2.clone()));
    let code2 = crate::cli::run(strings([
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "--api-base",
        &base2,
        "version",
        "list",
    ]));
    assert_eq!(code2, 0);
    let reqs2 = requests2.recv().unwrap();
    assert_eq!(reqs2.len(), 3);
    assert!(reqs2[2].contains("/projects/44/versions.json"));
    server2.join().unwrap();
    let _ = fs::remove_dir_all(dir2);
}

#[test]
fn multiple_matches_fail_before_writes_for_issue_and_version() {
    let _lock = lock_workflow_tests();
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
    let _mirror = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    save_orchestrator(&storage, None);
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
    let storage2 = Storage::open_at(&dir.join(crate::infra::storage::DB_FILENAME)).unwrap();
    storage2
        .save_redmine_config(
            Role::Orchestrator,
            &auth::RedmineStoredConfig {
                api_base: Some(base.clone()),
                project_id: None,
                close_status_id: None,
                group_name: None,
                group_role: None,
            },
        )
        .unwrap();
    let code = crate::cli::run(strings([
        "--role",
        "orchestrator",
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
    ]));
    assert_eq!(code, 1);
    let reqs = requests.recv().unwrap();
    assert_eq!(reqs.len(), 3);
    assert!(!reqs.iter().any(|r| r.starts_with("POST")));
    server.join().unwrap();
    let _ = fs::remove_dir_all(dir);

    let (storage3, _guard3, dir3) = temp_storage();
    let _mirror3 = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    save_orchestrator(&storage3, None);
    let (base3, requests3, server3) = sequence(vec![
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
    let storage3b = Storage::open_at(&dir3.join(crate::infra::storage::DB_FILENAME)).unwrap();
    storage3b
        .save_redmine_config(
            Role::Orchestrator,
            &auth::RedmineStoredConfig {
                api_base: Some(base3.clone()),
                project_id: None,
                close_status_id: None,
                group_name: None,
                group_role: None,
            },
        )
        .unwrap();
    let code3 = crate::cli::run(strings([
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "--api-base",
        &base3,
        "version",
        "list",
    ]));
    assert_eq!(code3, 1);
    let reqs3 = requests3.recv().unwrap();
    assert_eq!(reqs3.len(), 3);
    assert!(!reqs3.iter().any(|r| r.contains("/versions.json")));
    server3.join().unwrap();
    let _ = fs::remove_dir_all(dir3);
}

#[test]
fn discovery_errors_are_not_swallowed() {
    let _lock = lock_workflow_tests();
    let (s, _g, d) = temp_storage();
    let _m = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    let (b, req, srv) = sequence(vec![
        MockResponse::ok(project_collection(1, 100, &[(44, "Workflow", "workflow")])),
        MockResponse::error(500, r#"{"errors":["server error"]}"#),
    ]);
    save_orchestrator(&s, Some(b.clone()));
    let c = crate::cli::run(strings([
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "--api-base",
        &b,
        "issue",
        "create",
        "--title",
        "X",
        "--body",
        "Y",
    ]));
    assert_eq!(c, 1);
    let r = req.recv().unwrap();
    assert_eq!(r.len(), 2);
    assert!(!r.iter().any(|x| x.starts_with("POST")));
    srv.join().unwrap();
    let _ = fs::remove_dir_all(d);
}
