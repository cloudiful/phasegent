use super::cli_support::*;
use super::support::*;
use super::*;

use crate::lifecycle::cleanup::TargetResolver;
use crate::worktree::opencode::test_support::FakeOpenCodeApi;

/// Build a scope and pinned-active set over the temp repo's rows.
fn pass(
    storage: &Storage,
    repo: &TempRepo,
    identity: &str,
    api: &FakeOpenCodeApi,
    source: &ScriptedIssueStateSource,
    mode: PruneMode,
) -> PruneSummary {
    let runner = ProcessWorktreeRunner::new();
    let rows = list_all_for_repo(storage, identity).expect("list");
    let pinned = pinned_active_paths(&rows);
    let target = TargetResolver::new(repo.dir.path(), identity);
    let scope = PruneScope {
        identity,
        repo_path: repo.dir.path(),
        rows: &rows,
        target: &target,
    };
    prune_pass(
        storage,
        &runner,
        api,
        api,
        &scope,
        &pinned,
        source,
        14,
        mode,
        &|_| {},
    )
}

#[test]
fn prune_dry_run_classifies_every_status_correctly() {
    let _lock = lock_workflow_tests();
    let (_temp, storage, _env) = open_temp_db("prune-classify");
    ensure_schema(&storage).expect("ensure_schema");
    let now = now_unix_secs();
    let old = now - 15 * 86_400;
    // A directory that exists but is not a removable candidate: the shared
    // guards cannot prove it safe, so it is protected.
    let protected_dir = TempDir::new("prune-protected-stub");
    let protected_path = protected_dir.path().to_string_lossy().to_string();
    // A recognisable unknown lease status keeps the directory safely.
    let unknown_dir = TempDir::new("prune-unknown-stub");
    let unknown_path = unknown_dir.path().to_string_lossy().to_string();
    let rows = vec![
        fresh_lease_row(
            "lease-active",
            1,
            "s",
            "/tmp/repo/wt-active",
            "phasegent/1-active",
            LEASE_STATUS_ACTIVE,
            old,
        ),
        fresh_lease_row(
            "lease-protected",
            1,
            "s",
            &protected_path,
            "phasegent/1-protected",
            LEASE_STATUS_RETAINED,
            old,
        ),
        fresh_lease_row(
            "lease-unknown",
            1,
            "s",
            &unknown_path,
            "phasegent/1-unknown",
            "quarantined",
            old,
        ),
        fresh_lease_row(
            "lease-absent",
            1,
            "s",
            "/tmp/repo/does-not-exist",
            "phasegent/1-absent",
            LEASE_STATUS_RETAINED,
            old,
        ),
        fresh_lease_row(
            "lease-released",
            1,
            "s",
            "/tmp/repo/wt-released",
            "phasegent/1-released",
            LEASE_STATUS_RELEASED,
            old,
        ),
    ];
    for row in &rows {
        insert_row(&storage, row);
    }
    let runner = ProcessWorktreeRunner::new();
    let api = FakeOpenCodeApi::empty();
    let source = ScriptedIssueStateSource::closed();
    let stored = list_all_for_repo(&storage, "/tmp/repo").expect("list");
    let pinned = pinned_active_paths(&stored);
    let repo_path = std::path::Path::new("/tmp/repo");
    let target = TargetResolver::new(repo_path, "/tmp/repo");
    let scope = PruneScope {
        identity: "/tmp/repo",
        repo_path,
        rows: &stored,
        target: &target,
    };
    let summary = prune_pass(
        &storage,
        &runner,
        &api,
        &api,
        &scope,
        &pinned,
        &source,
        14,
        PruneMode::Report,
        &|_| {},
    );
    let by_path: std::collections::HashMap<&str, &PruneAction> = summary
        .actions
        .iter()
        .map(|action| (action.worktree_path.as_str(), action))
        .collect();
    assert_eq!(summary.scanned, 5);
    assert_eq!(summary.removed, 0, "preview must never prune");
    assert!(summary.dry_run);
    assert_eq!(summary.stale_days, 14);
    assert_eq!(by_path["/tmp/repo/wt-active"].result, "skipped_active");
    assert_eq!(by_path[protected_path.as_str()].result, "skipped_protected");
    assert_eq!(by_path[unknown_path.as_str()].result, "skipped_unknown");
    assert_eq!(by_path["/tmp/repo/does-not-exist"].result, "absent");
    assert_eq!(
        summary.absent, 2,
        "both absent paths are counted separately"
    );
    assert_eq!(summary.kept_active, 1);
    assert_eq!(summary.kept_protected, 1);
    assert_eq!(summary.kept_unknown, 1);
    assert_eq!(summary.eligible, 0, "no existing clean closed candidate");
    // Preview mutated nothing.
    let reloaded = list_all_for_repo(&storage, "/tmp/repo").expect("list");
    for row in &reloaded {
        match row.lease_id.as_str() {
            "lease-active" => assert_eq!(row.status, LEASE_STATUS_ACTIVE),
            "lease-released" => assert_eq!(row.status, LEASE_STATUS_RELEASED),
            "lease-unknown" => assert_eq!(row.status, "quarantined"),
            other => assert_eq!(
                row.status, LEASE_STATUS_RETAINED,
                "preview must never mutate lease status (row {other})"
            ),
        }
    }
}

#[test]
fn prune_summary_envelope_fields_are_populated() {
    let _lock = lock_workflow_tests();
    let (_temp, storage, _env) = open_temp_db("prune-summary");
    ensure_schema(&storage).expect("ensure_schema");
    let now = now_unix_secs();
    let row = fresh_lease_row(
        "lease-only",
        1,
        "s",
        "/tmp/repo/wt-only",
        "phasegent/1-only",
        LEASE_STATUS_ACTIVE,
        now,
    );
    insert_row(&storage, &row);
    let runner = ProcessWorktreeRunner::new();
    let api = FakeOpenCodeApi::empty();
    let source = ScriptedIssueStateSource::closed();
    let stored = list_all_for_repo(&storage, "/tmp/repo").expect("list");
    let pinned = pinned_active_paths(&stored);
    let repo_path = std::path::Path::new("/tmp/repo");
    let target = TargetResolver::new(repo_path, "/tmp/repo");
    let scope = PruneScope {
        identity: "/tmp/repo",
        repo_path,
        rows: &stored,
        target: &target,
    };
    let summary = prune_pass(
        &storage,
        &runner,
        &api,
        &api,
        &scope,
        &pinned,
        &source,
        14,
        PruneMode::Report,
        &|_| {},
    );
    let payload = serde_json::to_value(&summary).expect("serialise");
    for field in [
        "scanned",
        "removed",
        "eligible",
        "absent",
        "kept_active",
        "kept_unknown",
        "kept_protected",
        "kept_issue",
        "failed",
        "dry_run",
        "stale_days",
        "actions",
    ] {
        assert!(payload.get(field).is_some(), "summary missing {field}");
    }
    assert_eq!(payload["dry_run"], serde_json::json!(true));
    assert_eq!(payload["stale_days"], serde_json::json!(14));
    assert_eq!(payload["scanned"], serde_json::json!(1));
    assert_eq!(payload["removed"], serde_json::json!(0));
    assert_eq!(payload["eligible"], serde_json::json!(0));
}

#[test]
fn prune_dry_run_against_real_temp_repo_reports_protected_directory() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-dirty") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-dirty-repo");
    ensure_schema(&storage).expect("ensure_schema");
    let now = now_unix_secs();
    let old = now - 20 * 86_400;
    // A retained lease on the main checkout: the shared guards keep it.
    let dirty = repo.dir.path().join("scratch.txt");
    std::fs::write(&dirty, "scratch\n").expect("write scratch");
    let row = fresh_lease_row(
        "lease-main",
        239,
        "s",
        repo.dir.path().to_str().expect("utf8"),
        &repo.head_branch,
        LEASE_STATUS_RETAINED,
        old,
    );
    insert_row(&storage, &row);
    let runner = ProcessWorktreeRunner::new();
    let api = FakeOpenCodeApi::empty();
    let source = ScriptedIssueStateSource::closed();
    let stored = list_all_for_repo(&storage, &row.repo_identity).expect("list");
    let pinned = pinned_active_paths(&stored);
    let target = TargetResolver::new(repo.dir.path(), &row.repo_identity);
    let scope = PruneScope {
        identity: &row.repo_identity,
        repo_path: repo.dir.path(),
        rows: &stored,
        target: &target,
    };
    let summary = prune_pass(
        &storage,
        &runner,
        &api,
        &api,
        &scope,
        &pinned,
        &source,
        14,
        PruneMode::Report,
        &|_| {},
    );
    assert_eq!(summary.scanned, 1);
    assert_eq!(summary.removed, 0);
    let action = &summary.actions[0];
    assert_eq!(action.worktree_path, repo.dir.path().to_string_lossy());
    assert!(
        !action.prunable,
        "a protected directory must not be prunable"
    );
    assert_eq!(action.result, "skipped_protected");
    let _ = std::fs::remove_file(&dirty);
}

// ---------------------------------------------------------------------------
// Issue #246 Phase 2: dirty-tree triggers through the CLI-facing contract
// ---------------------------------------------------------------------------
//
// The CLI acquire executor (`execute_acquire`) is cwd- and stdout-bound
// (`current_dir` + a printed JSON envelope), so the unit tests here drive
// the exact same surface the executor does without spawning a subprocess
// or mutating a real checkout: a temp repo + temp `PHASEGENT_DB_PATH` +
// temp `PHASEGENT_WORKTREE_CACHE_DIR`, calling `acquire_lease` the way
// `execute_acquire` does (cache base `None` -> env-resolved cache), then
// mapping through `AcquireJson` to assert the JSON the CLI emits. The
// warnings the executor forwards to stderr arrive on
// `AcquireOutcome::warnings` (each element is the `message` of the
// `{"warning": {...}}` line `report_local_warnings` prints), so asserting
// the vector asserts the stderr payload.

#[test]
fn scan_worktree_candidates_classifies_without_writing() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("scan-classify") else {
        return;
    };
    let clean = add_real_worktree(&repo, "phasegent/916-clean01");
    let dirty = add_real_worktree(&repo, "phasegent/916-dirty01");
    std::fs::write(dirty.join("scratch.txt"), "scratch\n").expect("write scratch");
    let now = now_unix_secs();
    let old = now - 30 * 86_400;
    let recent = now - 60;
    let rows = vec![
        fresh_lease_row(
            "lease-active",
            7,
            "s",
            "/tmp/phasegent-916-active",
            "phasegent/7-active",
            LEASE_STATUS_ACTIVE,
            old,
        ),
        fresh_lease_row(
            "lease-released",
            7,
            "s",
            "/tmp/phasegent-916-released",
            "phasegent/7-released",
            LEASE_STATUS_RELEASED,
            old,
        ),
        fresh_lease_row(
            "lease-unknown",
            7,
            "s",
            "/tmp/phasegent-916-unknown",
            "phasegent/7-unknown",
            "quarantined",
            old,
        ),
        fresh_lease_row(
            "lease-absent",
            7,
            "s",
            "/tmp/phasegent-916-missing",
            "phasegent/7-missing",
            LEASE_STATUS_RETAINED,
            old,
        ),
        fresh_lease_row(
            "lease-clean",
            7,
            "s",
            &clean.to_string_lossy(),
            "phasegent/7-clean",
            LEASE_STATUS_RETAINED,
            recent,
        ),
        fresh_lease_row(
            "lease-dirty",
            7,
            "s",
            &dirty.to_string_lossy(),
            "phasegent/7-dirty",
            LEASE_STATUS_RETAINED,
            old,
        ),
    ];
    let runner = ProcessWorktreeRunner::new();
    let api = FakeOpenCodeApi::empty();
    let source = ScriptedIssueStateSource::closed();
    let target = TargetResolver::new(repo.dir.path(), "/tmp/repo");
    let scope = PruneScope {
        identity: "/tmp/repo",
        repo_path: repo.dir.path(),
        rows: &rows,
        target: &target,
    };
    let pinned = std::collections::BTreeSet::new();
    let scanned = scan_worktree_candidates(&runner, &api, &scope, &pinned, &source);
    let by_path: std::collections::HashMap<&str, &PruneDisposition> = scanned
        .iter()
        .map(|candidate| (candidate.worktree_path.as_str(), &candidate.disposition))
        .collect();
    assert_eq!(
        by_path["/tmp/phasegent-916-active"],
        &PruneDisposition::SkippedActive
    );
    assert_eq!(
        by_path["/tmp/phasegent-916-released"],
        &PruneDisposition::MissingDirectory,
        "a released row is a candidate; its absent path is counted separately"
    );
    assert_eq!(
        by_path["/tmp/phasegent-916-unknown"],
        &PruneDisposition::SkippedUnknownStatus
    );
    assert_eq!(
        by_path["/tmp/phasegent-916-missing"],
        &PruneDisposition::MissingDirectory
    );
    assert_eq!(
        by_path[clean.to_string_lossy().as_ref()],
        &PruneDisposition::Removable,
        "a recent clean closed directory is removable without waiting"
    );
    assert!(matches!(
        by_path[dirty.to_string_lossy().as_ref()],
        PruneDisposition::SkippedProtected(_)
    ));
    // The scan performed no side effect: both real worktrees survive.
    assert!(clean.exists(), "scan must not delete a removable worktree");
    assert!(dirty.exists());
}

#[test]
fn prune_report_mode_never_removes_a_removable_worktree() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-report-keep") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-report-keep");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let api = FakeOpenCodeApi::empty();
    let source = ScriptedIssueStateSource::closed();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/916-report01");
    insert_lease_for_identity(
        &storage,
        "lease-report",
        &identity,
        "session-A",
        LEASE_STATUS_RETAINED,
        now_unix_secs(),
        &target.to_string_lossy(),
    );
    let summary = pass(&storage, &repo, &identity, &api, &source, PruneMode::Report);
    assert!(summary.dry_run);
    assert_eq!(summary.removed, 0);
    assert_eq!(
        summary.eligible, 1,
        "clean closed row is a preview candidate"
    );
    assert!(target.exists(), "report mode must never delete a worktree");
    let row = list_all_for_repo(&storage, &identity)
        .expect("list")
        .into_iter()
        .find(|row| row.lease_id == "lease-report")
        .expect("row");
    assert_eq!(row.status, LEASE_STATUS_RETAINED);
}

/// P1 Operator shares the orchestrator prune authority (preview stays
/// read-only); a managed session without a role fails closed.
#[test]
fn prune_operator_authorized_and_managed_denied() {
    use crate::policy::CallerContext;

    assert!(CallerContext::Operator.is_orchestrator_authorized());
    assert!(!CallerContext::ManagedWithoutRole.is_orchestrator_authorized());

    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-operator") else {
        return;
    };
    let (_temp, _storage, _env) = open_temp_db("prune-operator");
    let preview = || crate::command::WorktreeCommand::Prune {
        repo: Some(repo.dir.path().to_string_lossy().to_string()),
        stale_days: 14,
        release_stale: false,
        remove: false,
        reason: None,
        dry_run: true,
        format: "json".to_owned(),
        no_sync: true,
    };
    // The role gate fires before any issue-state read, so the production
    // source is never constructed here and no credential is resolved.
    let exit =
        crate::cli::worktree::execute_worktree_with_caller(CallerContext::Operator, preview());
    assert_eq!(exit, 0, "operator prune preview must succeed");

    let denied = crate::cli::worktree::execute_worktree_with_caller(
        CallerContext::ManagedWithoutRole,
        preview(),
    );
    assert_eq!(denied, 3, "managed prune without a role must be denied");
}
