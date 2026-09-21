use super::cli_support::*;
use super::support::*;
use super::*;

#[test]
fn prune_active_leases_never_mutate() {
    let _lock = lock_workflow_tests();
    let (_temp, storage, _env) = open_temp_db("prune-active");
    ensure_schema(&storage).expect("ensure_schema");
    let now = now_unix_secs();
    let row = fresh_lease_row(
        "lease-active",
        1,
        "s",
        "/tmp/repo/wt",
        "phasegent/1-active",
        LEASE_STATUS_ACTIVE,
        now - 30 * 86_400,
    );
    insert_row(&storage, &row);
    let runner = ProcessWorktreeRunner::new();
    let summary = prune_pass(
        &storage,
        &runner,
        std::path::Path::new("/tmp/repo"),
        &list_for_repo(&storage, "/tmp/repo").expect("list"),
        14,
        PruneMode::Remove,
    );
    assert_eq!(summary.pruned, 0);
    let row_after = list_for_repo(&storage, "/tmp/repo")
        .expect("list")
        .into_iter()
        .find(|row| row.lease_id == "lease-active")
        .expect("row");
    assert_eq!(row_after.status, LEASE_STATUS_ACTIVE);
}

// ---------------------------------------------------------------------------
// Prune integration: real temp repo + is_clean + worktree_remove
// ---------------------------------------------------------------------------

#[test]
fn cli_prune_dry_run_does_not_mutate() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cli-stale-dry") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("cli-stale-dry");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let old = now_unix_secs() - 30 * 86_400;
    insert_lease_for_identity(
        &storage,
        "lease-stale",
        &identity,
        "session-A",
        LEASE_STATUS_ACTIVE,
        old,
        "/tmp/wt-stale",
    );
    let exit = execute_worktree(
        Some(Role::Orchestrator),
        prune_command(&repo, false, false, None),
    );
    assert_eq!(exit, 0, "default prune dry-run must succeed");
    let row = list_for_repo(&storage, &identity)
        .expect("list")
        .into_iter()
        .find(|row| row.lease_id == "lease-stale")
        .expect("row");
    assert_eq!(row.status, LEASE_STATUS_ACTIVE, "dry-run must not mutate");
    assert!(row.release_reason.is_none());
}

#[test]
fn cli_prune_release_stale_flips_candidates_and_keeps_worktree() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cli-stale-apply") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("cli-stale-apply");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let old = now_unix_secs() - 30 * 86_400;
    let worktree = TempDir::new("cli-stale-apply-wt");
    let worktree_path = worktree.path().to_string_lossy().to_string();
    insert_lease_for_identity(
        &storage,
        "lease-flip",
        &identity,
        "session-A",
        LEASE_STATUS_ACTIVE,
        old,
        &worktree_path,
    );
    let exit = execute_worktree(
        Some(Role::Orchestrator),
        prune_command(&repo, true, false, Some("stale session recovery")),
    );
    assert_eq!(exit, 0, "prune --release-stale must succeed");
    let row = list_for_repo(&storage, &identity)
        .expect("list")
        .into_iter()
        .find(|row| row.lease_id == "lease-flip")
        .expect("row");
    assert_eq!(row.status, LEASE_STATUS_RETAINED);
    assert_eq!(
        row.release_reason.as_deref(),
        Some("stale session recovery")
    );
    assert!(
        worktree.path().exists(),
        "stale recovery without --remove must never delete the worktree directory"
    );
}

#[test]
fn release_stale_summary_envelope_fields_are_populated() {
    let summary = ReleaseStaleSummary {
        repo_identity: "/tmp/repo".to_owned(),
        stale_days: 14,
        stale_before: 100,
        apply: false,
        candidates: 1,
        released: 0,
        reason: None,
        leases: vec![ReleaseStaleAction {
            lease_id: "lease-1".to_owned(),
            issue: 7,
            session: "session-A".to_owned(),
            worktree_path: "/tmp/wt".to_owned(),
            branch: "phasegent/7-aaaaaa".to_owned(),
            heartbeat_at: 0,
            age_secs: 100,
            status: "active".to_owned(),
        }],
    };
    let payload = serde_json::to_value(&summary).expect("serialise");
    for field in [
        "repo_identity",
        "stale_days",
        "stale_before",
        "apply",
        "candidates",
        "released",
        "leases",
    ] {
        assert!(payload.get(field).is_some(), "summary missing {field}");
    }
    assert!(
        payload.get("reason").is_none(),
        "dry-run must omit the reason field"
    );
}

#[test]
fn prune_combined_summary_records_lease_and_directory_actions_separately() {
    let summary = PruneCombinedSummary {
        repo_identity: "/tmp/repo".to_owned(),
        stale_days: 14,
        release_stale: true,
        remove: false,
        reason: Some("recovery".to_owned()),
        leases: ReleaseStaleSummary {
            repo_identity: "/tmp/repo".to_owned(),
            stale_days: 14,
            stale_before: 100,
            apply: true,
            candidates: 1,
            released: 1,
            reason: Some("recovery".to_owned()),
            leases: vec![ReleaseStaleAction {
                lease_id: "lease-1".to_owned(),
                issue: 7,
                session: "session-A".to_owned(),
                worktree_path: "/tmp/wt".to_owned(),
                branch: "phasegent/7-aaaaaa".to_owned(),
                heartbeat_at: 0,
                age_secs: 100,
                status: "retained".to_owned(),
            }],
        },
        worktrees: PruneSummary {
            scanned: 1,
            candidates: 0,
            pruned: 0,
            skipped_dirty: 0,
            skipped_active_or_released: 0,
            skipped_recent: 0,
            dry_run: true,
            stale_days: 14,
            actions: vec![],
        },
    };
    let payload = serde_json::to_value(&summary).expect("serialise");
    for field in [
        "repo_identity",
        "stale_days",
        "release_stale",
        "remove",
        "leases",
        "worktrees",
    ] {
        assert!(payload.get(field).is_some(), "summary missing {field}");
    }
    assert_eq!(payload["release_stale"], serde_json::json!(true));
    assert_eq!(payload["remove"], serde_json::json!(false));
    assert_eq!(payload["leases"]["apply"], serde_json::json!(true));
    assert_eq!(payload["worktrees"]["dry_run"], serde_json::json!(true));
}

// ---------------------------------------------------------------------------
// Issue #337 Phase 2: prune candidate-scan / action-apply boundary
// ---------------------------------------------------------------------------
//
// The scan is pure classification: it receives the rows and returns a
// disposition per row without touching storage or deleting anything.
// Only `PruneMode::Remove` may mutate. These tests pin both halves of
// that boundary against real temp worktrees so a future change cannot
// silently re-entangle classification with side effects.

#[test]
fn cli_prune_remove_deletes_clean_retained_worktree_and_keeps_branch() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cli-prune-remove") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("cli-prune-remove");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let branch = "phasegent/7-remove01";
    let target = add_real_worktree(&repo, branch);
    insert_lease_for_identity(
        &storage,
        "lease-remove",
        &identity,
        "session-A",
        LEASE_STATUS_RETAINED,
        now_unix_secs() - 30 * 86_400,
        &target.to_string_lossy(),
    );
    let exit = execute_worktree(
        Some(Role::Orchestrator),
        prune_command(&repo, false, true, None),
    );
    assert_eq!(exit, 0, "prune --remove must succeed");
    assert!(!target.exists(), "--remove must delete the clean worktree");
    assert!(
        branch_exists(&repo, branch),
        "prune must never delete the branch"
    );
    let row = list_for_repo(&storage, &identity)
        .expect("list")
        .into_iter()
        .find(|row| row.lease_id == "lease-remove")
        .expect("row");
    assert_eq!(row.status, LEASE_STATUS_RELEASED);
}

#[test]
fn cli_prune_remove_skips_dirty_worktree() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cli-prune-dirty") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("cli-prune-dirty");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/7-dirtyskip");
    std::fs::write(target.join("scratch.txt"), "scratch\n").expect("write scratch");
    insert_lease_for_identity(
        &storage,
        "lease-dirty",
        &identity,
        "session-A",
        LEASE_STATUS_RETAINED,
        now_unix_secs() - 30 * 86_400,
        &target.to_string_lossy(),
    );
    let exit = execute_worktree(
        Some(Role::Orchestrator),
        prune_command(&repo, false, true, None),
    );
    assert_eq!(exit, 0, "a dirty candidate is reported, not fatal");
    assert!(target.exists(), "--remove must keep a dirty worktree");
    let row = list_for_repo(&storage, &identity)
        .expect("list")
        .into_iter()
        .find(|row| row.lease_id == "lease-dirty")
        .expect("row");
    assert_eq!(row.status, LEASE_STATUS_RETAINED);
}

#[test]
fn cli_prune_release_stale_and_remove_act_on_separate_candidate_sets() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cli-prune-combined") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("cli-prune-combined");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let active_target = add_real_worktree(&repo, "phasegent/7-active1");
    let retained_target = add_real_worktree(&repo, "phasegent/7-retained1");
    let old = now_unix_secs() - 30 * 86_400;
    insert_lease_for_identity(
        &storage,
        "lease-active",
        &identity,
        "session-A",
        LEASE_STATUS_ACTIVE,
        old,
        &active_target.to_string_lossy(),
    );
    insert_lease_for_identity(
        &storage,
        "lease-retained",
        &identity,
        "session-B",
        LEASE_STATUS_RETAINED,
        old,
        &retained_target.to_string_lossy(),
    );
    let exit = execute_worktree(
        Some(Role::Orchestrator),
        prune_command(&repo, true, true, Some("stale session recovery")),
    );
    assert_eq!(exit, 0, "combined prune must succeed");
    let rows = list_for_repo(&storage, &identity).expect("list");
    let active = rows
        .iter()
        .find(|row| row.lease_id == "lease-active")
        .expect("active row");
    // The recovery flips the active lease to retained and refreshes its
    // heartbeat, so the directory pass sees it as recent and keeps the
    // worktree; the removal set is the pre-existing retained candidate.
    assert_eq!(active.status, LEASE_STATUS_RETAINED);
    assert_eq!(
        active.release_reason.as_deref(),
        Some("stale session recovery")
    );
    assert!(
        active_target.exists(),
        "a just-recovered lease must not be removed in the same pass"
    );
    let retained = rows
        .iter()
        .find(|row| row.lease_id == "lease-retained")
        .expect("retained row");
    assert_eq!(retained.status, LEASE_STATUS_RELEASED);
    assert!(
        !retained_target.exists(),
        "the retained + aged + clean candidate is removed"
    );
}

// ---------------------------------------------------------------------------
// Issue 18: shared auto-acquire hook after `issue create` / `issue bind`
// ---------------------------------------------------------------------------//
// `auto_acquire_after_bind` reuses `resolve_session` + `acquire_lease`, so the
// focused tests drive it against a temp repo + temp DB + temp cache exactly
// like the CLI executor does. They assert the three contracts the parent
// prompt fixes: stdout JSON is never involved (the helper only returns the
// stderr warning string), a created worktree surfaces the bounded
// `reason=new_worktree` warning, and no branch/worktree is ever deleted.
