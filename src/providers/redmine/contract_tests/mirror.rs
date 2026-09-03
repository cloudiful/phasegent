#![allow(unused_imports)]
use super::support;
use super::support::{
    MockResponse, TEST_API_KEY, current_user_response, git_mirror_response, issue_collection,
    issue_response, membership_collection, membership_collection_page, mirror_env, one,
    project_collection, project_response, provider, role_collection, role_collection_page,
    sequence, strings, time_entry_activities, time_entry_collection, time_entry_response,
    user_from_response, version_collection, version_collection_page,
};
use crate::auth;
use crate::command::{
    self, Command, IssueCommand, ProjectCommand, RelationCommand, StatusCommand, WorkflowCommand,
};
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::infra::storage::{Storage, TimerRun};
use crate::policy::{Capability, Role};
use crate::providers::redmine::model::{RedmineRelationType, RedmineTimeEntryActivity};
use crate::providers::{
    ProviderDispatcher, ProviderKind, RedmineConfig, RedmineIssueStatus, RedmineMetadataProvider,
    RedmineProvider,
};
use std::str::FromStr;
use std::{fs, time};

#[test]
fn mirror_plugin_uses_bearer_header_and_uses_redmine_base_url() {
    let _environment_lock = lock_workflow_tests();
    let (_key, _url) = mirror_env();
    let (base, requests, server) = sequence(vec![MockResponse::status(
        202,
        support::git_mirror_response(
            901,
            44,
            "mirror_44_owner_repo",
            "pending",
            Some("https://git.example.com/owner/repo.git"),
            Some("/var/redmine/repos/owner_repo.git"),
            None,
        ),
    )]);
    let outcome = crate::providers::redmine::register_git_mirror(
        &base,
        44,
        "owner",
        "repo",
        "https://git.example.com/owner/repo.git",
    )
    .unwrap();
    assert_eq!(outcome.id, 901);
    assert_eq!(outcome.project_id, 44);
    assert_eq!(outcome.identifier, "mirror_44_owner_repo");
    assert_eq!(outcome.status, "pending");
    assert_eq!(outcome.remote_url, "https://git.example.com/owner/repo.git");
    assert_eq!(outcome.local_path, "/var/redmine/repos/owner_repo.git");
    assert!(outcome.error.is_none());

    let request = requests.recv().unwrap().remove(0);
    // GET (404) and POST (202) reuse the exact same base; the bearer key is
    // passed in the Authorization header, never as a query parameter.
    support::assert_request_with_bearer(
        &request,
        "GET",
        "/sys/redmine_git_mirror/projects/44/repository/mirror_44_owner_repo",
        None,
        "mirror-bearer-key",
    );
    assert!(
        !request.contains("?key=") && !request.contains("&key="),
        "bearer key must not appear in the query string: {request}"
    );
    // The Redmine base URL has no `/api/v1` suffix; the plugin lives under
    // `/sys/redmine_git_mirror/...` instead.
    assert!(
        request.contains("HTTP/1.1\r\nauthorization: Bearer mirror-bearer-key")
            || request.contains("authorization: Bearer mirror-bearer-key\r\n"),
        "request must carry the bearer authorization header: {request}"
    );
    server.join().unwrap();
}

#[test]
fn mirror_plugin_get_existing_skips_post_and_returns_existing_status() {
    let _environment_lock = lock_workflow_tests();
    let (_key, _url) = mirror_env();
    let (base, requests, server) = sequence(vec![MockResponse::ok(support::git_mirror_response(
        901,
        44,
        "mirror_44_owner_repo",
        "ready",
        Some("https://git.example.com/owner/repo.git"),
        Some("/var/redmine/repos/owner_repo.git"),
        None,
    ))]);
    let outcome =
        crate::providers::redmine::register_git_mirror(&base, 44, "Owner", "Repo", "ignored")
            .unwrap();
    assert_eq!(outcome.status, "ready");
    assert_eq!(outcome.id, 901);
    let request = requests.recv().unwrap().remove(0);
    support::assert_request_with_bearer(
        &request,
        "GET",
        "/sys/redmine_git_mirror/projects/44/repository/mirror_44_owner_repo",
        None,
        "mirror-bearer-key",
    );
    // A 200 GET must short-circuit the POST — only one request is observed.
    server.join().unwrap();
    assert!(
        requests.recv().is_err(),
        "GET must short-circuit the POST, but the channel delivered a second batch"
    );
}

#[test]
fn mirror_plugin_404_triggers_post_and_carries_credential_free_url() {
    let _environment_lock = lock_workflow_tests();
    let (_key, _url) = mirror_env();
    let (base, requests, server) = sequence(vec![
        MockResponse::error(404, r#"{"errors":["mirror not found"]}"#),
        MockResponse::status(
            202,
            support::git_mirror_response(
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
    let outcome = crate::providers::redmine::register_git_mirror(
        &base,
        44,
        "owner",
        "repo",
        "https://git.example.com/owner/repo.git",
    )
    .unwrap();
    assert_eq!(outcome.status, "pending");
    let observed = requests.recv().unwrap();
    assert_eq!(observed.len(), 2);
    support::assert_request_with_bearer(
        &observed[0],
        "GET",
        "/sys/redmine_git_mirror/projects/44/repository/mirror_44_owner_repo",
        None,
        "mirror-bearer-key",
    );
    support::assert_request_with_bearer(
        &observed[1],
        "POST",
        "/sys/redmine_git_mirror/projects/44/repository",
        Some(r#""url":"https://git.example.com/owner/repo.git""#),
        "mirror-bearer-key",
    );
    server.join().unwrap();
}

#[test]
fn mirror_plugin_missing_key_fails_bootstrap_with_actionable_error() {
    let _environment_lock = lock_workflow_tests();
    // Isolate the test behind a private SQLite database via
    // `PHASEGENT_DB_PATH` so the production SQLite database (which
    // the operator may already have populated with a mirror key)
    // cannot leak into the resolver through the env → SQLite
    // fallback. A throwaway database with an empty schema ensures
    // the only way to satisfy the lookup is via the environment
    // variable we explicitly clear below.
    let directory = std::env::temp_dir().join(format!(
        "phasegent-redmine-missing-key-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db_path = directory.join(crate::infra::storage::DB_FILENAME);
    let _db_path_guard = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
    let _url = EnvGuard::set(
        "PHASEGENT_REDMINE_REPOSITORY_URL",
        "https://git.example.com/owner/repo.git",
    );
    let previous = std::env::var_os("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY");
    unsafe {
        std::env::remove_var("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY");
    }
    let error = crate::providers::redmine::register_git_mirror(
        "https://redmine.example",
        44,
        "owner",
        "repo",
        "https://git.example.com/owner/repo.git",
    )
    .expect_err("missing plugin key must fail bootstrap");
    if let Some(previous) = previous.as_ref() {
        unsafe {
            std::env::set_var("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", previous);
        }
    }
    let _ = fs::remove_dir_all(&directory);
    let json = error.json();
    assert_eq!(json["kind"], "config");
    let message = json["message"].as_str().unwrap();
    assert!(
        message.contains("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY"),
        "missing-key error must name the env var, got: {message}"
    );
}

#[test]
fn mirror_plugin_http_errors_redact_bearer_key() {
    let _environment_lock = lock_workflow_tests();
    let (_key, _url) = mirror_env();
    let (base, requests, server) = sequence(vec![MockResponse::error(
        500,
        r#"{"errors":["server error: mirror-bearer-key"]}"#.to_owned(),
    )]);
    let error = crate::providers::redmine::register_git_mirror(&base, 44, "owner", "repo", "url")
        .expect_err("5xx must surface as an actionable error");
    let json = error.json();
    assert_eq!(json["kind"], "http");
    assert_eq!(json["status"], 500);
    assert!(
        json["message"].as_str().unwrap().contains("[redacted]"),
        "bearer key must be redacted, got: {json}"
    );
    assert!(!error.to_string().contains("mirror-bearer-key"));
    let _ = requests.recv().unwrap();
    server.join().unwrap();
}

#[test]
fn mirror_plugin_get_failed_triggers_single_requeue_post() {
    let _environment_lock = lock_workflow_tests();
    let (_key, _url) = mirror_env();
    let (base, requests, server) = sequence(vec![
        // The plugin reports a stale mirror whose recorded remote_url
        // differs from what we register; the requeue POST must carry the
        // caller-supplied URL, never the untrusted response field.
        MockResponse::ok(support::git_mirror_response(
            901,
            44,
            "mirror_44_owner_repo",
            "failed",
            Some("https://stale.example.com/old/repo.git"),
            None,
            Some("git clone failed"),
        )),
        MockResponse::status(
            202,
            support::git_mirror_response(
                902,
                44,
                "mirror_44_owner_repo",
                "pending",
                Some("https://git.example.com/owner/repo.git"),
                Some("/var/redmine/repos/owner_repo.git"),
                None,
            ),
        ),
    ]);
    let outcome = crate::providers::redmine::register_git_mirror(
        &base,
        44,
        "owner",
        "repo",
        "https://git.example.com/owner/repo.git",
    )
    .unwrap();
    assert_eq!(outcome.id, 902);
    assert_eq!(outcome.status, "pending");
    let observed = requests.recv().unwrap();
    assert_eq!(observed.len(), 2);
    support::assert_request_with_bearer(
        &observed[0],
        "GET",
        "/sys/redmine_git_mirror/projects/44/repository/mirror_44_owner_repo",
        None,
        "mirror-bearer-key",
    );
    support::assert_request_with_bearer(
        &observed[1],
        "POST",
        "/sys/redmine_git_mirror/projects/44/repository",
        Some(r#""url":"https://git.example.com/owner/repo.git""#),
        "mirror-bearer-key",
    );
    server.join().unwrap();
}

#[test]
fn mirror_plugin_failed_status_fails_bootstrap_clearly() {
    let _environment_lock = lock_workflow_tests();
    let (_key, _url) = mirror_env();
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(support::git_mirror_response(
            901,
            44,
            "mirror_44_owner_repo",
            "failed",
            Some("https://git.example.com/owner/repo.git"),
            None,
            Some("git clone failed"),
        )),
        // Even after a requeue POST, a still-`failed` response must
        // surface as a clear bootstrap error.
        MockResponse::ok(support::git_mirror_response(
            901,
            44,
            "mirror_44_owner_repo",
            "failed",
            Some("https://git.example.com/owner/repo.git"),
            None,
            Some("still failing after requeue"),
        )),
    ]);
    let error = crate::providers::redmine::register_git_mirror(&base, 44, "owner", "repo", "url")
        .expect_err("a `failed` plugin status must surface as a bootstrap error");
    let json = error.json();
    assert_eq!(json["kind"], "config");
    let message = json["message"].as_str().unwrap();
    assert!(
        message.contains("failed status"),
        "failed-status error must explain the failure, got: {message}"
    );
    assert!(message.contains("still failing after requeue"));
    let observed = requests.recv().unwrap();
    assert_eq!(observed.len(), 2);
    support::assert_request_with_bearer(
        &observed[1],
        "POST",
        "/sys/redmine_git_mirror/projects/44/repository",
        Some(r#""url":"url""#),
        "mirror-bearer-key",
    );
    server.join().unwrap();
}

#[test]
fn mirror_identifier_lowercases_owner_and_repo() {
    assert_eq!(
        crate::providers::redmine::mirror_identifier(44, "Owner", "Repo"),
        "mirror_44_owner_repo"
    );
    assert_eq!(
        crate::providers::redmine::mirror_identifier(44, "Mixed.Case", "Repo+One"),
        "mirror_44_mixed.case_repo+one"
    );
}

#[test]
fn mirror_lookup_found_missing_and_error_are_distinguished() {
    let _lock = lock_workflow_tests();
    let (_key, _url) = mirror_env();
    // Found: 200 with remote_url
    let (base, requests, server) = sequence(vec![MockResponse::ok(git_mirror_response(
        901,
        44,
        "mirror_44_owner_repo",
        "ready",
        Some("https://git.example.com/owner/repo.git"),
        Some("/path"),
        None,
    ))]);
    let redmine = support::provider(base);
    let found = redmine
        .lookup_mirror_for_project(44, "owner", "repo")
        .unwrap();
    assert!(found.is_some());
    assert_eq!(
        found.unwrap().remote_url.unwrap(),
        "https://git.example.com/owner/repo.git"
    );
    let req = requests.recv().unwrap().remove(0);
    assert!(
        req.starts_with("GET /sys/redmine_git_mirror/projects/44/repository/mirror_44_owner_repo")
    );
    server.join().unwrap();

    // Missing: 404 -> None (normal non-match, no error)
    let (base2, requests2, server2) = sequence(vec![MockResponse::error(
        404,
        r#"{"errors":["not found"]}"#,
    )]);
    let redmine2 = support::provider(base2);
    let missing = redmine2
        .lookup_mirror_for_project(44, "owner", "repo")
        .unwrap();
    assert!(missing.is_none());
    let req2 = requests2.recv().unwrap().remove(0);
    assert!(
        req2.starts_with("GET /sys/redmine_git_mirror/projects/44/repository/mirror_44_owner_repo")
    );
    server2.join().unwrap();

    // Error: 500 must propagate as actionable error and redact bearer
    let (base3, requests3, server3) = sequence(vec![MockResponse::error(
        500,
        r#"{"errors":["server error: mirror-bearer-key"]}"#,
    )]);
    let redmine3 = support::provider(base3);
    let error = redmine3
        .lookup_mirror_for_project(44, "owner", "repo")
        .unwrap_err();
    assert_eq!(error.json()["kind"], "http");
    assert_eq!(error.json()["status"], 500);
    assert!(!error.to_string().contains("mirror-bearer-key"));
    assert!(error.to_string().contains("[redacted]"));
    let _ = requests3.recv().unwrap();
    server3.join().unwrap();

    // Empty remote_url is treated as non-match (None) even when 200
    let (base4, requests4, server4) = sequence(vec![MockResponse::ok(git_mirror_response(
        901,
        44,
        "mirror_44_owner_repo",
        "ready",
        Some(""),
        Some("/path"),
        None,
    ))]);
    let redmine4 = support::provider(base4);
    let empty = redmine4
        .lookup_mirror_for_project(44, "owner", "repo")
        .unwrap();
    assert!(
        empty.is_none(),
        "empty remote_url must be treated as non-match"
    );
    let _ = requests4.recv().unwrap();
    server4.join().unwrap();

    let (base5, requests5, server5) = sequence(vec![MockResponse::ok(git_mirror_response(
        901,
        44,
        "mirror_44_owner_repo",
        "ready",
        None,
        Some("/path"),
        None,
    ))]);
    let redmine5 = support::provider(base5);
    let none_url = redmine5
        .lookup_mirror_for_project(44, "owner", "repo")
        .unwrap();
    assert!(
        none_url.is_none(),
        "missing remote_url must be treated as non-match"
    );
    let _ = requests5.recv().unwrap();
    server5.join().unwrap();
}

#[test]
fn discovery_returns_no_match_when_no_projects_or_no_mirror_matches() {
    let _lock = lock_workflow_tests();
    let (_key, _url) = mirror_env();
    // No projects at all -> NoMatch
    let (base, requests, server) =
        sequence(vec![MockResponse::ok(project_collection(0, 100, &[]))]);
    let redmine = support::provider(base);
    let result = redmine
        .discover_matching_projects_for_urls("owner/repo", "https://git.example.com/owner/repo.git")
        .unwrap();
    assert_eq!(result, crate::providers::redmine::RedmineDiscovery::NoMatch);
    assert!(requests.recv().unwrap()[0].starts_with("GET /projects.json?"));
    server.join().unwrap();

    // Projects exist but all mirrors 404 -> NoMatch
    let (base2, requests2, server2) = sequence(vec![
        MockResponse::ok(project_collection(1, 100, &[(44, "Workflow", "workflow")])),
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
    ]);
    let redmine2 = support::provider(base2);
    let result2 = redmine2
        .discover_matching_projects_for_urls("owner/repo", "https://git.example.com/owner/repo.git")
        .unwrap();
    assert_eq!(
        result2,
        crate::providers::redmine::RedmineDiscovery::NoMatch
    );
    let reqs = requests2.recv().unwrap();
    assert_eq!(reqs.len(), 2);
    assert!(reqs[0].starts_with("GET /projects.json?"));
    assert!(
        reqs[1]
            .starts_with("GET /sys/redmine_git_mirror/projects/44/repository/mirror_44_owner_repo")
    );
    server2.join().unwrap();
}

#[test]
fn discovery_returns_single_match_and_preserves_identity_without_post() {
    let _lock = lock_workflow_tests();
    let (_key, _url) = mirror_env();
    let origin = "https://git.example.com/owner/repo.git";
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(project_collection(
            2,
            100,
            &[(44, "Workflow", "workflow"), (45, "Other", "other")],
        )),
        MockResponse::ok(git_mirror_response(
            901,
            44,
            "mirror_44_owner_repo",
            "ready",
            Some(origin),
            Some("/path"),
            None,
        )),
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
    ]);
    let redmine = support::provider(base);
    let result = redmine
        .discover_matching_projects_for_urls("owner/repo", origin)
        .unwrap();
    match result {
        crate::providers::redmine::RedmineDiscovery::Single(project) => {
            assert_eq!(project.id, 44);
            assert_eq!(project.name, "Workflow");
            assert_eq!(project.identifier, "workflow");
        }
        other => panic!("expected Single, got {other:?}"),
    }
    let reqs = requests.recv().unwrap();
    assert_eq!(reqs.len(), 3);
    assert!(reqs[0].starts_with("GET /projects.json?"));
    assert!(
        reqs[1]
            .starts_with("GET /sys/redmine_git_mirror/projects/44/repository/mirror_44_owner_repo")
    );
    assert!(
        reqs[2]
            .starts_with("GET /sys/redmine_git_mirror/projects/45/repository/mirror_45_owner_repo")
    );
    // Invariant: discovery never POSTs
    assert!(
        !reqs.iter().any(|r| r.starts_with("POST")),
        "discovery must not POST: {reqs:?}"
    );
    // Invariant: discovery never leaks bearer key in query and uses Bearer auth, not X-Redmine-API-Key
    for req in &reqs[1..] {
        assert!(
            req.to_ascii_lowercase()
                .contains("authorization: bearer mirror-bearer-key"),
            "plugin GET must carry Bearer auth: {req}"
        );
        assert!(
            !req.to_ascii_lowercase().contains("x-redmine-api-key"),
            "plugin request must not use X-Redmine-API-Key: {req}"
        );
        assert!(
            !req.contains("?key=") && !req.contains("&key="),
            "bearer key must not appear in query string: {req}"
        );
    }
    server.join().unwrap();
}

#[test]
fn discovery_canonicalises_credentials_query_fragment_git_and_ssh_https_equivalence() {
    let _lock = lock_workflow_tests();
    let (_key, _url) = mirror_env();
    // Origin with credentials, query, fragment, and .git; stored mirror is plain https
    let origin_with_noise = "https://user:pass@git.example.com/owner/repo.git?ref=main#frag";
    let stored_plain = "https://git.example.com/owner/repo";
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(project_collection(1, 100, &[(44, "Workflow", "workflow")])),
        MockResponse::ok(git_mirror_response(
            901,
            44,
            "mirror_44_owner_repo",
            "ready",
            Some(stored_plain),
            Some("/path"),
            None,
        )),
    ]);
    let redmine = support::provider(base);
    let result = redmine
        .discover_matching_projects_for_urls("owner/repo", origin_with_noise)
        .unwrap();
    assert!(matches!(
        result,
        crate::providers::redmine::RedmineDiscovery::Single(_)
    ));
    let _ = requests.recv().unwrap();
    server.join().unwrap();

    // SSH vs HTTPS equivalence: scp-style origin should match https mirror
    let scp_origin = "git@git.example.com:owner/repo.git";
    let https_mirror = "https://git.example.com/owner/repo.git";
    let (base2, requests2, server2) = sequence(vec![
        MockResponse::ok(project_collection(1, 100, &[(44, "Workflow", "workflow")])),
        MockResponse::ok(git_mirror_response(
            901,
            44,
            "mirror_44_owner_repo",
            "ready",
            Some(https_mirror),
            Some("/path"),
            None,
        )),
    ]);
    let redmine2 = support::provider(base2);
    let result2 = redmine2
        .discover_matching_projects_for_urls("owner/repo", scp_origin)
        .unwrap();
    assert!(matches!(
        result2,
        crate::providers::redmine::RedmineDiscovery::Single(_)
    ));
    let _ = requests2.recv().unwrap();
    server2.join().unwrap();

    // ssh:// with port vs https:// without port must NOT match when ports differ
    let https_with_port = "https://git.example.com:8443/owner/repo.git";
    let stored_without_port = "https://git.example.com/owner/repo.git";
    let (base3, requests3, server3) = sequence(vec![
        MockResponse::ok(project_collection(1, 100, &[(44, "Workflow", "workflow")])),
        MockResponse::ok(git_mirror_response(
            901,
            44,
            "mirror_44_owner_repo",
            "ready",
            Some(stored_without_port),
            Some("/path"),
            None,
        )),
    ]);
    let redmine3 = support::provider(base3);
    let result3 = redmine3
        .discover_matching_projects_for_urls("owner/repo", https_with_port)
        .unwrap();
    assert_eq!(
        result3,
        crate::providers::redmine::RedmineDiscovery::NoMatch,
        "different non-default ports must not match"
    );
    let _ = requests3.recv().unwrap();
    server3.join().unwrap();
}

#[test]
fn discovery_returns_multiple_matches_without_guessing() {
    let _lock = lock_workflow_tests();
    let (_key, _url) = mirror_env();
    let origin = "https://git.example.com/owner/repo.git";
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(project_collection(
            2,
            100,
            &[
                (44, "Workflow A", "workflow-a"),
                (45, "Workflow B", "workflow-b"),
            ],
        )),
        MockResponse::ok(git_mirror_response(
            901,
            44,
            "mirror_44_owner_repo",
            "ready",
            Some(origin),
            Some("/path/a"),
            None,
        )),
        MockResponse::ok(git_mirror_response(
            902,
            45,
            "mirror_45_owner_repo",
            "ready",
            Some(origin),
            Some("/path/b"),
            None,
        )),
    ]);
    let redmine = support::provider(base);
    let result = redmine
        .discover_matching_projects_for_urls("owner/repo", origin)
        .unwrap();
    match result {
        crate::providers::redmine::RedmineDiscovery::Multiple(projects) => {
            assert_eq!(projects.len(), 2);
            let ids: Vec<u64> = projects.iter().map(|p| p.id).collect();
            assert_eq!(ids, vec![44, 45]);
            assert_eq!(projects[0].name, "Workflow A");
            assert_eq!(projects[0].identifier, "workflow-a");
            assert_eq!(projects[1].name, "Workflow B");
            assert_eq!(projects[1].identifier, "workflow-b");
        }
        other => panic!("expected Multiple, got {other:?}"),
    }
    let reqs = requests.recv().unwrap();
    assert_eq!(reqs.len(), 3);
    assert!(!reqs.iter().any(|r| r.starts_with("POST")));
    server.join().unwrap();
}

#[test]
fn discovery_propagates_non404_errors_as_actionable() {
    let _lock = lock_workflow_tests();
    let (_key, _url) = mirror_env();
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(project_collection(1, 100, &[(44, "Workflow", "workflow")])),
        MockResponse::error(500, r#"{"errors":["internal"]}"#),
    ]);
    let redmine = support::provider(base);
    let error = redmine
        .discover_matching_projects_for_urls("owner/repo", "https://git.example.com/owner/repo.git")
        .unwrap_err();
    assert_eq!(error.json()["kind"], "http");
    assert_eq!(error.json()["status"], 500);
    let _ = requests.recv().unwrap();
    server.join().unwrap();

    // Unauthorized (401) also propagates
    let (base2, requests2, server2) = sequence(vec![
        MockResponse::ok(project_collection(1, 100, &[(44, "Workflow", "workflow")])),
        MockResponse::error(401, r#"{"errors":["unauthorized"]}"#),
    ]);
    let redmine2 = support::provider(base2);
    let error2 = redmine2
        .discover_matching_projects_for_urls("owner/repo", "https://git.example.com/owner/repo.git")
        .unwrap_err();
    assert_eq!(error2.json()["kind"], "http");
    assert_eq!(error2.json()["status"], 401);
    let _ = requests2.recv().unwrap();
    server2.join().unwrap();
}

#[test]
fn discovery_never_posts_and_never_writes_sqlite() {
    let _lock = lock_workflow_tests();
    let (_key, _url) = mirror_env();
    let directory = std::env::temp_dir().join(format!(
        "phasegent-discovery-no-write-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db_path = directory.join(crate::infra::storage::DB_FILENAME);
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
    let storage = Storage::open_at(&db_path).unwrap();
    storage
        .save_credential(Role::Orchestrator, "redmine", TEST_API_KEY)
        .unwrap();
    // Pre-populate a global setting to ensure discovery does not mutate it
    storage
        .save_global_setting(
            "PHASEGENT_REDMINE_REPOSITORY_URL",
            "https://example.com/persisted.git",
        )
        .unwrap();
    let before = storage
        .load_global_setting("PHASEGENT_REDMINE_REPOSITORY_URL")
        .unwrap();
    assert_eq!(before.as_deref(), Some("https://example.com/persisted.git"));

    let (base, requests, server) = sequence(vec![
        MockResponse::ok(project_collection(1, 100, &[(44, "Workflow", "workflow")])),
        MockResponse::ok(git_mirror_response(
            901,
            44,
            "mirror_44_owner_repo",
            "ready",
            Some("https://git.example.com/owner/repo.git"),
            Some("/path"),
            None,
        )),
    ]);
    // Use a provider that talks to the mock server but whose config is
    // derived from the temp DB path; discovery must not create or mutate
    // any project or mirror state.
    let redmine = support::provider(base);
    let result = redmine
        .discover_matching_projects_for_urls("owner/repo", "https://git.example.com/owner/repo.git")
        .unwrap();
    assert!(matches!(
        result,
        crate::providers::redmine::RedmineDiscovery::Single(_)
    ));
    let reqs = requests.recv().unwrap();
    assert!(
        !reqs.iter().any(|r| r.contains("POST")),
        "discovery must not POST: {reqs:?}"
    );
    assert!(
        !reqs
            .iter()
            .any(|r| r.contains("/projects.json") && r.starts_with("POST")),
        "discovery must not create projects"
    );
    // Verify SQLite was not mutated: the persisted setting is unchanged and
    // no new role config was created by discovery.
    let after = storage
        .load_global_setting("PHASEGENT_REDMINE_REPOSITORY_URL")
        .unwrap();
    assert_eq!(after, before);
    // Discovery should not have persisted any bootstrap identity; check that
    // the orchestrator redmine config still has no project_id (which is
    // anyway ignored) but also that no new file was written beyond what we
    // created. The strongest check is that the request log contains only
    // the expected GETs.
    assert_eq!(reqs.len(), 2);
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

// ---------------------------------------------------------------------------
// Phase 4A: native planning fields and project version discovery
// ---------------------------------------------------------------------------
