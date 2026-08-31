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
fn no_match_keeps_bootstrap_for_issue_and_actionable_for_version() {
    let _lock = lock_workflow_tests();
    let origin = real_origin();
    let bootstrap_id = crate::remote::redmine_identifier(&origin.repository).unwrap();

    // Issue create with NoMatch -> bootstrap
    let (storage, _guard, dir) = temp_storage();
    storage
        .save_credential(Role::Orchestrator, "redmine", TEST_API_KEY)
        .unwrap();
    storage
        .save_credential(Role::Executor, "redmine", "executor-redmine-key")
        .unwrap();
    storage
        .save_credential(Role::Reviewer, "redmine", "reviewer-redmine-key")
        .unwrap();
    storage
        .save_credential(Role::Admin, "redmine", "admin-redmine-key")
        .unwrap();
    let _mirror = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    let repo_url = format!("https://git.example.com/{}.git", origin.repository);
    let _mirror_url = EnvGuard::set("PHASEGENT_REDMINE_REPOSITORY_URL", &repo_url);
    let (owner, repo) = origin.repository.split_once('/').unwrap();
    let mir_id = format!(
        "mirror_{}_{}_{}",
        44,
        owner.to_ascii_lowercase(),
        repo.to_ascii_lowercase()
    );
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(project_collection(0, 100, &[])),
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        MockResponse::ok(
            serde_json::json!({"issue_statuses": [{"id": 5, "name": "Closed", "is_closed": true}]})
                .to_string(),
        ),
        MockResponse::ok(project_response(
            44,
            &origin.repository,
            &bootstrap_id,
            "Workflow",
        )),
        MockResponse::ok(current_user_response(11, "orchestrator")),
        MockResponse::ok(current_user_response(22, "executor")),
        MockResponse::ok(current_user_response(33, "reviewer")),
        MockResponse::ok(role_collection(&[
            (3, "Maintainer"),
            (4, "Developer"),
            (5, "Reporter"),
        ])),
        MockResponse::ok(membership_collection(None)),
        MockResponse::ok("{}"),
        MockResponse::ok(role_collection(&[
            (3, "Maintainer"),
            (4, "Developer"),
            (5, "Reporter"),
        ])),
        MockResponse::ok(membership_collection(None)),
        MockResponse::ok("{}"),
        MockResponse::ok(role_collection(&[
            (3, "Maintainer"),
            (4, "Developer"),
            (5, "Reporter"),
        ])),
        MockResponse::ok(membership_collection(None)),
        MockResponse::ok("{}"),
        MockResponse::error(404, r#"{"errors":["mirror not found"]}"#),
        MockResponse::status(
            202,
            git_mirror_response(
                901,
                44,
                &mir_id,
                "pending",
                Some(&repo_url),
                Some("/path"),
                None,
            ),
        ),
        MockResponse::ok(issue_response(92, "Bootstrapped", "Body", false, &[])),
    ]);
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
        "Bootstrapped",
        "--body",
        "Body",
    ]));
    assert_eq!(code, 0);
    let reqs = requests.recv().unwrap();
    assert!(reqs[0].starts_with("GET /projects.json?"));
    assert!(reqs[1].starts_with("GET /projects/"));
    server.join().unwrap();
    let _ = fs::remove_dir_all(dir);

    // Version list with NoMatch -> actionable
    let (storage2, _guard2, dir2) = temp_storage();
    let _mirror2 = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    let (base2, requests2, server2) =
        sequence(vec![MockResponse::ok(project_collection(0, 100, &[]))]);
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
    assert_eq!(code2, 1);
    let reqs2 = requests2.recv().unwrap();
    assert_eq!(reqs2.len(), 1);
    assert!(reqs2[0].starts_with("GET /projects.json?"));
    server2.join().unwrap();
    let _ = fs::remove_dir_all(dir2);
}

#[test]
fn explicit_repository_mismatch_does_not_use_wrong_origin() {
    let _lock = lock_workflow_tests();
    let origin = real_origin();
    let explicit = if origin.repository == "owner/repo" {
        "other/tools"
    } else {
        "owner/repo"
    };
    let (storage, _guard, dir) = temp_storage();
    storage
        .save_credential(Role::Orchestrator, "redmine", TEST_API_KEY)
        .unwrap();
    storage
        .save_credential(Role::Executor, "redmine", "executor-redmine-key")
        .unwrap();
    storage
        .save_credential(Role::Reviewer, "redmine", "reviewer-redmine-key")
        .unwrap();
    storage
        .save_credential(Role::Admin, "redmine", "admin-redmine-key")
        .unwrap();
    let _mirror = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    let repo_url2 = format!("https://git.example.com/{explicit}.git");
    let _mirror_url = EnvGuard::set("PHASEGENT_REDMINE_REPOSITORY_URL", &repo_url2);
    let bootstrap_id = crate::remote::redmine_identifier(explicit).unwrap();
    let (base, requests, server) = sequence(vec![
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        MockResponse::ok(
            serde_json::json!({"issue_statuses": [{"id": 5, "name": "Closed", "is_closed": true}]})
                .to_string(),
        ),
        MockResponse::ok(project_response(45, explicit, &bootstrap_id, "Workflow")),
        MockResponse::ok(current_user_response(11, "orchestrator")),
        MockResponse::ok(current_user_response(22, "executor")),
        MockResponse::ok(current_user_response(33, "reviewer")),
        MockResponse::ok(role_collection(&[
            (3, "Maintainer"),
            (4, "Developer"),
            (5, "Reporter"),
        ])),
        MockResponse::ok(membership_collection(None)),
        MockResponse::ok("{}"),
        MockResponse::ok(role_collection(&[
            (3, "Maintainer"),
            (4, "Developer"),
            (5, "Reporter"),
        ])),
        MockResponse::ok(membership_collection(None)),
        MockResponse::ok("{}"),
        MockResponse::ok(role_collection(&[
            (3, "Maintainer"),
            (4, "Developer"),
            (5, "Reporter"),
        ])),
        MockResponse::ok(membership_collection(None)),
        MockResponse::ok("{}"),
        MockResponse::error(404, r#"{"errors":["mirror not found"]}"#),
        MockResponse::status(
            202,
            git_mirror_response(
                902,
                45,
                &format!(
                    "mirror_45_{}_{}",
                    explicit.split('/').next().unwrap().to_ascii_lowercase(),
                    explicit.split('/').nth(1).unwrap().to_ascii_lowercase()
                ),
                "pending",
                Some(&repo_url2),
                Some("/path"),
                None,
            ),
        ),
        MockResponse::ok(issue_response(93, "Mismatched", "Body", false, &[])),
    ]);
    let code = crate::cli::run(strings([
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "--api-base",
        &base,
        "--repository",
        explicit,
        "issue",
        "create",
        "--title",
        "Mismatched",
        "--body",
        "Body",
    ]));
    assert_eq!(code, 0);
    let reqs = requests.recv().unwrap();
    assert!(reqs[0].starts_with(&format!("GET /projects/{bootstrap_id}.json")));
    assert!(!reqs.iter().any(|r| r.contains("/projects.json?limit=100")));
    server.join().unwrap();
    let _ = fs::remove_dir_all(dir);
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
    let c = crate::cli::run(strings([
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "--api-base",
        &b,
        "version",
        "list",
    ]));
    assert_eq!(c, 1);
    let r = req.recv().unwrap();
    assert_eq!(r.len(), 2);
    srv.join().unwrap();
    let _ = fs::remove_dir_all(d);
}
