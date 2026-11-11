use super::support::*;
use super::*;

#[test]
fn list_payload_reports_the_registered_directory_fields() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("read-list") else {
        return;
    };
    let (_db, _db_env) = open_temp_db("read-list");
    let identity = repo_identity(&ProcessWorktreeRunner::new(), repo.dir.path()).expect("identity");
    let record = record_directory(
        &identity,
        repo.dir.path(),
        repo.dir.path(),
        &repo.head_branch,
        7,
        "session-X",
    );
    let storage = Storage::open().expect("storage");
    let payload = crate::cli::worktree::query::list_payload(&storage, &identity).expect("payload");
    assert_eq!(payload["repo_identity"], serde_json::json!(identity));
    let directory = &payload["directories"][0];
    assert_eq!(
        directory["directory_id"],
        serde_json::json!(record.directory_id)
    );
    assert_eq!(
        directory["worktree_path"],
        serde_json::json!(repo.dir.path().to_string_lossy())
    );
    assert_eq!(directory["branch"], serde_json::json!(repo.head_branch));
    assert_eq!(directory["legacy_issues"][0]["issue"], serde_json::json!(7));
    assert_eq!(directory["sessions"], serde_json::json!(["session-X"]));
    for absent in ["status", "heartbeat_at", "release_reason", "expires_at"] {
        assert!(
            directory.get(absent).is_none(),
            "the directory JSON never carries lease state '{absent}'"
        );
    }
    assert_eq!(
        open_records(&identity)[0].directory_id,
        record.directory_id,
        "a read never changes the registry"
    );
}

#[test]
fn status_and_list_reads_create_no_lock_or_registry_on_an_absent_store() {
    let _lock = lock_workflow_tests();
    let cache = temp_cache("read-no-lock");
    let _cache_env = EnvGuard::set(
        "PHASEGENT_WORKTREE_CACHE_DIR",
        cache.path().to_string_lossy().as_ref(),
    );
    let (_db, _db_env) = open_temp_db("read-no-lock");
    let status = execute_worktree(
        Some(Role::Reviewer),
        WorktreeCommand::Status {
            issue: 7,
            page: None,
        },
    );
    assert_eq!(status, 0);
    let (listing_dir, _listing_env) = open_temp_db("read-no-lock-list");
    let listing = execute_worktree(
        Some(Role::Reviewer),
        WorktreeCommand::List {
            repo: Some(listing_dir.path().to_string_lossy().to_string()),
            no_sync: true,
            page: None,
        },
    );
    assert!(
        listing == 0 || listing == 2,
        "a list read may report an unusable repo as a structured error, not a crash: {listing}"
    );
    assert!(
        !cache.path().join("worktrees").exists(),
        "status/list never create a lock directory or file"
    );
}
