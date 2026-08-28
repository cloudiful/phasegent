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
use crate::ci_model::CiRunsFilter;
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

// ---------------------------------------------------------------------------
// Phase 4A: native planning fields and project version discovery
// ---------------------------------------------------------------------------
