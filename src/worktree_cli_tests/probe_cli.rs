use super::cli_support::*;
use super::support::*;
use super::*;

use crate::cli::worktree::build_probe;

fn probe_value(path: Option<&str>, issue: Option<u64>, session: Option<&str>) -> serde_json::Value {
    let report = build_probe(path, issue, session).expect("probe builds");
    serde_json::to_value(report).expect("probe serialises")
}

#[test]
fn probe_json_envelope_has_a_stable_bounded_key_set() {
    let dir = TempDir::new("probe-keys");
    let json = probe_value(Some(&dir.path().to_string_lossy()), None, None);
    let object = json.as_object().expect("probe envelope must be an object");
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "branch",
            "clean",
            "errors",
            "exists",
            "head",
            "is_git_worktree",
            "is_main_checkout",
            "lease",
            "path",
            "resolved",
        ],
        "the probe JSON contract must stay bounded"
    );
}

#[test]
fn probe_missing_path_returns_a_structured_result_not_an_error() {
    let missing = crate::test_scratch::root().join(format!(
        "phasegent-probe-cli-missing-{}-{}",
        std::process::id(),
        now_unix_secs()
    ));
    let _ = std::fs::remove_dir_all(&missing);
    let missing_text = missing.to_string_lossy().to_string();
    let json = probe_value(Some(&missing_text), None, None);
    assert_eq!(json["resolved"], serde_json::json!(true));
    assert_eq!(json["path"], serde_json::json!(missing_text));
    assert_eq!(json["exists"], serde_json::json!(false));
    assert_eq!(json["is_git_worktree"], serde_json::json!(false));
    assert_eq!(json["clean"], serde_json::Value::Null);
    assert_eq!(json["lease"], serde_json::Value::Null);
    assert!(
        json["errors"]
            .as_array()
            .is_some_and(|errors| !errors.is_empty()),
        "a missing path must report a structured error: {json}"
    );
}

#[test]
fn probe_default_checkout_reports_the_main_checkout_read_only() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("probe-cli-main") else {
        return;
    };
    let json = in_temp_repo(&repo, || probe_value(None, None, None));
    assert_eq!(json["resolved"], serde_json::json!(true));
    assert_eq!(json["exists"], serde_json::json!(true));
    assert_eq!(json["is_git_worktree"], serde_json::json!(true));
    assert_eq!(json["clean"], serde_json::json!(true));
    assert_eq!(json["is_main_checkout"], serde_json::json!(true));
    assert_eq!(json["branch"], serde_json::json!(repo.head_branch));
    assert!(
        json["head"].as_str().is_some_and(|head| !head.is_empty()),
        "the main checkout must report HEAD: {json}"
    );
    assert_eq!(json["errors"], serde_json::json!([]));
}

#[test]
fn probe_issue_selector_resolves_the_lease_path_without_writing() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("probe-cli-issue") else {
        return;
    };
    let (db_temp, _cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("probe-cli-issue");
    let storage = Storage::open().expect("storage");
    ensure_schema(&storage).expect("ensure_schema");
    let branch = "phasegent/7-probe0";
    let target = add_real_worktree(&repo, branch);
    let identity = repo_identity(&ProcessWorktreeRunner::new(), repo.dir.path()).expect("identity");
    let heartbeat = now_unix_secs();
    insert_lease_for_identity(
        &storage,
        "lease-probe",
        &identity,
        "session-X",
        LEASE_STATUS_ACTIVE,
        heartbeat,
        &target.to_string_lossy(),
    );
    let json = in_temp_repo(&repo, || probe_value(None, Some(7), Some("session-X")));
    assert_eq!(json["resolved"], serde_json::json!(true));
    assert_eq!(json["path"], serde_json::json!(target.to_string_lossy()));
    assert_eq!(json["exists"], serde_json::json!(true));
    assert_eq!(json["is_git_worktree"], serde_json::json!(true));
    assert_eq!(
        json["is_main_checkout"],
        serde_json::json!(false),
        "a resolved linked worktree is not the main checkout: {json}"
    );
    assert_eq!(json["branch"], serde_json::json!(branch));
    assert_eq!(json["lease"]["lease_id"], serde_json::json!("lease-probe"));
    assert_eq!(json["lease"]["issue"], serde_json::json!(7));
    assert_eq!(json["lease"]["session"], serde_json::json!("session-X"));
    // Read-only guarantee: the probe must not flip the lease or touch its
    // heartbeat.
    let rows = crate::worktree::list_for_repo(&storage, &identity).expect("list leases");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, LEASE_STATUS_ACTIVE);
    assert_eq!(rows[0].heartbeat_at, heartbeat);
    drop(db_temp);
}

#[test]
fn probe_issue_selector_without_a_match_returns_the_empty_envelope() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("probe-cli-nomatch") else {
        return;
    };
    let (db_temp, _storage, _db_env, _cache_env) = open_temp_db_and_cache("probe-cli-nomatch");
    let json = in_temp_repo(&repo, || probe_value(None, Some(595), Some("session-X")));
    assert_eq!(json["resolved"], serde_json::json!(false));
    assert_eq!(json["path"], serde_json::Value::Null);
    assert_eq!(json["exists"], serde_json::json!(false));
    assert_eq!(json["is_git_worktree"], serde_json::json!(false));
    assert_eq!(json["lease"], serde_json::Value::Null);
    assert_eq!(
        json["errors"],
        serde_json::json!([]),
        "a no-match result is stable and empty, not an error"
    );
    drop(db_temp);
}

#[test]
fn execute_probe_returns_zero_and_never_syncs() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("probe-cli-exit") else {
        return;
    };
    let (db_temp, _storage, _db_env, _cache_env) = open_temp_db_and_cache("probe-cli-exit");
    let exit = in_temp_repo(&repo, || {
        execute_worktree(
            Some(Role::Executor),
            WorktreeCommand::Probe {
                path: None,
                issue: None,
                session: None,
            },
        )
    });
    assert_eq!(exit, 0, "a read-only probe exits 0");
    drop(db_temp);
}
