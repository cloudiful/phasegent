use super::{
    closed_status, membership_added, real_origin, temp_storage, user_create_response,
    user_get_with_key, user_list_empty,
};
use crate::infra::storage::Storage;
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::policy::Role;
use crate::providers::redmine::contract_tests::support::{
    MockResponse, git_mirror_response, issue_response, project_collection, project_response,
    sequence, strings,
};
use std::fs;

/// Deterministic service identities in provisioning order.
const SERVICE_ROLES: [(u64, &str); 4] = [
    (11, "phasegent-orchestrator"),
    (22, "phasegent-executor"),
    (33, "phasegent-reviewer"),
    (44, "phasegent-explore"),
];

fn seed_every_role(storage: &Storage) {
    storage
        .save_credential(Role::Orchestrator, "redmine", "test-redmine-key")
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
}

fn create_role(id: u64, login: &str, api_key: &str) -> Vec<MockResponse> {
    vec![
        MockResponse::ok(user_list_empty()),
        MockResponse::status(201, user_create_response(id, login)),
        MockResponse::ok(user_get_with_key(id, login, api_key)),
    ]
}

#[test]
fn no_match_keeps_bootstrap_for_issue_and_actionable_for_version() {
    let _lock = lock_workflow_tests();
    crate::workflow::clear_completed_bootstraps_for_tests();
    let origin = real_origin();
    let bootstrap_id = crate::remote::redmine_identifier(&origin.repository).unwrap();

    // Issue create with NoMatch -> bootstrap
    let (storage, _guard, dir) = temp_storage();
    seed_every_role(&storage);
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
    let mut responses = vec![
        MockResponse::ok(project_collection(0, 100, &[])),
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        MockResponse::ok(closed_status()),
        MockResponse::ok(project_response(
            44,
            &origin.repository,
            &bootstrap_id,
            "Workflow",
        )),
    ];
    for (id, login) in SERVICE_ROLES {
        responses.extend(create_role(id, login, &format!("{login}-key")));
    }
    for _ in 0..4 {
        responses.extend(membership_added());
    }
    responses.extend([
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
    let (base, requests, server) = sequence(responses);
    let code = crate::cli::run_with_role(
        strings([
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
        ]),
        Some("orchestrator"),
    );
    assert_eq!(code, 0);
    let reqs = requests.recv().unwrap();
    // 1 discovery + 3 project + 12 provisioning + 12 membership + 2 mirror + 1 issue.
    assert_eq!(reqs.len(), 31, "bootstrap-for-issue run: {reqs:?}");
    assert!(reqs[0].starts_with("GET /projects.json?"));
    assert!(reqs[1].starts_with("GET /projects/"));
    server.join().unwrap();
    let _ = fs::remove_dir_all(dir);

    // Version list with NoMatch -> actionable
    let (storage2, _guard2, dir2) = temp_storage();
    let _mirror2 = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    let (base2, requests2, server2) =
        sequence(vec![MockResponse::ok(project_collection(0, 100, &[]))]);
    super::save_orchestrator(&storage2, Some(base2.clone()));
    let code2 = crate::cli::run_with_role(
        strings([
            "--provider",
            "redmine",
            "--api-base",
            &base2,
            "version",
            "list",
        ]),
        Some("orchestrator"),
    );
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
    crate::workflow::clear_completed_bootstraps_for_tests();
    let origin = real_origin();
    let explicit = if origin.repository == "owner/repo" {
        "other/tools"
    } else {
        "owner/repo"
    };
    let (storage, _guard, dir) = temp_storage();
    seed_every_role(&storage);
    let _mirror = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    let repo_url2 = format!("https://git.example.com/{explicit}.git");
    let _mirror_url = EnvGuard::set("PHASEGENT_REDMINE_REPOSITORY_URL", &repo_url2);
    let bootstrap_id = crate::remote::redmine_identifier(explicit).unwrap();
    let mut responses = vec![
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        MockResponse::ok(closed_status()),
        MockResponse::ok(project_response(45, explicit, &bootstrap_id, "Workflow")),
    ];
    for (id, login) in SERVICE_ROLES {
        responses.extend(create_role(id, login, &format!("{login}-key")));
    }
    for _ in 0..4 {
        responses.extend(membership_added());
    }
    responses.extend([
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
    let (base, requests, server) = sequence(responses);
    let code = crate::cli::run_with_role(
        strings([
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
        ]),
        Some("orchestrator"),
    );
    assert_eq!(code, 0);
    let reqs = requests.recv().unwrap();
    // 3 project + 12 provisioning + 12 membership + 2 mirror + 1 issue.
    assert_eq!(reqs.len(), 30, "explicit repository run: {reqs:?}");
    assert!(reqs[0].starts_with(&format!("GET /projects/{bootstrap_id}.json")));
    assert!(!reqs.iter().any(|r| r.contains("/projects.json?limit=100")));
    server.join().unwrap();
    let _ = fs::remove_dir_all(dir);
}
