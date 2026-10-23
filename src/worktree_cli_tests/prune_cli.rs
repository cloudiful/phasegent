use super::cli_support::*;
use super::support::*;
use super::*;

use crate::lifecycle::cleanup::TargetResolver;
use crate::worktree::opencode::test_support::FakeOpenCodeApi;
use crate::worktree::opencode::{ApiError, ListRequest, LocatedSession, OpenCodeApi, SessionPage};

/// A host that blocks on its first running-session query until the test
/// releases it, so a test can prove a progress notice was streamed before the
/// check.
struct BlockingHost {
    inner: FakeOpenCodeApi,
    blocked: std::sync::mpsc::Sender<()>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    blocked_once: std::sync::atomic::AtomicBool,
}

impl OpenCodeApi for BlockingHost {
    fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
        self.inner.session(id)
    }

    fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
        self.inner.list(request)
    }

    fn move_session(&self, id: &str, directory: &std::path::Path) -> Result<(), ApiError> {
        self.inner.move_session(id, directory)
    }

    fn active_sessions(&self) -> Result<Vec<String>, ApiError> {
        if !self
            .blocked_once
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            let _ = self.blocked.send(());
            let _ = self.release.lock().expect("release lock").recv();
        }
        self.inner.active_sessions()
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
fn scope_for<'a>(
    rows: &'a [LeaseRow],
    repo_path: &'a std::path::Path,
    target: &'a TargetResolver<'a>,
) -> PruneScope<'a> {
    PruneScope {
        identity: "/tmp/repo",
        repo_path,
        rows,
        target,
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
    let repo_path = std::path::Path::new("/tmp/repo");
    let target = TargetResolver::new(repo_path, "/tmp/repo");
    let scope = scope_for(&rows, repo_path, &target);
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
    let exit = run_scoped_prune(
        &repo,
        14,
        false,
        None,
        true,
        "json",
        &source,
        &FakeOpenCodeApi::empty(),
    );
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
        &FakeOpenCodeApi::empty(),
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
                            relocated_children: Vec::new(),
                            planned_children: Vec::new(),
                        },
                        // A directory kept on a fresh re-check.
                        PruneAction {
                            worktree_path: "/tmp/repo-a/wt-kept".to_owned(),
                            branch: "phasegent/918-kept".to_owned(),
                            status: LEASE_STATUS_RETAINED.to_owned(),
                            heartbeat_at: 0,
                            lease_ids: vec!["lease-kept".to_owned()],
                            issues: vec![918],
                            prunable: false,
                            result: "skipped_protected".to_owned(),
                            message: Some(
                                "kept on re-check: a running session is at the directory"
                                    .to_owned(),
                            ),
                            relocated_children: Vec::new(),
                            planned_children: Vec::new(),
                        },
                        // A preview proposal.
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
                            planned_children: Vec::new(),
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
    let text = render_text(&aggregate, std::time::Duration::from_millis(1234));
    assert!(text.starts_with("worktree prune (removal)"), "got: {text}");
    // Actual actions print with their paths and explanations.
    assert!(text.contains("removed /tmp/repo-a/wt"), "got: {text}");
    assert!(text.contains("prunable /tmp/repo-a/wt2"), "got: {text}");
    // A kept directory is listed with the reason it was kept, so a reader
    // knows what still blocks the removal.
    assert!(
        text.contains("kept /tmp/repo-a/wt-kept:"),
        "kept rows carry their reason: {text}"
    );
    // No per-repository headings or counts in the compact default.
    assert!(
        !text.contains("repo /tmp/repo-a"),
        "no per-repo heading: {text}"
    );
    assert!(
        text.contains("removed 1, eligible 1, absent 0, kept 1, removal failures 0"),
        "got: {text}"
    );
    assert!(
        text.contains("kept reasons: active 0, protected 1, issue-state 0, unknown 0"),
        "got: {text}"
    );
    assert!(text.contains("skipped repositories: 1"), "got: {text}");
    assert!(text.contains("elapsed 1.234s"), "got: {text}");
    // The default stays bounded: one line per action plus the summary lines.
    assert!(
        text.lines().count() <= 10,
        "bounded output, got {} lines: {text}",
        text.lines().count()
    );
    // Prune never reports a relocation in text.
    assert!(!text.contains("relocate"), "got: {text}");
    // The text default must not dump the lease JSON payload or skip reasons.
    assert!(!text.contains("stale_before"), "got: {text}");
    assert!(
        !text.contains("checkout directory is missing"),
        "skip reasons stay in --json: {text}"
    );

    // JSON keeps the complete detail unchanged.
    let payload = serde_json::to_value(&aggregate).expect("serialise");
    assert_eq!(payload["totals"]["removed"], serde_json::json!(1));
    assert_eq!(
        payload["repos"][0]["worktrees"]["removed"],
        serde_json::json!(1)
    );
    assert!(payload.get("elapsed").is_none(), "no elapsed JSON field");
    // Prune no longer relocates a session, so both compatibility fields are
    // always empty and omitted.
    for index in 0..3 {
        assert!(
            payload["repos"][0]["worktrees"]["actions"][index]
                .get("relocated_children")
                .is_none(),
            "relocated_children is omitted"
        );
        assert!(
            payload["repos"][0]["worktrees"]["actions"][index]
                .get("planned_children")
                .is_none(),
            "planned_children is omitted"
        );
    }
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
    let text = render_text(&aggregate, std::time::Duration::ZERO);
    assert!(
        text.contains("no repositories with recorded worktree leases"),
        "got: {text}"
    );
    assert!(text.contains("dry-run preview"), "got: {text}");
    assert!(
        text.contains("removed 0, eligible 0, absent 0, kept 0, removal failures 0"),
        "got: {text}"
    );
    assert!(text.contains("elapsed 0.000s"), "got: {text}");
    // A no-op default is bounded.
    assert!(
        text.lines().count() <= 8,
        "bounded output, got {} lines: {text}",
        text.lines().count()
    );
}

/// The compact default stays within the line budget even when many repositories
/// contribute only kept directories, and it never lists a kept path.
#[test]
fn prune_compact_text_is_bounded_across_many_repositories() {
    let repo_outcome = |index: usize| PruneRepoOutcome {
        repo_identity: format!("/tmp/repo-{index}/.git"),
        repo_path: Some(format!("/tmp/repo-{index}")),
        skipped_reason: None,
        leases: None,
        worktrees: Some(PruneSummary {
            scanned: 3,
            removed: 0,
            eligible: 0,
            absent: 1,
            kept_active: 1,
            kept_unknown: 0,
            kept_protected: 1,
            kept_issue: 0,
            failed: 0,
            dry_run: false,
            stale_days: 7,
            actions: vec![
                PruneAction {
                    worktree_path: format!("/tmp/repo-{index}/kept-a"),
                    branch: "phasegent/1-a".to_owned(),
                    status: LEASE_STATUS_RETAINED.to_owned(),
                    heartbeat_at: 0,
                    lease_ids: vec!["lease-a".to_owned()],
                    issues: vec![1],
                    prunable: false,
                    result: "skipped_protected".to_owned(),
                    message: Some("uncommitted or untracked files".to_owned()),
                    relocated_children: Vec::new(),
                    planned_children: Vec::new(),
                },
                PruneAction {
                    worktree_path: format!("/tmp/repo-{index}/absent"),
                    branch: "phasegent/1-b".to_owned(),
                    status: LEASE_STATUS_RETAINED.to_owned(),
                    heartbeat_at: 0,
                    lease_ids: vec!["lease-b".to_owned()],
                    issues: vec![1],
                    prunable: false,
                    result: "absent".to_owned(),
                    message: Some("worktree directory already absent".to_owned()),
                    relocated_children: Vec::new(),
                    planned_children: Vec::new(),
                },
            ],
        }),
    };
    let repos: Vec<PruneRepoOutcome> = (0..19).map(repo_outcome).collect();
    let totals = PruneTotals {
        repositories: 19,
        removed: 0,
        eligible: 0,
        absent: 19,
        kept_active: 19,
        kept_unknown: 0,
        kept_protected: 19,
        kept_issue: 0,
        failed: 0,
    };
    let aggregate = PruneAggregate {
        dry_run: false,
        remove: true,
        release_stale: false,
        stale_days: 7,
        repos,
        totals,
    };
    let text = render_text(&aggregate, std::time::Duration::from_millis(42));
    // The report stays bounded by the number of actionable rows: one line per
    // kept directory plus the summary lines.
    assert!(
        text.lines().count() <= 19 + 5,
        "19 repositories must stay bounded, got {}: {text}",
        text.lines().count()
    );
    assert!(
        text.contains("removed 0, eligible 0, absent 19, kept 38, removal failures 0"),
        "got: {text}"
    );
    // A kept directory is listed with its reason; an absent directory is
    // reported by the counts only.
    assert!(text.contains("kept /tmp/repo-0/kept-a:"), "got: {text}");
    assert!(!text.contains("absent /tmp"), "got: {text}");
}

/// A `--release-stale` recovery stays visible in the compact default.
#[test]
fn prune_compact_text_reports_recovery_results() {
    let aggregate = PruneAggregate {
        dry_run: false,
        remove: true,
        release_stale: true,
        stale_days: 14,
        repos: vec![PruneRepoOutcome {
            repo_identity: "/tmp/repo/.git".to_owned(),
            repo_path: Some("/tmp/repo".to_owned()),
            skipped_reason: None,
            leases: Some(ReleaseStaleSummary {
                repo_identity: "/tmp/repo/.git".to_owned(),
                stale_days: 14,
                stale_before: 100,
                apply: true,
                candidates: 3,
                released: 2,
                reason: Some("stale recovery".to_owned()),
                leases: vec![],
            }),
            worktrees: Some(PruneSummary {
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
            }),
        }],
        totals: PruneTotals {
            repositories: 1,
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
    let text = render_text(&aggregate, std::time::Duration::from_millis(500));
    assert!(
        text.contains("leases: 3 stale candidate(s), 2 released"),
        "recovery counts stay visible: {text}"
    );
    assert!(text.lines().count() <= 8, "bounded: {text}");
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
    let exit = run_scoped_prune(
        &repo,
        14,
        true,
        Some("preview only"),
        true,
        "json",
        &source,
        &FakeOpenCodeApi::empty(),
    );
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
    let exit = run_scoped_prune(
        &repo,
        999,
        false,
        None,
        false,
        "json",
        &source,
        &FakeOpenCodeApi::empty(),
    );
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
    let exit = run_scoped_prune(
        &repo,
        999,
        false,
        None,
        false,
        "json",
        &source,
        &FakeOpenCodeApi::empty(),
    );
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
    let exit = run_scoped_prune(
        &repo,
        999,
        false,
        None,
        false,
        "json",
        &source,
        &FakeOpenCodeApi::empty(),
    );
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
        &FakeOpenCodeApi::empty(),
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

/// The prune follows the resolved output format. Without a scoped run the
/// explicit legacy parameter decides; inside a scoped run the scope carries
/// the resolved global format and the explicit legacy override.
#[test]
fn the_prune_output_format_follows_the_resolved_context() {
    use crate::output::context::{OutputMode, OutputOptions, OutputSelection, effective_mode};
    use crate::policy::CallerContext;

    // Without a scope the explicit parameter is the only signal.
    assert_eq!(effective_mode("json"), OutputMode::Json);
    assert_eq!(effective_mode("text"), OutputMode::Text);

    // A scoped run decides: the scope carries the resolved global format and
    // the explicit legacy override.
    let options = OutputOptions {
        selection: OutputSelection::Text,
        no_progress: false,
        legacy: Some(OutputMode::Json),
    };
    let _scope = crate::output::context::install(&options, Some(CallerContext::Operator));
    assert_eq!(
        effective_mode("json"),
        OutputMode::Json,
        "an explicit legacy override wins over the global format"
    );
    drop(_scope);

    let options = OutputOptions {
        selection: OutputSelection::Text,
        no_progress: false,
        legacy: None,
    };
    let _scope = crate::output::context::install(&options, Some(CallerContext::Operator));
    assert_eq!(
        effective_mode("text"),
        OutputMode::Text,
        "the resolved global format decides without a legacy override"
    );
}
