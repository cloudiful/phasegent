use super::cli_support::*;
use super::support::*;
use super::*;

#[test]
fn prune_dry_run_classifies_every_status_correctly() {
    let _lock = lock_workflow_tests();
    let (_temp, storage, _env) = open_temp_db("prune-classify");
    ensure_schema(&storage).expect("ensure_schema");
    let now = now_unix_secs();
    let old = now - 15 * 86_400;
    let recent = now - 60;
    // Create a directory that exists but is not a git worktree so
    // the is_clean probe fails with a structured git error and the
    // action is classified as `skipped_dirty`. This is the cheapest
    // way to exercise the dirty branch without standing up a real
    // git worktree (the real-temp-repo path is covered by
    // `prune_dry_run_against_real_temp_repo_reports_dirty_lease`).
    let dirty_dir = TempDir::new("prune-dirty-stub");
    let dirty_path = dirty_dir.path().to_string_lossy().to_string();
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
            "lease-recent",
            1,
            "s",
            "/tmp/repo/wt-recent",
            "phasegent/1-recent",
            LEASE_STATUS_RETAINED,
            recent,
        ),
        fresh_lease_row(
            "lease-dirty",
            1,
            "s",
            &dirty_path,
            "phasegent/1-dirty",
            LEASE_STATUS_RETAINED,
            old,
        ),
        // A lease whose worktree directory no longer exists is
        // classified as `noop` and is marked released after a
        // non-dry-run pass; dry-run reports it explicitly so the
        // operator sees the audit trail.
        fresh_lease_row(
            "lease-missing",
            1,
            "s",
            "/tmp/repo/does-not-exist",
            "phasegent/1-missing",
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
    let summary = prune_pass(
        &storage,
        &runner,
        std::path::Path::new("/tmp/repo"),
        &list_for_repo(&storage, "/tmp/repo").expect("list"),
        14,
        PruneMode::Report,
    );
    let action_by_id: std::collections::HashMap<&str, &PruneAction> = summary
        .actions
        .iter()
        .map(|action| (action.lease_id.as_str(), action))
        .collect();
    assert_eq!(summary.scanned, 5);
    assert_eq!(summary.pruned, 0, "dry-run must never prune");
    assert!(summary.dry_run);
    assert_eq!(summary.stale_days, 14);
    assert_eq!(action_by_id["lease-active"].result, "skipped_active");
    assert!(!action_by_id["lease-active"].prunable);
    assert_eq!(action_by_id["lease-recent"].result, "skipped_recent");
    assert!(!action_by_id["lease-recent"].prunable);
    assert_eq!(action_by_id["lease-dirty"].result, "skipped_dirty");
    assert!(!action_by_id["lease-dirty"].prunable);
    assert_eq!(action_by_id["lease-missing"].result, "noop");
    assert!(action_by_id["lease-missing"].prunable);
    assert_eq!(action_by_id["lease-released"].result, "skipped_released");
    assert!(!action_by_id["lease-released"].prunable);
    let reloaded = list_for_repo(&storage, "/tmp/repo").expect("list");
    for row in &reloaded {
        if row.lease_id == "lease-active" {
            // The active lease is correctly skipped without
            // mutation; verify it stayed active.
            assert_eq!(row.status, LEASE_STATUS_ACTIVE);
        } else if row.lease_id == "lease-released" {
            // The released lease started as `released`; verify
            // prune_pass did not flip it back to anything else.
            assert_eq!(row.status, LEASE_STATUS_RELEASED);
        } else {
            // Every other row in the test fixture is `retained`
            // and prune_pass must never flip the status to
            // `released` while `dry_run` is true.
            assert_eq!(
                row.status, LEASE_STATUS_RETAINED,
                "dry-run must never mutate lease status (row {})",
                row.lease_id
            );
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
    let summary = prune_pass(
        &storage,
        &runner,
        std::path::Path::new("/tmp/repo"),
        &list_for_repo(&storage, "/tmp/repo").expect("list"),
        14,
        PruneMode::Report,
    );
    let payload = serde_json::to_value(&summary).expect("serialise");
    for field in [
        "scanned",
        "candidates",
        "pruned",
        "skipped_dirty",
        "skipped_active_or_released",
        "skipped_recent",
        "dry_run",
        "stale_days",
        "actions",
    ] {
        assert!(payload.get(field).is_some(), "summary missing {field}");
    }
    assert_eq!(payload["dry_run"], serde_json::json!(true));
    assert_eq!(payload["stale_days"], serde_json::json!(14));
    assert_eq!(payload["scanned"], serde_json::json!(1));
    assert_eq!(payload["candidates"], serde_json::json!(0));
    assert_eq!(payload["pruned"], serde_json::json!(0));
}

#[test]
fn prune_dry_run_against_real_temp_repo_reports_dirty_lease() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-dirty") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-dirty-repo");
    ensure_schema(&storage).expect("ensure_schema");
    let now = now_unix_secs();
    let old = now - 20 * 86_400;
    // Dirty retained lease: directory is the repo root, which has
    // an uncommitted change so `git status --porcelain` is not
    // empty.
    let dirty = repo.dir.path().join("scratch.txt");
    std::fs::write(&dirty, "scratch\n").expect("write scratch");
    let row = fresh_lease_row(
        "lease-real-dirty",
        239,
        "s",
        repo.dir.path().to_str().expect("utf8"),
        &repo.head_branch,
        LEASE_STATUS_RETAINED,
        old,
    );
    insert_row(&storage, &row);
    let runner = ProcessWorktreeRunner::new();
    let summary = prune_pass(
        &storage,
        &runner,
        repo.dir.path(),
        &list_for_repo(&storage, &row.repo_identity).expect("list"),
        14,
        PruneMode::Report,
    );
    assert_eq!(summary.scanned, 1);
    assert_eq!(summary.pruned, 0);
    let action = &summary.actions[0];
    assert_eq!(action.lease_id, "lease-real-dirty");
    assert!(!action.prunable, "dirty lease must not be prunable");
    assert_eq!(action.result, "skipped_dirty");
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
    let clean = add_real_worktree(&repo, "phasegent/7-clean01");
    let dirty = add_real_worktree(&repo, "phasegent/7-dirty01");
    std::fs::write(dirty.join("scratch.txt"), "scratch\n").expect("write scratch");
    let now = now_unix_secs();
    let old = now - 30 * 86_400;
    let recent = now - 60;
    let rows = vec![
        fresh_lease_row(
            "lease-active",
            7,
            "s",
            &clean.to_string_lossy(),
            "phasegent/7-active",
            LEASE_STATUS_ACTIVE,
            old,
        ),
        fresh_lease_row(
            "lease-released",
            7,
            "s",
            &clean.to_string_lossy(),
            "phasegent/7-released",
            LEASE_STATUS_RELEASED,
            old,
        ),
        fresh_lease_row(
            "lease-unknown",
            7,
            "s",
            &clean.to_string_lossy(),
            "phasegent/7-unknown",
            "quarantined",
            old,
        ),
        fresh_lease_row(
            "lease-recent",
            7,
            "s",
            &clean.to_string_lossy(),
            "phasegent/7-recent",
            LEASE_STATUS_RETAINED,
            recent,
        ),
        fresh_lease_row(
            "lease-missing",
            7,
            "s",
            "/tmp/phasegent-337-missing",
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
            old,
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
    let scanned = scan_worktree_candidates(&runner, &rows, now, i64::from(14u32) * 86_400);
    let by_id: std::collections::HashMap<&str, &PruneDisposition> = scanned
        .iter()
        .map(|candidate| (candidate.lease_id.as_str(), &candidate.disposition))
        .collect();
    assert_eq!(by_id["lease-active"], &PruneDisposition::SkippedActive);
    assert_eq!(by_id["lease-released"], &PruneDisposition::SkippedReleased);
    assert_eq!(
        by_id["lease-unknown"],
        &PruneDisposition::SkippedUnknownStatus
    );
    assert!(matches!(
        by_id["lease-recent"],
        PruneDisposition::SkippedRecent { .. }
    ));
    assert_eq!(by_id["lease-missing"], &PruneDisposition::MissingDirectory);
    assert_eq!(by_id["lease-clean"], &PruneDisposition::Removable);
    assert_eq!(
        by_id["lease-dirty"],
        &PruneDisposition::SkippedDirty("worktree has uncommitted changes".to_owned())
    );
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
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/7-report01");
    insert_lease_for_identity(
        &storage,
        "lease-report",
        &identity,
        "session-A",
        LEASE_STATUS_RETAINED,
        now_unix_secs() - 30 * 86_400,
        &target.to_string_lossy(),
    );
    let summary = prune_pass(
        &storage,
        &runner,
        repo.dir.path(),
        &list_for_repo(&storage, &identity).expect("list"),
        14,
        PruneMode::Report,
    );
    assert!(summary.dry_run);
    assert_eq!(summary.pruned, 0);
    assert_eq!(summary.candidates, 1, "clean retained row is a candidate");
    assert!(target.exists(), "report mode must never delete a worktree");
    let row = list_for_repo(&storage, &identity)
        .expect("list")
        .into_iter()
        .find(|row| row.lease_id == "lease-report")
        .expect("row");
    assert_eq!(row.status, LEASE_STATUS_RETAINED);
}
