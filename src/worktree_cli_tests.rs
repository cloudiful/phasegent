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

use crate::cli::worktree::{PruneAction, execute_worktree, prune_pass};
use crate::command::{Command, WorktreeCommand};
use crate::infra::storage::Storage;
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::policy::Role;
use crate::worktree::leases::{NewLease, insert_lease};
use crate::worktree::{
    LEASE_STATUS_ACTIVE, LEASE_STATUS_RELEASED, LEASE_STATUS_RETAINED, LeaseRow,
    ProcessWorktreeRunner, WorktreeRunner, ensure_schema, list_for_repo, now_unix_secs,
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
        }) => {
            assert_eq!(issue, 1);
            assert_eq!(session, "phasegent");
            assert_eq!(base, None);
            assert_eq!(format, "json");
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
        }) => {
            assert_eq!(issue, 42);
            assert_eq!(session, "alpha");
            assert_eq!(base.as_deref(), Some("main"));
            assert_eq!(format, "json");
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
        Command::Worktree(WorktreeCommand::Release { lease, retain }) => {
            assert_eq!(lease, "lease-1");
            assert!(retain);
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
        Command::Worktree(WorktreeCommand::Release { lease, retain }) => {
            assert_eq!(lease, "lease-1");
            assert!(!retain);
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
fn parse_prune_defaults_dry_run_off_stale_days_14() {
    let invocation =
        crate::command::parse(&strings(["--role", "orchestrator", "worktree", "prune"])).unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Prune {
            stale_days,
            dry_run,
        }) => {
            assert_eq!(stale_days, 14);
            assert!(!dry_run);
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn parse_prune_dry_run_flag_round_trip() {
    let invocation = crate::command::parse(&strings([
        "--role",
        "orchestrator",
        "worktree",
        "prune",
        "--dry-run",
        "--stale-days",
        "7",
    ]))
    .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Prune {
            stale_days,
            dry_run,
        }) => {
            assert_eq!(stale_days, 7);
            assert!(dry_run);
        }
        other => panic!("unexpected command {other:?}"),
    }
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

// ---------------------------------------------------------------------------
// Policy gates (command-level role checks)
// ---------------------------------------------------------------------------

#[test]
fn executor_cannot_acquire() {
    let exit = execute_worktree(
        Some(Role::Executor),
        WorktreeCommand::Acquire {
            issue: 1,
            session: "s".to_owned(),
            base: None,
            format: "json".to_owned(),
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
        },
    );
    assert_eq!(exit, 3, "permission error must return exit code 3");
}

#[test]
fn executor_cannot_prune() {
    let exit = execute_worktree(
        Some(Role::Executor),
        WorktreeCommand::Prune {
            stale_days: 14,
            dry_run: true,
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
            session: "s".to_owned(),
            base: None,
            format: "json".to_owned(),
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
        true,
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
        true,
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
        false,
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
        true,
    );
    assert_eq!(summary.scanned, 1);
    assert_eq!(summary.pruned, 0);
    let action = &summary.actions[0];
    assert_eq!(action.lease_id, "lease-real-dirty");
    assert!(!action.prunable, "dirty lease must not be prunable");
    assert_eq!(action.result, "skipped_dirty");
    let _ = std::fs::remove_file(&dirty);
}
