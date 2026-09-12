//! Focused tests for Phase 2 of issue #239 (worktree-seamless).
//!
//! Coverage mirrors the three scope rows the Phase 2 task calls out:
//!
//! * **CLI parsing** — every `worktree` subcommand parses, default
//!   values land where the help advertises them, and the strict
//!   missing-value detection (`--lease` without a value,
//!   non-numeric `--issue`, non-JSON `--format`) is preserved.
//! * **Policy gates** — the role gate is command-level, not
//!   capability-level, and matches the documented split:
//!   `acquire` / `release` / `prune` are orchestrator-only;
//!   `status` / `list` are available to orchestrator, executor, and
//!   reviewer; tester is denied. The executor function returns the
//!   documented `permission` JSON envelope with exit code 3.
//! * **Prune dry-run** — the prune pass classifies every lease
//!   row correctly: active and released rows are skipped, recent
//!   retained rows are skipped, dirty retained rows are
//!   `prunable=false`, and clean + stale + retained rows are
//!   reported as `prunable=true` in dry-run mode. A separate test
//!   drives the same classification against a real temp repo so the
//!   `git status --porcelain` path is exercised end-to-end.
//!
//! All tests run with `PHASEGENT_DB_PATH` pointed at a temp SQLite
//! so the operator's real database is never touched. Each prune
//! integration test also uses its own temp git repo so production
//! worktrees are never mutated.

use crate::cli::worktree::{
    AcquireJson, PruneAction, PruneCombinedSummary, PruneDisposition, PruneMode, PruneSummary,
    ReleaseStaleAction, ReleaseStaleSummary, execute_worktree, prune_pass,
    scan_worktree_candidates,
};
use crate::command::{Command, WorktreeCommand};
use crate::infra::storage::Storage;
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::policy::Role;
use crate::worktree::leases::{NewLease, insert_lease};
use crate::worktree::{
    AcquireOutcome, LEASE_STATUS_ACTIVE, LEASE_STATUS_RELEASED, LEASE_STATUS_RETAINED, LeaseRow,
    ProcessWorktreeRunner, WorktreeRunner, acquire_lease, ensure_schema, list_for_repo,
    now_unix_secs, repo_identity, resolve_worktree_auto, worktree_add,
};
use std::path::PathBuf;

fn strings<const N: usize>(values: [&str; N]) -> Vec<String> {
    values.into_iter().map(str::to_owned).collect()
}

fn open_temp_db(label: &str) -> (TempDir, Storage, EnvGuard) {
    let temp = TempDir::new(label);
    let db = temp.path().join("phasegent.sqlite3");
    let env = EnvGuard::set(
        "PHASEGENT_DB_PATH",
        db.as_os_str().to_string_lossy().as_ref(),
    );
    let storage = Storage::open_at(&db).expect("storage open");
    (temp, storage, env)
}

fn fresh_lease_row(
    lease_id: &str,
    issue: u64,
    session: &str,
    worktree_path: &str,
    branch: &str,
    status: &str,
    heartbeat_at: i64,
) -> LeaseRow {
    LeaseRow {
        lease_id: lease_id.to_owned(),
        repo_identity: "/tmp/repo".to_owned(),
        issue,
        session: session.to_owned(),
        checkout_path: "/tmp/repo".to_owned(),
        worktree_path: worktree_path.to_owned(),
        branch: branch.to_owned(),
        status: status.to_owned(),
        created_at: heartbeat_at,
        heartbeat_at,
        release_reason: None,
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "phasegent-wt-cli-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir create");
        Self(dir)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn insert_row(storage: &Storage, row: &LeaseRow) {
    insert_lease(
        storage,
        NewLease {
            lease_id: &row.lease_id,
            identity: &row.repo_identity,
            issue: row.issue,
            session: &row.session,
            checkout_path: &row.checkout_path,
            worktree_path: &row.worktree_path,
            branch: &row.branch,
            status: &row.status,
            created_at: row.created_at,
            heartbeat_at: row.heartbeat_at,
        },
    )
    .expect("insert row");
}

// ---------------------------------------------------------------------------
// CLI parsing
// ---------------------------------------------------------------------------

#[test]
fn parse_acquire_minimal() {
    let invocation = crate::command::parse(&strings([
        "--role",
        "orchestrator",
        "worktree",
        "acquire",
        "--issue",
        "1",
    ]))
    .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Acquire {
            issue,
            session,
            base,
            format,
            isolate,
        }) => {
            assert_eq!(issue, 1);
            assert_eq!(session, None);
            assert_eq!(base, None);
            assert_eq!(format, "json");
            assert!(!isolate, "--isolate must default off");
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn parse_acquire_isolate_flag_round_trips() {
    let invocation = crate::command::parse(&strings([
        "--role",
        "orchestrator",
        "worktree",
        "acquire",
        "--issue",
        "247",
        "--isolate",
    ]))
    .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Acquire { isolate, .. }) => {
            assert!(isolate, "--isolate must round-trip to the command");
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn parse_acquire_rejects_missing_issue() {
    let err = crate::command::parse(&strings(["--role", "orchestrator", "worktree", "acquire"]))
        .unwrap_err();
    assert!(err.contains("--issue"), "expected --issue error, got {err}");
}

#[test]
fn parse_acquire_with_session_and_base() {
    let invocation = crate::command::parse(&strings([
        "--role",
        "orchestrator",
        "worktree",
        "acquire",
        "--issue",
        "42",
        "--session",
        "alpha",
        "--base",
        "main",
    ]))
    .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Acquire {
            issue,
            session,
            base,
            format,
            isolate,
        }) => {
            assert_eq!(issue, 42);
            assert_eq!(session.as_deref(), Some("alpha"));
            assert_eq!(base.as_deref(), Some("main"));
            assert_eq!(format, "json");
            assert!(!isolate);
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn parse_acquire_rejects_non_json_format() {
    let err = crate::command::parse(&strings([
        "--role",
        "orchestrator",
        "worktree",
        "acquire",
        "--issue",
        "1",
        "--format",
        "yaml",
    ]))
    .unwrap_err();
    assert!(
        err.contains("--format"),
        "expected --format error, got {err}"
    );
}

#[test]
fn parse_acquire_rejects_non_numeric_issue() {
    let err = crate::command::parse(&strings([
        "--role",
        "orchestrator",
        "worktree",
        "acquire",
        "--issue",
        "abc",
    ]))
    .unwrap_err();
    assert!(err.contains("--issue"), "expected --issue error, got {err}");
}

#[test]
fn parse_release_default_retain_is_true() {
    let invocation = crate::command::parse(&strings([
        "--role",
        "orchestrator",
        "worktree",
        "release",
        "--lease",
        "lease-1",
    ]))
    .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Release {
            lease,
            retain,
            force,
            reason,
        }) => {
            assert_eq!(lease, "lease-1");
            assert!(retain);
            assert!(!force);
            assert_eq!(reason, None);
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn parse_release_retain_false_round_trip() {
    let invocation = crate::command::parse(&strings([
        "--role",
        "orchestrator",
        "worktree",
        "release",
        "--lease",
        "lease-1",
        "--retain",
        "false",
    ]))
    .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Release {
            lease,
            retain,
            force,
            reason,
        }) => {
            assert_eq!(lease, "lease-1");
            assert!(!retain);
            assert!(!force);
            assert_eq!(reason, None);
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn parse_release_rejects_bad_retain() {
    let err = crate::command::parse(&strings([
        "--role",
        "orchestrator",
        "worktree",
        "release",
        "--lease",
        "lease-1",
        "--retain",
        "maybe",
    ]))
    .unwrap_err();
    assert!(
        err.contains("--retain"),
        "expected --retain error, got {err}"
    );
}

#[test]
fn parse_status_requires_issue() {
    let err =
        crate::command::parse(&strings(["--role", "executor", "worktree", "status"])).unwrap_err();
    assert!(err.contains("--issue"), "expected --issue error, got {err}");
}

#[test]
fn parse_list_repo_optional() {
    let invocation =
        crate::command::parse(&strings(["--role", "executor", "worktree", "list"])).unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::List { repo }) => assert!(repo.is_none()),
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn parse_prune_defaults_to_read_only_and_stale_days_14() {
    let invocation =
        crate::command::parse(&strings(["--role", "orchestrator", "worktree", "prune"])).unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Prune {
            repo,
            stale_days,
            release_stale,
            remove,
            reason,
        }) => {
            assert_eq!(repo, None);
            assert_eq!(stale_days, 14);
            assert!(!release_stale);
            assert!(!remove);
            assert_eq!(reason, None);
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn parse_prune_remove_flag_round_trip() {
    let invocation = crate::command::parse(&strings([
        "--role",
        "orchestrator",
        "worktree",
        "prune",
        "--remove",
        "--stale-days",
        "7",
    ]))
    .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Prune {
            stale_days, remove, ..
        }) => {
            assert_eq!(stale_days, 7);
            assert!(remove);
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn parse_prune_rejects_removed_dry_run_flag() {
    let err = crate::command::parse(&strings([
        "--role",
        "orchestrator",
        "worktree",
        "prune",
        "--dry-run",
    ]))
    .unwrap_err();
    assert!(err.contains("--dry-run"), "unexpected error: {err}");
}

#[test]
fn parse_prune_rejects_removed_release_stale_subcommand() {
    let err = crate::command::parse(&strings([
        "--role",
        "orchestrator",
        "worktree",
        "release-stale",
    ]))
    .unwrap_err();
    assert!(
        err.contains("unknown worktree command 'release-stale'"),
        "unexpected error: {err}"
    );
}

#[test]
fn parse_prune_rejects_negative_stale_days() {
    let err = crate::command::parse(&strings([
        "--role",
        "orchestrator",
        "worktree",
        "prune",
        "--stale-days",
        "-1",
    ]))
    .unwrap_err();
    assert!(
        err.contains("--stale-days"),
        "expected --stale-days error, got {err}"
    );
}

#[test]
fn unknown_subcommand_is_rejected() {
    let err =
        crate::command::parse(&strings(["--role", "orchestrator", "worktree", "fly"])).unwrap_err();
    assert!(err.contains("fly"), "expected 'fly' in error, got {err}");
}

#[test]
fn top_level_help_routes_to_worktree() {
    let invocation =
        crate::command::parse(&strings(["--role", "orchestrator", "worktree", "--help"])).unwrap();
    match invocation.command {
        Command::Help(crate::command::HelpTopic::Worktree) => {}
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn subcommand_help_routes_to_command_topic() {
    let invocation = crate::command::parse(&strings([
        "--role",
        "orchestrator",
        "worktree",
        "acquire",
        "--help",
    ]))
    .unwrap();
    match invocation.command {
        Command::Help(crate::command::HelpTopic::WorktreeCommand(command)) => {
            assert_eq!(command, "acquire");
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn root_help_topic_routes_heartbeat_and_prune() {
    for command in ["heartbeat", "prune"] {
        let invocation = crate::command::parse(&strings([
            "--role",
            "orchestrator",
            "--help",
            "worktree",
            command,
        ]))
        .unwrap();
        match invocation.command {
            Command::Help(crate::command::HelpTopic::WorktreeCommand(value)) => {
                assert_eq!(value, command);
            }
            other => panic!("unexpected command {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Policy gates (command-level role checks)
// ---------------------------------------------------------------------------

#[test]
fn executor_cannot_acquire() {
    let exit = execute_worktree(
        Some(Role::Executor),
        WorktreeCommand::Acquire {
            issue: 1,
            session: Some("s".to_owned()),
            base: None,
            format: "json".to_owned(),
            isolate: false,
        },
    );
    assert_eq!(exit, 3, "permission error must return exit code 3");
}

#[test]
fn reviewer_cannot_release() {
    let exit = execute_worktree(
        Some(Role::Reviewer),
        WorktreeCommand::Release {
            lease: "lease-1".to_owned(),
            retain: true,
            force: false,
            reason: None,
        },
    );
    assert_eq!(exit, 3, "permission error must return exit code 3");
}

#[test]
fn executor_cannot_prune() {
    let exit = execute_worktree(
        Some(Role::Executor),
        WorktreeCommand::Prune {
            repo: None,
            stale_days: 14,
            release_stale: false,
            remove: false,
            reason: None,
        },
    );
    assert_eq!(exit, 3, "permission error must return exit code 3");
}

#[test]
fn tester_cannot_status() {
    // Use a synthetic `status` invocation that does not touch the
    // database: the role gate fires first.
    let exit = execute_worktree(Some(Role::Tester), WorktreeCommand::Status { issue: 1 });
    assert_eq!(exit, 3, "permission error must return exit code 3");
}

#[test]
fn tester_cannot_list() {
    let exit = execute_worktree(Some(Role::Tester), WorktreeCommand::List { repo: None });
    assert_eq!(exit, 3, "permission error must return exit code 3");
}

#[test]
fn admin_cannot_acquire() {
    let exit = execute_worktree(
        Some(Role::Admin),
        WorktreeCommand::Acquire {
            issue: 1,
            session: Some("s".to_owned()),
            base: None,
            format: "json".to_owned(),
            isolate: false,
        },
    );
    assert_eq!(exit, 3, "permission error must return exit code 3");
}

#[test]
fn orchestrator_status_passes_role_gate_and_queries_storage() {
    // The role gate is the first check; the storage call only fires
    // when the role is allowed. We use a temp DB so the operator's
    // real database is never touched.
    let _lock = lock_workflow_tests();
    let (_temp, _storage, _env) = open_temp_db("status-role");
    let exit = execute_worktree(
        Some(Role::Orchestrator),
        WorktreeCommand::Status { issue: 1 },
    );
    assert_eq!(exit, 0, "orchestrator status must pass the role gate");
}

#[test]
fn executor_status_passes_role_gate() {
    let _lock = lock_workflow_tests();
    let (_temp, _storage, _env) = open_temp_db("status-executor");
    let exit = execute_worktree(Some(Role::Executor), WorktreeCommand::Status { issue: 1 });
    assert_eq!(exit, 0, "executor status must pass the role gate");
}

#[test]
fn reviewer_list_passes_role_gate() {
    let _lock = lock_workflow_tests();
    let (_temp, _storage, _env) = open_temp_db("list-reviewer");
    let exit = execute_worktree(Some(Role::Reviewer), WorktreeCommand::List { repo: None });
    // List requires a real git repo; the test path lives under
    // /tmp so `repo_identity` will return an error and the
    // executor returns the structured error. The important
    // assertion is the exit code is NOT 3, i.e. the role gate
    // let the call through.
    assert_ne!(exit, 3, "reviewer list must pass the role gate");
}

// ---------------------------------------------------------------------------
// Prune pass: dry-run classification (no real git)
// ---------------------------------------------------------------------------

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

struct TempRepo {
    dir: TempDir,
    head_branch: String,
}

impl TempRepo {
    fn init(label: &str) -> Option<Self> {
        let dir = TempDir::new(label);
        let runner = ProcessWorktreeRunner::new();
        if runner
            .run(&["init", "-q", "-b", "main"], dir.path())
            .is_err()
        {
            return None;
        }
        let _ = runner.run(
            &[
                "-c",
                "user.name=phasegent-test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "init",
            ],
            dir.path(),
        );
        let branch = match runner.run(&["symbolic-ref", "--quiet", "--short", "HEAD"], dir.path()) {
            Ok(out) if out.status == 0 => out.stdout.trim().to_string(),
            _ => "main".to_string(),
        };
        Some(Self {
            dir,
            head_branch: branch,
        })
    }
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

/// Bind the temp repo's current branch to `issue` through the same local
/// git config key `branch_context::read_issue_id` reads.
fn bind_current_branch(repo: &TempRepo, issue: u64) {
    let runner = ProcessWorktreeRunner::new();
    let key = crate::branch_context::config_key(&repo.head_branch);
    let output = runner
        .run(
            &["config", "--local", key.as_str(), &issue.to_string()],
            repo.dir.path(),
        )
        .expect("git config binding write");
    assert_eq!(output.status, 0, "binding write must succeed");
}

/// `(temp_dir, cache_dir, db_env, cache_env)` where `db_env` pins
/// `PHASEGENT_DB_PATH` and `cache_env` pins `PHASEGENT_WORKTREE_CACHE_DIR`
/// so an `acquire_lease(..., None)` call stays entirely inside temp dirs.
fn open_temp_db_and_cache(label: &str) -> (TempDir, TempDir, EnvGuard, EnvGuard) {
    let temp = TempDir::new(&format!("{label}-db"));
    let db = temp.path().join("phasegent.sqlite3");
    let db_env = EnvGuard::set(
        "PHASEGENT_DB_PATH",
        db.as_os_str().to_string_lossy().as_ref(),
    );
    let cache = TempDir::new(&format!("{label}-cache"));
    let cache_env = EnvGuard::set(
        "PHASEGENT_WORKTREE_CACHE_DIR",
        cache.path().as_os_str().to_string_lossy().as_ref(),
    );
    (temp, cache, db_env, cache_env)
}

/// Assert the serialised CLI acquire envelope carries exactly the six
/// documented fields (no `warnings` key) with the given decision values.
fn assert_cli_envelope(
    outcome: AcquireOutcome,
    expected_created: bool,
    expected_reason: &str,
) -> serde_json::Value {
    let json = serde_json::to_value(AcquireJson::from(outcome)).expect("envelope serialise");
    let object = json.as_object().expect("envelope must be an object");
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "branch",
            "created",
            "lease_id",
            "path",
            "reason",
            "repo_identity"
        ],
        "CLI envelope fields must stay stable"
    );
    assert_eq!(json["created"], serde_json::json!(expected_created));
    assert_eq!(json["reason"], serde_json::json!(expected_reason));
    json
}

#[test]
fn cli_acquire_dirty_foreign_bound_returns_new_worktree_envelope_with_warning() {
    // Issue #246 replica through the CLI-facing contract: empty lease
    // table + dirty checkout bound to 241 + acquiring 245 -> the CLI
    // envelope must say created:true / reason:"new_worktree" and the
    // stderr-bound warning must carry the bound-issue detail.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cli-dirty-foreign") else {
        return;
    };
    let (_db_temp, cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("cli-dirty-foreign");
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 241);
    let runner = ProcessWorktreeRunner::new();
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        None,
        true,
        false,
    )
    .expect("dirty + foreign-bound must isolate, not error");
    assert!(
        outcome.created,
        "dirty + bound-to-241 must create a worktree for 245"
    );
    assert_eq!(outcome.reason, "new_worktree");
    assert!(
        outcome
            .path
            .starts_with(cache_temp.path().to_string_lossy().as_ref()),
        "new worktree must live under the temp cache"
    );
    assert!(outcome.branch.starts_with("phasegent/245-"));
    let warning_text = outcome.warnings.join(" ");
    assert!(
        outcome.warnings.iter().any(|w| w.contains("241")),
        "trigger detail must reach the stderr warning payload: {warning_text}"
    );
    assert_cli_envelope(outcome, true, "new_worktree");
    let _ = std::fs::remove_file(&scratch);
}

#[test]
fn cli_acquire_dirty_unbound_returns_reuse_envelope_with_warning() {
    // Dirty + unbound: no task evidence -> the CLI envelope must say
    // created:false / reason:"no_conflict" (reuse) and the stderr-bound
    // warning must explain the reused tree is not pristine.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cli-dirty-unbound") else {
        return;
    };
    let (_db_temp, _cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("cli-dirty-unbound");
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    let runner = ProcessWorktreeRunner::new();
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        None,
        false,
        false,
    )
    .expect("dirty + unbound must reuse, not error");
    assert!(!outcome.created);
    assert_eq!(outcome.reason, "no_conflict");
    assert_eq!(outcome.path, repo.dir.path().to_string_lossy().to_string());
    let joined = outcome.warnings.join(" ");
    assert!(
        joined.contains("dirty") && joined.contains("not bound"),
        "reuse of a dirty unbound checkout must carry a warning: {joined}"
    );
    assert_cli_envelope(outcome, false, "no_conflict");
    let _ = std::fs::remove_file(&scratch);
}

#[test]
fn cli_acquire_envelope_is_unchanged_when_warnings_are_present() {
    // Regression guard for the "CLI envelope fields unchanged" rule: even
    // when the outcome carries warnings (dirty-tree triggers), the JSON the
    // CLI prints must contain exactly lease_id/path/branch/repo_identity/
    // created/reason and never a warnings key.
    let outcome = AcquireOutcome {
        lease_id: "lease-x".to_owned(),
        path: "/tmp/cache/wt".to_owned(),
        branch: "phasegent/245-abcdef".to_owned(),
        repo_identity: "/tmp/r/.git".to_owned(),
        created: true,
        reason: "new_worktree".to_owned(),
        warnings: vec![
            "checkout is dirty and bound to issue 241; acquiring issue 245 \
             in an isolated worktree"
                .to_owned(),
            "checkout is dirty and not bound to an issue; reusing it \
             (no task evidence of a conflict)"
                .to_owned(),
        ],
    };
    let json = serde_json::to_value(AcquireJson::from(outcome)).expect("envelope serialise");
    let text = json.to_string();
    assert!(
        !text.contains("warnings"),
        "CLI envelope must not leak warnings: {text}"
    );
    assert_eq!(json["lease_id"], serde_json::json!("lease-x"));
    assert_eq!(json["path"], serde_json::json!("/tmp/cache/wt"));
    assert_eq!(json["branch"], serde_json::json!("phasegent/245-abcdef"));
    assert_eq!(json["repo_identity"], serde_json::json!("/tmp/r/.git"));
    assert_eq!(json["created"], serde_json::json!(true));
    assert_eq!(json["reason"], serde_json::json!("new_worktree"));
}

#[test]
fn cli_acquire_executor_resolves_cache_through_env_override() {
    // The CLI executor passes `cache_base = None`; production resolution
    // must route through `PHASEGENT_WORKTREE_CACHE_DIR` when set. This
    // proves the executor's env-driven cache path lands in the temp dir
    // rather than the real OS cache (temp-only guarantee).
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cli-cache-env") else {
        return;
    };
    let (db_temp, cache_temp, db_env, cache_env) = open_temp_db_and_cache("cli-cache-env");
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 7);
    let runner = ProcessWorktreeRunner::new();
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        None,
        true,
        false,
    )
    .expect("acquire through env-resolved cache");
    assert!(outcome.created);
    assert!(
        outcome
            .path
            .starts_with(cache_temp.path().to_string_lossy().as_ref()),
        "executor cache resolution must honour PHASEGENT_WORKTREE_CACHE_DIR"
    );
    drop((db_temp, cache_temp, db_env, cache_env));
    let _ = std::fs::remove_file(&scratch);
}

#[test]
fn cli_acquire_dirty_foreign_bound_recorded_in_temp_db_only() {
    // Temp-only guarantee: after a dirty + foreign-bound acquire through
    // the CLI-facing path, the lease row must live in the temp database
    // (queryable under the resolved repo identity) and never touch the
    // operator's real database.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cli-temp-db") else {
        return;
    };
    let (db_temp, _cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("cli-temp-db");
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 241);
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        None,
        true,
        false,
    )
    .expect("dirty + foreign-bound acquire");
    let storage = Storage::open_at(&db_temp.path().join("phasegent.sqlite3")).expect("storage");
    ensure_schema(&storage).expect("ensure_schema");
    let rows = list_for_repo(&storage, &identity).expect("list temp db");
    assert_eq!(
        rows.len(),
        1,
        "one active lease must be recorded in the temp DB"
    );
    assert_eq!(rows[0].issue, 245);
    assert_eq!(rows[0].lease_id, outcome.lease_id);
    assert_eq!(rows[0].worktree_path, outcome.path);
    assert_eq!(rows[0].status, LEASE_STATUS_ACTIVE);
    let _ = std::fs::remove_file(&scratch);
}

// ---------------------------------------------------------------------------
// Issue #247: worktree-auto switch + `--isolate` gating
// ---------------------------------------------------------------------------
//
// The default is off: a dirty checkout bound to another issue (or any
// other active lease) must be reused with a warning and must not create
// a worktree directory. `--isolate` or the resolved `worktree-auto`
// switch (env over SQLite, default false) restores the issue #246
// isolation behaviour. All tests pin their DB and cache to temp dirs.

#[test]
fn acquire_default_off_reuses_dirty_foreign_bound_with_warning_and_no_new_dir() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p1-default-off") else {
        return;
    };
    let (_db_temp, cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("p1-default-off");
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 241);
    let runner = ProcessWorktreeRunner::new();
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        None,
        false,
        false,
    )
    .expect("default-off acquire must reuse, not error");
    assert!(!outcome.created, "default-off must not create a worktree");
    assert_eq!(outcome.reason, "no_conflict");
    assert_eq!(outcome.path, repo.dir.path().to_string_lossy().to_string());
    assert!(
        outcome.warnings.iter().any(|w| w.contains("241")),
        "conflict warning must name the bound trigger: {:?}",
        outcome.warnings
    );
    assert!(
        !cache_temp.path().join("worktrees").exists(),
        "default-off must not create any worktree directory"
    );
    let _ = std::fs::remove_file(&scratch);
}

#[test]
fn acquire_isolate_flag_creates_new_worktree() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p1-isolate") else {
        return;
    };
    let (_db_temp, cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("p1-isolate");
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 241);
    let runner = ProcessWorktreeRunner::new();
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        None,
        true,
        false,
    )
    .expect("--isolate must create, not error");
    assert!(outcome.created, "--isolate must create a worktree");
    assert_eq!(outcome.reason, "new_worktree");
    assert!(
        outcome
            .path
            .starts_with(cache_temp.path().to_string_lossy().as_ref()),
        "new worktree must live under the temp cache"
    );
    assert!(outcome.branch.starts_with("phasegent/245-"));
    let _ = std::fs::remove_file(&scratch);
}

#[test]
fn acquire_env_worktree_auto_true_creates_new_worktree() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p1-env-auto") else {
        return;
    };
    let (_db_temp, _cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("p1-env-auto");
    let _auto_env = EnvGuard::set("PHASEGENT_WORKTREE_AUTO", "true");
    let storage = Storage::open().expect("storage open");
    let auto = resolve_worktree_auto(&storage).expect("resolve worktree-auto");
    assert!(auto, "PHASEGENT_WORKTREE_AUTO=true must resolve true");
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 241);
    let runner = ProcessWorktreeRunner::new();
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        None,
        false,
        auto,
    )
    .expect("auto=true must create, not error");
    assert!(outcome.created);
    assert_eq!(outcome.reason, "new_worktree");
    let _ = std::fs::remove_file(&scratch);
}

#[test]
fn acquire_sqlite_worktree_auto_true_creates_new_worktree() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p1-sqlite-auto") else {
        return;
    };
    let (_db_temp, _cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("p1-sqlite-auto");
    let storage = Storage::open().expect("storage open");
    crate::config_write::set_setting_value(None, "PHASEGENT_WORKTREE_AUTO", "true", &storage)
        .expect("config set worktree-auto true");
    let auto = resolve_worktree_auto(&storage).expect("resolve worktree-auto");
    assert!(auto, "SQLite worktree-auto=true must resolve true");
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 241);
    let runner = ProcessWorktreeRunner::new();
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        None,
        false,
        auto,
    )
    .expect("SQLite auto=true must create, not error");
    assert!(outcome.created);
    assert_eq!(outcome.reason, "new_worktree");
    let _ = std::fs::remove_file(&scratch);
}

#[test]
fn resolve_worktree_auto_defaults_false_and_env_false_overrides_sqlite_true() {
    let _lock = lock_workflow_tests();
    let (_db_temp, _cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("p1-precedence");
    let storage = Storage::open().expect("storage open");
    // Empty env behaves as unset -> default false.
    {
        let _unset = EnvGuard::set("PHASEGENT_WORKTREE_AUTO", "");
        assert!(
            !resolve_worktree_auto(&storage).expect("resolve default"),
            "unset worktree-auto must default false"
        );
    }
    crate::config_write::set_setting_value(None, "PHASEGENT_WORKTREE_AUTO", "true", &storage)
        .expect("config set worktree-auto true");
    assert!(
        resolve_worktree_auto(&storage).expect("resolve sqlite true"),
        "SQLite worktree-auto=true must resolve true"
    );
    let _auto_env = EnvGuard::set("PHASEGENT_WORKTREE_AUTO", "false");
    assert!(
        !resolve_worktree_auto(&storage).expect("resolve env false"),
        "env false must override SQLite true"
    );
}

#[test]
fn acquire_precedence_env_false_over_sqlite_true_reuses_current_checkout() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p1-precedence-reuse") else {
        return;
    };
    let (_db_temp, cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("p1-precedence-reuse");
    let storage = Storage::open().expect("storage open");
    crate::config_write::set_setting_value(None, "PHASEGENT_WORKTREE_AUTO", "true", &storage)
        .expect("config set worktree-auto true");
    let _auto_env = EnvGuard::set("PHASEGENT_WORKTREE_AUTO", "false");
    let auto = resolve_worktree_auto(&storage).expect("resolve worktree-auto");
    assert!(!auto, "env false must override SQLite true");
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 241);
    let runner = ProcessWorktreeRunner::new();
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        None,
        false,
        auto,
    )
    .expect("env-false precedence acquire must reuse, not error");
    assert!(!outcome.created);
    assert_eq!(outcome.reason, "no_conflict");
    assert!(
        !cache_temp.path().join("worktrees").exists(),
        "env-false precedence must not create a worktree directory"
    );
    let _ = std::fs::remove_file(&scratch);
}

// ---------------------------------------------------------------------------
// Issue #247 Phase 2: three-state CLI-surface contract tests
// ---------------------------------------------------------------------------
//
// These tests drive the CLI executor (`execute_worktree`) end-to-end so the
// contract the help text advertises is exercised through the same code path
// the operator's shell runs. Each test pins its DB and cache to temp dirs
// (temp-only guarantee), chdirs into a temp repo so the executor's
// `std::env::current_dir()` resolves to a sandboxed checkout, and asserts on
// the exit code together with the post-condition (lease row in the temp DB
// + presence/absence of the worktree dir under the temp cache).
//
// The three states covered:
//   1. Default off: dirty + foreign-bound + no `--isolate` + no env +
//      no SQLite setting -> exit 0, lease row inserted, NO worktree dir.
//   2. `--isolate`: dirty + foreign-bound + `--isolate=true` -> exit 0,
//      lease row inserted, worktree dir under the temp cache.
//   3. Env true: dirty + foreign-bound + `PHASEGENT_WORKTREE_AUTO=true` ->
//      exit 0, lease row inserted, worktree dir under the temp cache.

fn run_cli_acquire_in_temp_repo(repo: &TempRepo, _db_path: &std::path::Path, isolate: bool) -> i32 {
    run_cli_acquire_with_session(repo, isolate, Some("session-A"))
}

fn run_cli_acquire_with_session(repo: &TempRepo, isolate: bool, session: Option<&str>) -> i32 {
    let previous_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    std::env::set_current_dir(repo.dir.path()).expect("set cwd to temp repo");
    let exit = execute_worktree(
        Some(Role::Orchestrator),
        WorktreeCommand::Acquire {
            issue: 245,
            session: session.map(str::to_owned),
            base: None,
            format: "json".to_owned(),
            isolate,
        },
    );
    let _ = std::env::set_current_dir(&previous_cwd);
    exit
}

#[test]
fn cli_surface_acquire_default_off_reuses_dirty_foreign_bound_with_no_worktree_dir() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p2-cli-default-off") else {
        return;
    };
    let (db_temp, cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("p2-cli-default-off");
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 241);
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let exit =
        run_cli_acquire_in_temp_repo(&repo, &db_temp.path().join("phasegent.sqlite3"), false);
    assert_eq!(
        exit, 0,
        "default-off acquire through CLI surface must succeed"
    );
    let storage = Storage::open_at(&db_temp.path().join("phasegent.sqlite3")).expect("storage");
    let rows = list_for_repo(&storage, &identity).expect("list temp db");
    assert_eq!(
        rows.len(),
        1,
        "default-off CLI acquire must still record one lease"
    );
    assert_eq!(rows[0].issue, 245);
    assert_eq!(
        rows[0].worktree_path,
        repo.dir.path().to_string_lossy().to_string(),
        "default-off CLI acquire must reuse the current checkout as the worktree_path"
    );
    assert!(
        !cache_temp.path().join("worktrees").exists(),
        "default-off CLI acquire must not create a worktree directory"
    );
    let _ = std::fs::remove_file(&scratch);
}

#[test]
fn cli_surface_acquire_isolate_flag_creates_new_worktree_in_temp_cache() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p2-cli-isolate") else {
        return;
    };
    let (db_temp, cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("p2-cli-isolate");
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 241);
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let exit = run_cli_acquire_in_temp_repo(&repo, &db_temp.path().join("phasegent.sqlite3"), true);
    assert_eq!(exit, 0, "--isolate CLI acquire must succeed");
    let storage = Storage::open_at(&db_temp.path().join("phasegent.sqlite3")).expect("storage");
    let rows = list_for_repo(&storage, &identity).expect("list temp db");
    assert_eq!(rows.len(), 1, "--isolate CLI acquire must record one lease");
    assert_eq!(rows[0].issue, 245);
    assert!(
        rows[0].branch.starts_with("phasegent/245-"),
        "--isolate CLI acquire must use the new-issue branch slug"
    );
    assert!(
        rows[0]
            .worktree_path
            .starts_with(cache_temp.path().to_string_lossy().as_ref()),
        "--isolate CLI acquire must land under the temp cache, not the operator's real cache"
    );
    assert!(
        std::path::Path::new(&rows[0].worktree_path).exists(),
        "--isolate CLI acquire must create the worktree directory on disk"
    );
    let _ = std::fs::remove_file(&scratch);
}

#[test]
fn cli_surface_acquire_env_worktree_auto_true_creates_new_worktree_in_temp_cache() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p2-cli-env-auto") else {
        return;
    };
    let (db_temp, cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("p2-cli-env-auto");
    let _auto_env = EnvGuard::set("PHASEGENT_WORKTREE_AUTO", "true");
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 241);
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let exit =
        run_cli_acquire_in_temp_repo(&repo, &db_temp.path().join("phasegent.sqlite3"), false);
    assert_eq!(exit, 0, "env-true CLI acquire must succeed");
    let storage = Storage::open_at(&db_temp.path().join("phasegent.sqlite3")).expect("storage");
    let rows = list_for_repo(&storage, &identity).expect("list temp db");
    assert_eq!(rows.len(), 1, "env-true CLI acquire must record one lease");
    assert_eq!(rows[0].issue, 245);
    assert!(
        rows[0].branch.starts_with("phasegent/245-"),
        "env-true CLI acquire must use the new-issue branch slug"
    );
    assert!(
        rows[0]
            .worktree_path
            .starts_with(cache_temp.path().to_string_lossy().as_ref()),
        "env-true CLI acquire must land under the temp cache, not the operator's real cache"
    );
    assert!(
        std::path::Path::new(&rows[0].worktree_path).exists(),
        "env-true CLI acquire must create the worktree directory on disk"
    );
    let _ = std::fs::remove_file(&scratch);
}

#[test]
fn cli_acquire_explicit_session_overrides_environment_and_persists() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p2-cli-session-explicit") else {
        return;
    };
    let (db_temp, _cache_temp, _db_env, _cache_env) =
        open_temp_db_and_cache("p2-cli-session-explicit");
    let _session_env = EnvGuard::set("PHASEGENT_SESSION_ID", "env-session");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let exit = run_cli_acquire_with_session(&repo, false, Some("explicit-session"));
    assert_eq!(exit, 0, "explicit-session acquire must succeed");
    let storage = Storage::open_at(&db_temp.path().join("phasegent.sqlite3")).expect("storage");
    let rows = list_for_repo(&storage, &identity).expect("list temp db");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].session, "explicit-session",
        "--session must win over PHASEGENT_SESSION_ID"
    );
}

#[test]
fn cli_acquire_uses_environment_session_when_flag_absent() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p2-cli-session-env") else {
        return;
    };
    let (db_temp, _cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("p2-cli-session-env");
    let _session_env = EnvGuard::set("PHASEGENT_SESSION_ID", "env-session");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let exit = run_cli_acquire_with_session(&repo, false, None);
    assert_eq!(exit, 0, "environment-session acquire must succeed");
    let storage = Storage::open_at(&db_temp.path().join("phasegent.sqlite3")).expect("storage");
    let rows = list_for_repo(&storage, &identity).expect("list temp db");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].session, "env-session",
        "PHASEGENT_SESSION_ID must supply the session when --session is absent"
    );
}

// ---------------------------------------------------------------------------
// Issue 305 Task 4: unknown `git status` through the CLI surface
// ---------------------------------------------------------------------------
//
// A corrupt `.git/index` makes `git status --porcelain` fail while
// `git rev-parse --git-common-dir` and `git worktree add` keep working,
// so the real `execute_worktree` path can be driven into the `Unknown`
// dirty state. Auto-isolation on must create an isolated worktree;
// auto-isolation off must reuse the current checkout, never silently
// treating the checkout as clean.

fn corrupt_git_index(repo: &TempRepo) {
    std::fs::write(repo.dir.path().join(".git/index"), b"not-a-valid-index")
        .expect("corrupt git index");
}

#[test]
fn cli_surface_acquire_unknown_status_auto_isolation_creates_worktree() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("t4-cli-unknown-auto") else {
        return;
    };
    let (db_temp, cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("t4-cli-unknown-auto");
    let _auto_env = EnvGuard::set("PHASEGENT_WORKTREE_AUTO", "true");
    corrupt_git_index(&repo);
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let exit =
        run_cli_acquire_in_temp_repo(&repo, &db_temp.path().join("phasegent.sqlite3"), false);
    assert_eq!(
        exit, 0,
        "unknown status + auto-isolation CLI acquire must succeed"
    );
    let storage = Storage::open_at(&db_temp.path().join("phasegent.sqlite3")).expect("storage");
    let rows = list_for_repo(&storage, &identity).expect("list temp db");
    assert_eq!(rows.len(), 1, "CLI acquire must record one lease");
    assert_eq!(rows[0].issue, 245);
    assert!(
        rows[0].branch.starts_with("phasegent/245-"),
        "unknown status with isolation must create a fresh worktree"
    );
    assert!(
        rows[0]
            .worktree_path
            .starts_with(cache_temp.path().to_string_lossy().as_ref()),
        "isolated worktree must land under the temp cache"
    );
    assert!(
        std::path::Path::new(&rows[0].worktree_path).exists(),
        "unknown status with isolation must create the worktree directory"
    );
}

#[test]
fn cli_surface_acquire_unknown_status_default_off_reuses_checkout() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("t4-cli-unknown-off") else {
        return;
    };
    let (db_temp, cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("t4-cli-unknown-off");
    let _auto_env = EnvGuard::set("PHASEGENT_WORKTREE_AUTO", "false");
    corrupt_git_index(&repo);
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let exit =
        run_cli_acquire_in_temp_repo(&repo, &db_temp.path().join("phasegent.sqlite3"), false);
    assert_eq!(
        exit, 0,
        "unknown status + default off CLI acquire must succeed"
    );
    let storage = Storage::open_at(&db_temp.path().join("phasegent.sqlite3")).expect("storage");
    let rows = list_for_repo(&storage, &identity).expect("list temp db");
    assert_eq!(rows.len(), 1, "CLI acquire must record one lease");
    assert_eq!(rows[0].issue, 245);
    assert_eq!(
        rows[0].worktree_path,
        repo.dir.path().to_string_lossy().to_string(),
        "default-off unknown status must reuse the current checkout"
    );
    assert!(
        !cache_temp.path().join("worktrees").exists(),
        "default-off unknown status must not create a worktree directory"
    );
}

// ---------------------------------------------------------------------------
// Issue 305 Task 2 / issue 337 Phase 1: heartbeat + prune CLI surface
// ---------------------------------------------------------------------------

fn insert_lease_for_identity(
    storage: &Storage,
    lease_id: &str,
    identity: &str,
    session: &str,
    status: &str,
    heartbeat_at: i64,
    worktree_path: &str,
) {
    insert_lease(
        storage,
        NewLease {
            lease_id,
            identity,
            issue: 7,
            session,
            checkout_path: "/tmp/checkout",
            worktree_path,
            branch: "phasegent/7-aaaaaa",
            status,
            created_at: heartbeat_at,
            heartbeat_at,
        },
    )
    .expect("insert row");
}

fn prune_command(
    repo: &TempRepo,
    release_stale: bool,
    remove: bool,
    reason: Option<&str>,
) -> WorktreeCommand {
    WorktreeCommand::Prune {
        repo: Some(repo.dir.path().to_string_lossy().to_string()),
        stale_days: 14,
        release_stale,
        remove,
        reason: reason.map(str::to_owned),
    }
}

#[test]
fn parse_heartbeat_requires_lease_and_parses_session() {
    let invocation = crate::command::parse(&strings([
        "--role",
        "orchestrator",
        "worktree",
        "heartbeat",
        "--lease",
        "lease-1",
        "--session",
        "alpha",
    ]))
    .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Heartbeat { lease, session }) => {
            assert_eq!(lease, "lease-1");
            assert_eq!(session.as_deref(), Some("alpha"));
        }
        other => panic!("unexpected command {other:?}"),
    }
    let missing = crate::command::parse(&strings([
        "--role",
        "orchestrator",
        "worktree",
        "heartbeat",
    ]))
    .unwrap_err();
    assert!(missing.contains("--lease"), "unexpected error: {missing}");
}

#[test]
fn parse_heartbeat_rejects_blank_session() {
    let error = crate::command::parse(&strings([
        "--role",
        "orchestrator",
        "worktree",
        "heartbeat",
        "--lease",
        "lease-1",
        "--session",
        "",
    ]))
    .unwrap_err();
    assert!(error.contains("session"), "unexpected error: {error}");
}

#[test]
fn parse_prune_defaults_to_read_only_scan_and_14_days() {
    let invocation =
        crate::command::parse(&strings(["--role", "orchestrator", "worktree", "prune"])).unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Prune {
            repo,
            stale_days,
            release_stale,
            remove,
            reason,
        }) => {
            assert_eq!(repo, None);
            assert_eq!(stale_days, 14);
            assert!(!release_stale);
            assert!(!remove);
            assert_eq!(reason, None);
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn parse_prune_release_stale_requires_reason_and_reason_requires_release_stale() {
    let missing_reason = crate::command::parse(&strings([
        "--role",
        "orchestrator",
        "worktree",
        "prune",
        "--release-stale",
    ]))
    .unwrap_err();
    assert!(
        missing_reason.contains("--reason"),
        "unexpected error: {missing_reason}"
    );
    let dangling_reason = crate::command::parse(&strings([
        "--role",
        "orchestrator",
        "worktree",
        "prune",
        "--reason",
        "cleanup",
    ]))
    .unwrap_err();
    assert!(
        dangling_reason.contains("--release-stale"),
        "unexpected error: {dangling_reason}"
    );
    let invocation = crate::command::parse(&strings([
        "--role",
        "orchestrator",
        "worktree",
        "prune",
        "--repo",
        "/tmp/repo",
        "--stale-days",
        "3",
        "--release-stale",
        "--remove",
        "--reason",
        "cleanup",
    ]))
    .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Prune {
            repo,
            stale_days,
            release_stale,
            remove,
            reason,
        }) => {
            assert_eq!(repo.as_deref(), Some("/tmp/repo"));
            assert_eq!(stale_days, 3);
            assert!(release_stale);
            assert!(remove);
            assert_eq!(reason.as_deref(), Some("cleanup"));
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn parse_prune_rejects_non_numeric_stale_days() {
    let error = crate::command::parse(&strings([
        "--role",
        "orchestrator",
        "worktree",
        "prune",
        "--stale-days",
        "soon",
    ]))
    .unwrap_err();
    assert!(error.contains("--stale-days"), "unexpected error: {error}");
}

#[test]
fn heartbeat_and_prune_help_route_to_command_topics() {
    for command in ["heartbeat", "prune"] {
        let invocation = crate::command::parse(&strings([
            "--role",
            "orchestrator",
            "worktree",
            command,
            "--help",
        ]))
        .unwrap();
        match invocation.command {
            Command::Help(crate::command::HelpTopic::WorktreeCommand(value)) => {
                assert_eq!(value, command);
            }
            other => panic!("unexpected command {other:?}"),
        }
    }
}

#[test]
fn executor_cannot_heartbeat() {
    let exit = execute_worktree(
        Some(Role::Executor),
        WorktreeCommand::Heartbeat {
            lease: "lease-1".to_owned(),
            session: Some("s".to_owned()),
        },
    );
    assert_eq!(exit, 3, "permission error must return exit code 3");
}

#[test]
fn reviewer_cannot_prune() {
    let exit = execute_worktree(
        Some(Role::Reviewer),
        WorktreeCommand::Prune {
            repo: None,
            stale_days: 14,
            release_stale: false,
            remove: false,
            reason: None,
        },
    );
    assert_eq!(exit, 3, "permission error must return exit code 3");
}

#[test]
fn cli_heartbeat_refreshes_owned_active_lease() {
    let _lock = lock_workflow_tests();
    let (_temp, storage, _env) = open_temp_db("cli-heartbeat");
    ensure_schema(&storage).expect("schema");
    let old = now_unix_secs() - 1000;
    insert_lease_for_identity(
        &storage,
        "lease-hb",
        "/tmp/repo",
        "session-A",
        LEASE_STATUS_ACTIVE,
        old,
        "/tmp/repo",
    );
    let exit = execute_worktree(
        Some(Role::Orchestrator),
        WorktreeCommand::Heartbeat {
            lease: "lease-hb".to_owned(),
            session: Some("session-A".to_owned()),
        },
    );
    assert_eq!(exit, 0, "owned heartbeat must succeed");
    let row = list_for_repo(&storage, "/tmp/repo")
        .expect("list")
        .into_iter()
        .find(|row| row.lease_id == "lease-hb")
        .expect("row");
    assert!(row.heartbeat_at > old, "heartbeat must advance");
}

#[test]
fn cli_heartbeat_rejects_foreign_session_without_mutation() {
    let _lock = lock_workflow_tests();
    let (_temp, storage, _env) = open_temp_db("cli-heartbeat-foreign");
    ensure_schema(&storage).expect("schema");
    let old = now_unix_secs() - 1000;
    insert_lease_for_identity(
        &storage,
        "lease-hb",
        "/tmp/repo",
        "session-A",
        LEASE_STATUS_ACTIVE,
        old,
        "/tmp/repo",
    );
    let exit = execute_worktree(
        Some(Role::Orchestrator),
        WorktreeCommand::Heartbeat {
            lease: "lease-hb".to_owned(),
            session: Some("session-B".to_owned()),
        },
    );
    assert_eq!(exit, 1, "foreign session is a structured state conflict");
    let row = list_for_repo(&storage, "/tmp/repo")
        .expect("list")
        .into_iter()
        .find(|row| row.lease_id == "lease-hb")
        .expect("row");
    assert_eq!(row.heartbeat_at, old, "conflict must not mutate the row");
}

#[test]
fn cli_heartbeat_uses_environment_session() {
    let _lock = lock_workflow_tests();
    let (_temp, storage, _env) = open_temp_db("cli-heartbeat-env");
    ensure_schema(&storage).expect("schema");
    let _session_env = EnvGuard::set("PHASEGENT_SESSION_ID", "env-session");
    let old = now_unix_secs() - 1000;
    insert_lease_for_identity(
        &storage,
        "lease-hb",
        "/tmp/repo",
        "env-session",
        LEASE_STATUS_ACTIVE,
        old,
        "/tmp/repo",
    );
    let exit = execute_worktree(
        Some(Role::Orchestrator),
        WorktreeCommand::Heartbeat {
            lease: "lease-hb".to_owned(),
            session: None,
        },
    );
    assert_eq!(exit, 0, "environment session must own the lease");
    let row = list_for_repo(&storage, "/tmp/repo")
        .expect("list")
        .into_iter()
        .find(|row| row.lease_id == "lease-hb")
        .expect("row");
    assert!(row.heartbeat_at > old, "heartbeat must advance");
}

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

/// Create a real linked worktree on a fresh generated branch and return
/// its path. Uses the same wrapper the production acquire path uses so
/// prune runs against a real worktree rather than a stub directory.
fn add_real_worktree(repo: &TempRepo, branch: &str) -> PathBuf {
    let runner = ProcessWorktreeRunner::new();
    let target = repo.dir.path().join(branch.replace('/', "-"));
    worktree_add(&runner, repo.dir.path(), &target, branch).expect("worktree add");
    target
}

fn branch_exists(repo: &TempRepo, branch: &str) -> bool {
    let runner = ProcessWorktreeRunner::new();
    runner
        .run(&["branch", "--list", branch], repo.dir.path())
        .map(|out| out.status == 0 && !out.stdout.trim().is_empty())
        .unwrap_or(false)
}

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
