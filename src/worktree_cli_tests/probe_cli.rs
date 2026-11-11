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
            "directory",
            "errors",
            "exists",
            "head",
            "is_git_worktree",
            "is_main_checkout",
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
        crate::worktree::now_unix_secs()
    ));
    let _ = std::fs::remove_dir_all(&missing);
    let missing_text = missing.to_string_lossy().to_string();
    let json = probe_value(Some(&missing_text), None, None);
    assert_eq!(json["resolved"], serde_json::json!(true));
    assert_eq!(json["path"], serde_json::json!(missing_text));
    assert_eq!(json["exists"], serde_json::json!(false));
    assert_eq!(json["is_git_worktree"], serde_json::json!(false));
    assert_eq!(json["clean"], serde_json::Value::Null);
    assert_eq!(json["directory"], serde_json::Value::Null);
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
    let cache = temp_cache("probe-cli-main");
    let _cache_env = EnvGuard::set(
        "PHASEGENT_WORKTREE_CACHE_DIR",
        cache.path().to_string_lossy().as_ref(),
    );
    let previous = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(repo.dir.path()).expect("enter temp repo");
    let json = probe_value(None, None, None);
    std::env::set_current_dir(previous).expect("restore cwd");
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
    assert!(
        !cache.path().join("worktrees").exists(),
        "a probe never creates a lock or worktree directory"
    );
}

#[test]
fn probe_issue_selector_resolves_the_registered_directory_without_writing() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("probe-cli-issue") else {
        return;
    };
    let (db, _db_env) = open_temp_db("probe-cli-issue");
    let cache = temp_cache("probe-cli-issue");
    let _cache_env = EnvGuard::set(
        "PHASEGENT_WORKTREE_CACHE_DIR",
        cache.path().to_string_lossy().as_ref(),
    );
    let target = repo.dir.path().join("wt-probe");
    crate::worktree::worktree_add(
        &ProcessWorktreeRunner::new(),
        repo.dir.path(),
        &target,
        "phasegent/7-probe0",
    )
    .expect("worktree add");
    let identity = repo_identity(&ProcessWorktreeRunner::new(), repo.dir.path()).expect("identity");
    let record = record_directory(
        &identity,
        repo.dir.path(),
        &target,
        "phasegent/7-probe0",
        7,
        "session-X",
    );
    let previous = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(repo.dir.path()).expect("enter temp repo");
    let json = probe_value(None, Some(7), Some("session-X"));
    std::env::set_current_dir(previous).expect("restore cwd");
    assert_eq!(json["resolved"], serde_json::json!(true));
    assert_eq!(json["path"], serde_json::json!(target.to_string_lossy()));
    assert_eq!(json["exists"], serde_json::json!(true));
    assert_eq!(json["is_git_worktree"], serde_json::json!(true));
    assert_eq!(json["is_main_checkout"], serde_json::json!(false));
    assert_eq!(json["branch"], serde_json::json!("phasegent/7-probe0"));
    assert_eq!(
        json["directory"]["directory_id"],
        serde_json::json!(record.directory_id)
    );
    assert_eq!(
        json["directory"]["legacy_issues"][0]["issue"],
        serde_json::json!(7)
    );
    assert_eq!(
        json["directory"]["sessions"],
        serde_json::json!(["session-X"])
    );
    assert_eq!(
        open_records(&identity)[0].sessions,
        vec!["session-X"],
        "the probe never changes registry evidence"
    );
    drop(db);
}

#[test]
fn probe_issue_selector_without_a_match_returns_the_empty_envelope() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("probe-cli-nomatch") else {
        return;
    };
    let (_db, _db_env) = open_temp_db("probe-cli-nomatch");
    let identity = repo_identity(&ProcessWorktreeRunner::new(), repo.dir.path()).expect("identity");
    record_directory(
        &identity,
        repo.dir.path(),
        repo.dir.path(),
        &repo.head_branch,
        7,
        "session-X",
    );
    let previous = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(repo.dir.path()).expect("enter temp repo");
    let json = probe_value(None, Some(595), Some("session-X"));
    std::env::set_current_dir(previous).expect("restore cwd");
    assert_eq!(json["resolved"], serde_json::json!(false));
    assert_eq!(json["path"], serde_json::Value::Null);
    assert_eq!(json["exists"], serde_json::json!(false));
    assert_eq!(json["is_git_worktree"], serde_json::json!(false));
    assert_eq!(json["directory"], serde_json::Value::Null);
    assert_eq!(
        json["errors"],
        serde_json::json!([]),
        "a no-match result is stable and empty, not an error"
    );
}

#[test]
fn execute_probe_returns_zero_and_never_syncs() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("probe-cli-exit") else {
        return;
    };
    let (_db, _db_env) = open_temp_db("probe-cli-exit");
    let previous = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(repo.dir.path()).expect("enter temp repo");
    let exit = execute_worktree(
        Some(Role::Executor),
        WorktreeCommand::Probe {
            path: None,
            issue: None,
            session: None,
        },
    );
    std::env::set_current_dir(previous).expect("restore cwd");
    assert_eq!(exit, 0, "a read-only probe exits 0");
}
