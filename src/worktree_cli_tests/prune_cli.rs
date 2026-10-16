use super::cli_support::*;
use super::support::*;
use super::*;

use crate::worktree::opencode::test_support::FakeOpenCodeApi;
use crate::worktree::opencode::{ApiError, ListRequest, LocatedSession, OpenCodeApi, SessionPage};

/// A host that blocks on its first session lookup until the test releases it,
/// so a test can prove a progress notice was streamed before the check.
struct BlockingHost {
    inner: FakeOpenCodeApi,
    blocked: std::sync::mpsc::Sender<()>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    blocked_once: std::sync::atomic::AtomicBool,
}

impl OpenCodeApi for BlockingHost {
    fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
        if !self
            .blocked_once
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            let _ = self.blocked.send(());
            let _ = self.release.lock().expect("release lock").recv();
        }
        self.inner.session(id)
    }

    fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
        self.inner.list(request)
    }

    fn move_session(&self, id: &str, directory: &std::path::Path) -> Result<(), ApiError> {
        self.inner.move_session(id, directory)
    }
}

/// The prune streams a flushed progress notice before it blocks on the host,
/// so an operator watching a slow invocation sees work start; the final result
/// is still produced once the host answers. The host is an isolated fake, so no
/// real user worktree or provider is touched.
#[test]
fn cli_prune_streams_progress_before_a_blocked_host_check() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cli-prune-progress") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("cli-prune-progress");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/917-progress1");
    insert_lease_for_identity(
        &storage,
        "lease-progress",
        &identity,
        "ses_a",
        LEASE_STATUS_RETAINED,
        now_unix_secs(),
        &target.to_string_lossy(),
    );

    let (blocked_tx, blocked_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let host = BlockingHost {
        inner: FakeOpenCodeApi::empty().with_session("ses_a", repo.dir.path()),
        blocked: blocked_tx,
        release: std::sync::Mutex::new(release_rx),
        blocked_once: std::sync::atomic::AtomicBool::new(false),
    };
    let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let worker_lines = lines.clone();
    let repo_path = repo.dir.path().to_string_lossy().into_owned();

    let handle = std::thread::spawn(move || {
        let source = ScriptedIssueStateSource::closed();
        execute_prune_with_source(
            Some(&repo_path),
            999,
            false,
            None,
            false,
            "json",
            &source,
            &host,
            &move |message: &str| {
                worker_lines
                    .lock()
                    .expect("progress lock")
                    .push(message.to_owned());
            },
        )
    });

    blocked_rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("prune must reach the host check");
    let seen = lines.lock().expect("progress lock").clone();
    assert!(
        seen.iter().any(|line| line.contains("scanning")),
        "a scan notice must be streamed before the host check blocks, got {seen:?}"
    );
    release_tx.send(()).expect("release the host");
    let exit = handle.join().expect("prune thread");
    assert_eq!(exit, 0, "a released prune completes");
    assert!(
        lines
            .lock()
            .expect("progress lock")
            .iter()
            .any(|line| line.contains("verifying")),
        "the removal recheck also streams a notice"
    );
}

/// A `prune_pass` scope bound to the temp database's rows.
fn scope_for<'a>(rows: &'a [LeaseRow], repo_path: &'a std::path::Path) -> PruneScope<'a> {
    PruneScope {
        identity: "/tmp/repo",
        repo_path,
        rows,
    }
}

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
    let api = FakeOpenCodeApi::empty();
    let source = ScriptedIssueStateSource::closed();
    let rows = list_for_repo(&storage, "/tmp/repo").expect("list");
    let pinned = pinned_active_paths(&rows);
    let scope = scope_for(&rows, std::path::Path::new("/tmp/repo"));
    let summary = prune_pass(
        &storage,
        &runner,
        &api,
        &api,
        &scope,
        &pinned,
        &source,
        14,
        PruneMode::Remove,
        &|_| {},
    );
    assert_eq!(summary.removed, 0);
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
    let source = ScriptedIssueStateSource::closed();
    let exit = run_scoped_prune(&repo, 14, false, None, true, "json", &source);
    assert_eq!(exit, 0, "default prune preview must succeed");
    let row = list_for_repo(&storage, &identity)
        .expect("list")
        .into_iter()
        .find(|row| row.lease_id == "lease-stale")
        .expect("row");
    assert_eq!(row.status, LEASE_STATUS_ACTIVE, "preview must not mutate");
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
    let source = ScriptedIssueStateSource::closed();
    let exit = run_scoped_prune(
        &repo,
        14,
        true,
        Some("stale session recovery"),
        false,
        "json",
        &source,
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
        "a directory active at invocation start is never removed by the same recovery call"
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
        "preview must omit the reason field"
    );
}

fn empty_worktree_summary() -> PruneSummary {
    PruneSummary {
        scanned: 0,
        removed: 0,
        eligible: 0,
        absent: 0,
        kept_active: 0,
        kept_unknown: 0,
        kept_protected: 0,
        kept_issue: 0,
        failed: 0,
        dry_run: false,
        stale_days: 14,
        actions: vec![],
    }
}

#[test]
fn prune_aggregate_records_lease_and_directory_actions_separately() {
    let mut worktrees = empty_worktree_summary();
    worktrees.scanned = 1;
    worktrees.removed = 1;
    let summary = PruneAggregate {
        dry_run: false,
        remove: true,
        release_stale: true,
        stale_days: 14,
        repos: vec![PruneRepoOutcome {
            repo_identity: "/tmp/repo".to_owned(),
            repo_path: Some("/tmp/repo".to_owned()),
            skipped_reason: None,
            leases: Some(ReleaseStaleSummary {
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
            }),
            worktrees: Some(worktrees),
        }],
        totals: PruneTotals {
            repositories: 1,
            removed: 1,
            eligible: 0,
            absent: 0,
            kept_active: 0,
            kept_unknown: 0,
            kept_protected: 0,
            kept_issue: 0,
            failed: 0,
        },
    };
    let payload = serde_json::to_value(&summary).expect("serialise");
    for field in [
        "dry_run",
        "remove",
        "release_stale",
        "stale_days",
        "repos",
        "totals",
    ] {
        assert!(payload.get(field).is_some(), "summary missing {field}");
    }
    assert_eq!(payload["remove"], serde_json::json!(true));
    assert_eq!(
        payload["repos"][0]["leases"]["apply"],
        serde_json::json!(true)
    );
    assert_eq!(
        payload["repos"][0]["worktrees"]["removed"],
        serde_json::json!(1)
    );
    assert_eq!(payload["totals"]["repositories"], serde_json::json!(1));
    assert!(
        payload["repos"][0].get("skipped_reason").is_none(),
        "resolved repos omit the skip reason"
    );
}

#[test]
fn prune_aggregate_text_is_concise_and_json_is_equivalent() {
    let aggregate = PruneAggregate {
        dry_run: false,
        remove: true,
        release_stale: false,
        stale_days: 7,
        repos: vec![
            PruneRepoOutcome {
                repo_identity: "/tmp/repo-a/.git".to_owned(),
                repo_path: Some("/tmp/repo-a".to_owned()),
                skipped_reason: None,
                leases: None,
                worktrees: Some(PruneSummary {
                    scanned: 3,
                    removed: 1,
                    eligible: 1,
                    absent: 1,
                    kept_active: 0,
                    kept_unknown: 0,
                    kept_protected: 1,
                    kept_issue: 0,
                    failed: 0,
                    dry_run: false,
                    stale_days: 7,
                    actions: vec![
                        PruneAction {
                            worktree_path: "/tmp/repo-a/wt".to_owned(),
                            branch: "phasegent/916-a".to_owned(),
                            status: LEASE_STATUS_RETAINED.to_owned(),
                            heartbeat_at: 0,
                            lease_ids: vec!["lease-a".to_owned()],
                            issues: vec![916],
                            prunable: true,
                            result: "pruned".to_owned(),
                            message: Some("git worktree remove succeeded".to_owned()),
                            relocated_children: vec!["ses_child1".to_owned()],
                            planned_children: Vec::new(),
                        },
                        // A child moved and confirmed, but a later occupancy
                        // check kept the directory: the confirmed move stays
                        // reported as an actual relocation, next to the keep.
                        PruneAction {
                            worktree_path: "/tmp/repo-a/wt-kept".to_owned(),
                            branch: "phasegent/918-kept".to_owned(),
                            status: LEASE_STATUS_RETAINED.to_owned(),
                            heartbeat_at: 0,
                            lease_ids: vec!["lease-kept".to_owned()],
                            issues: vec![918],
                            prunable: false,
                            result: "skipped_protected".to_owned(),
                            message: Some("kept on re-check: an incoming occupant".to_owned()),
                            relocated_children: vec!["ses_child2".to_owned()],
                            planned_children: Vec::new(),
                        },
                        // Preview proposal only: no actual relocation.
                        PruneAction {
                            worktree_path: "/tmp/repo-a/wt2".to_owned(),
                            branch: "phasegent/917-b".to_owned(),
                            status: LEASE_STATUS_RETAINED.to_owned(),
                            heartbeat_at: 0,
                            lease_ids: vec!["lease-b".to_owned()],
                            issues: vec![917],
                            prunable: true,
                            result: "prunable".to_owned(),
                            message: Some("dry-run: would call git worktree remove".to_owned()),
                            relocated_children: Vec::new(),
                            planned_children: vec!["ses_child3".to_owned()],
                        },
                    ],
                }),
            },
            PruneRepoOutcome {
                repo_identity: "/tmp/gone/.git".to_owned(),
                repo_path: None,
                skipped_reason: Some("checkout directory is missing: /tmp/gone".to_owned()),
                leases: None,
                worktrees: None,
            },
        ],
        totals: PruneTotals {
            repositories: 1,
            removed: 1,
            eligible: 1,
            absent: 0,
            kept_active: 0,
            kept_unknown: 0,
            kept_protected: 1,
            kept_issue: 0,
            failed: 0,
        },
    };
    let text = render_text(&aggregate);
    assert!(text.contains("worktree prune (removal"), "got: {text}");
    assert!(text.contains("/tmp/repo-a"), "got: {text}");
    assert!(
        text.contains("removed 1, eligible 1, absent 0, kept 1, removal failures 0"),
        "got: {text}"
    );
    assert!(text.contains("pruned /tmp/repo-a/wt"), "got: {text}");
    // A confirmed move is factual and prints as `relocated`, even on the kept
    // directory whose final result is `skipped_protected`.
    assert!(
        text.contains("relocated finished child session(s): ses_child1"),
        "got: {text}"
    );
    assert!(
        text.contains("kept on re-check: an incoming occupant"),
        "got: {text}"
    );
    assert!(
        text.contains("relocated finished child session(s): ses_child2"),
        "got: {text}"
    );
    // A preview proposal is labelled distinctly, never as an actual move.
    assert!(
        text.contains("would relocate finished child session(s): ses_child3"),
        "got: {text}"
    );
    assert!(text.contains("skipped /tmp/gone/.git"), "got: {text}");
    assert!(
        text.contains(
            "summary: 1 repositories, removed 1, eligible 1, absent 0, kept 1, removal failures 0"
        ),
        "got: {text}"
    );
    assert!(
        text.contains("removal failures count only failed `git worktree remove` calls"),
        "got: {text}"
    );
    // The text default must not dump the lease JSON payload.
    assert!(!text.contains("stale_before"), "got: {text}");

    let payload = serde_json::to_value(&aggregate).expect("serialise");
    assert_eq!(payload["totals"]["removed"], serde_json::json!(1));
    assert_eq!(
        payload["repos"][0]["worktrees"]["removed"],
        serde_json::json!(1)
    );
    // The actual-only key and the additive proposal key are distinct.
    assert_eq!(
        payload["repos"][0]["worktrees"]["actions"][0]["relocated_children"],
        serde_json::json!(["ses_child1"])
    );
    assert!(
        payload["repos"][0]["worktrees"]["actions"][0]
            .get("planned_children")
            .is_none(),
        "an empty planned_children list is omitted"
    );
    assert_eq!(
        payload["repos"][0]["worktrees"]["actions"][2]["planned_children"],
        serde_json::json!(["ses_child3"])
    );
    assert!(
        payload["repos"][0]["worktrees"]["actions"][2]
            .get("relocated_children")
            .is_none(),
        "a preview proposal never sets the actual-only key"
    );
    assert!(payload["repos"][1]["skipped_reason"].is_string());
}

#[test]
fn prune_aggregate_empty_state_is_actionable() {
    let aggregate = PruneAggregate {
        dry_run: true,
        remove: false,
        release_stale: false,
        stale_days: 7,
        repos: vec![],
        totals: PruneTotals {
            repositories: 0,
            removed: 0,
            eligible: 0,
            absent: 0,
            kept_active: 0,
            kept_unknown: 0,
            kept_protected: 0,
            kept_issue: 0,
            failed: 0,
        },
    };
    let text = render_text(&aggregate);
    assert!(
        text.contains("no repositories with recorded worktree leases"),
        "got: {text}"
    );
    assert!(text.contains("dry-run preview"), "got: {text}");
    assert!(
        text.contains(
            "summary: 0 repositories, removed 0, eligible 0, absent 0, kept 0, removal failures 0"
        ),
        "got: {text}"
    );
}

#[test]
fn cli_prune_preview_with_release_stale_never_mutates() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cli-preview-release") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("cli-preview-release");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let old = now_unix_secs() - 30 * 86_400;
    let worktree = TempDir::new("cli-preview-release-wt");
    let worktree_path = worktree.path().to_string_lossy().to_string();
    insert_lease_for_identity(
        &storage,
        "lease-preview-release",
        &identity,
        "session-A",
        LEASE_STATUS_ACTIVE,
        old,
        &worktree_path,
    );
    // A preview that also requests a recovery, with a reason and under the
    // `--dry-run` alias, must still flip and delete nothing.
    let source = ScriptedIssueStateSource::closed();
    let exit = run_scoped_prune(&repo, 14, true, Some("preview only"), true, "json", &source);
    assert_eq!(exit, 0, "preview must succeed");
    // Both spellings parse to the same preview flag.
    for flag in ["--dryrun", "--dry-run"] {
        let invocation = crate::command::parse_with_role_env(
            &strings([
                "worktree",
                "prune",
                "--release-stale",
                "--reason",
                "preview only",
                flag,
            ]),
            Some("orchestrator"),
        )
        .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::Prune { dry_run, .. }) => {
                assert!(dry_run, "{flag} must select the preview");
            }
            other => panic!("unexpected command {other:?}"),
        }
    }
    let row = list_for_repo(&storage, &identity)
        .expect("list")
        .into_iter()
        .find(|row| row.lease_id == "lease-preview-release")
        .expect("row");
    assert_eq!(
        row.status, LEASE_STATUS_ACTIVE,
        "preview must not recover a stale lease"
    );
    assert!(row.release_reason.is_none());
    assert!(
        worktree.path().exists(),
        "preview must not delete a directory"
    );
}

// ---------------------------------------------------------------------------
// Issue #337 Phase 2 / issue 913 / issue 916: candidate-scan / action-apply
// boundary.
// ---------------------------------------------------------------------------
//
// The scan is pure classification: it receives the rows and returns a
// disposition per directory without touching storage or deleting anything.
// Only `PruneMode::Remove` may mutate, and only after the shared candidate
// guards clear the directory. These tests pin both halves of that boundary
// against real temp worktrees so a future change cannot silently re-entangle
// classification with side effects.

#[test]
fn cli_prune_remove_deletes_clean_closed_worktree_and_keeps_branch() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cli-prune-remove") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("cli-prune-remove");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let branch = "phasegent/916-remove01";
    let target = add_real_worktree(&repo, branch);
    insert_lease_for_identity(
        &storage,
        "lease-remove",
        &identity,
        "session-A",
        LEASE_STATUS_RETAINED,
        now_unix_secs(),
        &target.to_string_lossy(),
    );
    let source = ScriptedIssueStateSource::closed();
    // `--stale-days 999` proves the age threshold no longer gates removal.
    let exit = run_scoped_prune(&repo, 999, false, None, false, "json", &source);
    assert_eq!(exit, 0, "default prune removal must succeed");
    assert!(!target.exists(), "removal must delete the clean worktree");
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
    let target = add_real_worktree(&repo, "phasegent/916-dirtyskip");
    std::fs::write(target.join("scratch.txt"), "scratch\n").expect("write scratch");
    insert_lease_for_identity(
        &storage,
        "lease-dirty",
        &identity,
        "session-A",
        LEASE_STATUS_RETAINED,
        now_unix_secs(),
        &target.to_string_lossy(),
    );
    let source = ScriptedIssueStateSource::closed();
    let exit = run_scoped_prune(&repo, 999, false, None, false, "json", &source);
    assert_eq!(exit, 0, "a dirty candidate is reported, not fatal");
    assert!(target.exists(), "prune must keep a dirty worktree");
    let row = list_for_repo(&storage, &identity)
        .expect("list")
        .into_iter()
        .find(|row| row.lease_id == "lease-dirty")
        .expect("row");
    assert_eq!(row.status, LEASE_STATUS_RETAINED);
}

#[test]
fn cli_prune_released_closed_directory_is_eligible() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cli-prune-released") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("cli-prune-released");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/916-released1");
    insert_lease_for_identity(
        &storage,
        "lease-released",
        &identity,
        "session-A",
        LEASE_STATUS_RELEASED,
        now_unix_secs(),
        &target.to_string_lossy(),
    );
    let source = ScriptedIssueStateSource::closed();
    let exit = run_scoped_prune(&repo, 999, false, None, false, "json", &source);
    assert_eq!(exit, 0);
    assert!(
        !target.exists(),
        "a released but closed clean directory is also eligible"
    );
    let row = list_for_repo(&storage, &identity)
        .expect("list")
        .into_iter()
        .find(|row| row.lease_id == "lease-released")
        .expect("row");
    assert_eq!(row.status, LEASE_STATUS_RELEASED);
}

#[test]
fn cli_prune_keeps_a_directory_at_invocation_start_active_after_recovery() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cli-prune-combined") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("cli-prune-combined");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let active_target = add_real_worktree(&repo, "phasegent/916-active1");
    let retained_target = add_real_worktree(&repo, "phasegent/916-retained1");
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
    let source = ScriptedIssueStateSource::closed();
    let exit = run_scoped_prune(
        &repo,
        14,
        true,
        Some("stale session recovery"),
        false,
        "json",
        &source,
    );
    assert_eq!(exit, 0, "combined prune must succeed");
    let rows = list_for_repo(&storage, &identity).expect("list");
    let active = rows
        .iter()
        .find(|row| row.lease_id == "lease-active")
        .expect("active row");
    // The recovery flips the active lease to retained, but the directory held
    // an active lease at invocation start, so it cannot become removable in
    // the same call.
    assert_eq!(active.status, LEASE_STATUS_RETAINED);
    assert_eq!(
        active.release_reason.as_deref(),
        Some("stale session recovery")
    );
    assert!(
        active_target.exists(),
        "a directory active at invocation start is never removed by the same recovery call"
    );
    let retained = rows
        .iter()
        .find(|row| row.lease_id == "lease-retained")
        .expect("retained row");
    assert_eq!(retained.status, LEASE_STATUS_RELEASED);
    assert!(
        !retained_target.exists(),
        "the released + closed + clean candidate is removed"
    );
}
