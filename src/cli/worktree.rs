//! Executor for the `worktree` command group (issue #239 Phase 2).
//!
//! Subcommand gating lives here (command-level, not capability-level)
//! so `acquire` / `release` / `prune` are orchestrator-only while
//! `status` / `list` mirror the issue-status read surface
//! (orchestrator, executor, reviewer; tester denied). Branch
//! deletion is never invoked; only `git worktree remove` is used on
//! clean candidates. `.env` and secret material are never read,
//! copied, or written by any code path here (issue #239 Decisions).
//!
//! Mutating subcommands route through `acquire_lease` /
//! `release_lease` / the new `prune_candidates` helper. The lease
//! table is created lazily through `ensure_schema` so the executor
//! can run against a pre-Phase-1 database without a migration step.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::command::WorktreeCommand;
use crate::infra::storage::Storage;
use crate::policy::Role;
use crate::worktree::git::{is_clean, worktree_remove};
use crate::worktree::leases::{ensure_schema, list_for_repo, load_lease, update_status};
use crate::worktree::{
    AcquireOutcome, LEASE_STATUS_ACTIVE, LEASE_STATUS_RELEASED, LEASE_STATUS_RETAINED,
    ProcessWorktreeRunner, ReleaseOutcome, WorktreeError, WorktreeRunner, leases_for_issue,
    now_unix_secs, repo_identity, resolve_worktree_auto,
};
const SECONDS_PER_DAY: i64 = 86_400;

/// One row of the JSON envelope returned by `acquire`. Field order
/// is stable for downstream parsing.
#[derive(Debug, Serialize)]
pub struct AcquireJson {
    pub lease_id: String,
    pub path: String,
    pub branch: String,
    pub repo_identity: String,
    pub created: bool,
    pub reason: String,
}

impl From<AcquireOutcome> for AcquireJson {
    fn from(outcome: AcquireOutcome) -> Self {
        Self {
            lease_id: outcome.lease_id,
            path: outcome.path,
            branch: outcome.branch,
            repo_identity: outcome.repo_identity,
            created: outcome.created,
            reason: outcome.reason,
        }
    }
}

/// One row of the JSON envelope returned by `release`.
#[derive(Debug, Serialize)]
pub struct ReleaseJson {
    pub lease_id: String,
    pub status: String,
}

impl From<ReleaseOutcome> for ReleaseJson {
    fn from(outcome: ReleaseOutcome) -> Self {
        Self {
            lease_id: outcome.lease_id,
            status: outcome.status,
        }
    }
}

/// One row of the JSON envelope returned by `status` and `list`.
#[derive(Debug, Serialize)]
pub struct LeaseJson {
    pub lease_id: String,
    pub repo_identity: String,
    pub issue: u64,
    pub session: String,
    pub checkout_path: String,
    pub worktree_path: String,
    pub branch: String,
    pub status: String,
    pub created_at: i64,
    pub heartbeat_at: i64,
}

impl From<crate::worktree::LeaseRow> for LeaseJson {
    fn from(row: crate::worktree::LeaseRow) -> Self {
        Self {
            lease_id: row.lease_id,
            repo_identity: row.repo_identity,
            issue: row.issue,
            session: row.session,
            checkout_path: row.checkout_path,
            worktree_path: row.worktree_path,
            branch: row.branch,
            status: row.status,
            created_at: row.created_at,
            heartbeat_at: row.heartbeat_at,
        }
    }
}

/// One action of the prune pass. `prunable: false` is the explicit
/// safety signal; the caller can tell at a glance which leases
/// were skipped because they were dirty, still active, or too
/// recent.
#[derive(Debug, Serialize)]
pub struct PruneAction {
    pub lease_id: String,
    pub worktree_path: String,
    pub branch: String,
    pub status: String,
    pub heartbeat_at: i64,
    pub prunable: bool,
    pub result: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Envelope returned by `prune`. Stable shape: `dry_run` and
/// `stale_days` echo the inputs so the same call can be replayed
/// from the recorded output.
#[derive(Debug, Serialize)]
pub struct PruneSummary {
    pub scanned: usize,
    pub candidates: usize,
    pub pruned: usize,
    pub skipped_dirty: usize,
    pub skipped_active_or_released: usize,
    pub skipped_recent: usize,
    pub dry_run: bool,
    pub stale_days: u32,
    pub actions: Vec<PruneAction>,
}

pub(crate) fn execute_worktree(role_value: Option<Role>, command: WorktreeCommand) -> i32 {
    let role = super::required_role(role_value);
    match command {
        WorktreeCommand::Acquire {
            issue,
            session,
            base: _,
            format,
            isolate,
        } => {
            if role != Role::Orchestrator {
                return permission_error(role, "worktree acquire");
            }
            execute_acquire(role, issue, &session, &format, isolate)
        }
        WorktreeCommand::Release { lease, retain } => {
            if role != Role::Orchestrator {
                return permission_error(role, "worktree release");
            }
            execute_release(&lease, retain)
        }
        WorktreeCommand::Status { issue } => {
            if !is_read_role(role) {
                return permission_error(role, "worktree status");
            }
            execute_status(issue)
        }
        WorktreeCommand::List { repo } => {
            if !is_read_role(role) {
                return permission_error(role, "worktree list");
            }
            execute_list(repo.as_deref())
        }
        WorktreeCommand::Prune {
            stale_days,
            dry_run,
        } => {
            if role != Role::Orchestrator {
                return permission_error(role, "worktree prune");
            }
            execute_prune(stale_days, dry_run)
        }
    }
}

fn is_read_role(role: Role) -> bool {
    matches!(role, Role::Orchestrator | Role::Executor | Role::Reviewer)
}

fn permission_error(role: Role, operation: &str) -> i32 {
    super::structured_error(
        serde_json::json!({
            "kind": "permission",
            "role": role.as_str(),
            "operation": operation,
            "message": format!("role '{}' is not allowed to perform {operation}", role)
        }),
        3,
    )
}

fn execute_acquire(role: Role, issue: u64, session: &str, format: &str, isolate: bool) -> i32 {
    debug_assert_eq!(role, Role::Orchestrator);
    let _ = format; // only "json" is accepted at the parser layer
    let repo_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let runner = ProcessWorktreeRunner::new();
    // Resolve the persisted `worktree-auto` switch (env over SQLite,
    // default false) so `--isolate` and the setting share one gate.
    let storage = match open_storage() {
        Ok(storage) => storage,
        Err(message) => return config_error(&message),
    };
    let auto = match resolve_worktree_auto(&storage) {
        Ok(auto) => auto,
        Err(error) => {
            return super::structured_error(
                serde_json::json!({
                    "kind": error.kind,
                    "message": error.message,
                }),
                1,
            );
        }
    };
    match crate::worktree::acquire_lease(&runner, &repo_path, issue, session, None, isolate, auto) {
        Ok(outcome) => {
            let payload = AcquireJson::from(outcome);
            super::print_json(&payload)
        }
        Err(error) => super::structured_error(
            serde_json::json!({
                "kind": error.kind,
                "message": error.message,
            }),
            1,
        ),
    }
}

fn execute_release(lease: &str, retain: bool) -> i32 {
    match crate::worktree::release_lease(lease, retain) {
        Ok(outcome) => {
            let payload = ReleaseJson::from(outcome);
            super::print_json(&payload)
        }
        Err(error) => super::structured_error(
            serde_json::json!({
                "kind": error.kind,
                "message": error.message,
            }),
            1,
        ),
    }
}

fn execute_status(issue: u64) -> i32 {
    match leases_for_issue(issue) {
        Ok(rows) => {
            let payload = serde_json::json!({
                "issue": issue,
                "leases": rows.into_iter().map(LeaseJson::from).collect::<Vec<_>>(),
            });
            super::print_json(&payload)
        }
        Err(error) => storage_error(&error),
    }
}

fn execute_list(repo: Option<&str>) -> i32 {
    let storage = match open_storage() {
        Ok(storage) => storage,
        Err(message) => return config_error(&message),
    };
    if let Err(message) = ensure_schema(&storage) {
        return config_error(&message);
    }
    let identity = match resolve_list_identity(&storage, repo) {
        Ok(identity) => identity,
        Err(message) => return super::structured_error(message, 2),
    };
    match list_for_repo(&storage, &identity) {
        Ok(rows) => {
            let payload = serde_json::json!({
                "repo_identity": identity,
                "leases": rows.into_iter().map(LeaseJson::from).collect::<Vec<_>>(),
            });
            super::print_json(&payload)
        }
        Err(error) => storage_error(&error),
    }
}

fn execute_prune(stale_days: u32, dry_run: bool) -> i32 {
    let storage = match open_storage() {
        Ok(storage) => storage,
        Err(message) => return config_error(&message),
    };
    if let Err(message) = ensure_schema(&storage) {
        return config_error(&message);
    }
    let repo_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let runner = ProcessWorktreeRunner::new();
    let identity = match repo_identity(&runner, &repo_path) {
        Ok(identity) => identity,
        Err(error) => {
            return super::structured_error(
                serde_json::json!({
                    "kind": error.kind,
                    "message": error.message,
                }),
                2,
            );
        }
    };
    let rows = match list_for_repo(&storage, &identity) {
        Ok(rows) => rows,
        Err(error) => return storage_error(&error),
    };
    let summary = prune_pass(&storage, &runner, &repo_path, &rows, stale_days, dry_run);
    super::print_json(&summary)
}

fn open_storage() -> Result<Storage, String> {
    Storage::open()
}

fn config_error(message: &str) -> i32 {
    super::structured_error(serde_json::json!({"kind": "config", "message": message}), 1)
}

fn storage_error(error: &WorktreeError) -> i32 {
    super::structured_error(
        serde_json::json!({
            "kind": error.kind,
            "message": error.message,
        }),
        1,
    )
}

fn resolve_list_identity(
    storage: &Storage,
    repo: Option<&str>,
) -> Result<String, serde_json::Value> {
    let repo_path = match repo {
        Some(raw) => PathBuf::from(raw),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    if !repo_path.exists() {
        return Err(serde_json::json!({
            "kind": "argument",
            "message": format!("worktree list --repo path does not exist: {}", repo_path.display()),
        }));
    }
    let runner = ProcessWorktreeRunner::new();
    match repo_identity(&runner, &repo_path) {
        Ok(identity) => {
            // Persist the schema if it was not present yet so a
            // `list` call also works on pre-Phase-1 databases.
            let _ = ensure_schema(storage);
            Ok(identity)
        }
        Err(error) => Err(serde_json::json!({
            "kind": error.kind,
            "message": error.message,
        })),
    }
}

/// The prune pass. Public within the crate so the focused tests
/// can drive it without a full CLI round-trip. Branches are never
/// deleted; only `git worktree remove` is invoked, and only on a
/// clean worktree. The lease row is flipped to `released` only on
/// a successful non-dry-run remove so the table keeps an audit
/// trail of what was actually removed.
pub(crate) fn prune_pass(
    storage: &Storage,
    runner: &dyn WorktreeRunner,
    repo_path: &Path,
    rows: &[crate::worktree::LeaseRow],
    stale_days: u32,
    dry_run: bool,
) -> PruneSummary {
    let now = now_unix_secs();
    let threshold_secs = i64::from(stale_days) * SECONDS_PER_DAY;
    let mut summary = PruneSummary {
        scanned: rows.len(),
        candidates: 0,
        pruned: 0,
        skipped_dirty: 0,
        skipped_active_or_released: 0,
        skipped_recent: 0,
        dry_run,
        stale_days,
        actions: Vec::with_capacity(rows.len()),
    };
    for row in rows {
        let action = prune_action_for(
            storage,
            runner,
            repo_path,
            row,
            now,
            threshold_secs,
            dry_run,
        );
        if action.prunable && !dry_run && action.result == "pruned" {
            summary.pruned += 1;
        }
        if action.result == "prunable" {
            summary.candidates += 1;
        }
        match action.result.as_str() {
            "skipped_dirty" => summary.skipped_dirty += 1,
            "skipped_active" | "skipped_released" => summary.skipped_active_or_released += 1,
            "skipped_recent" => summary.skipped_recent += 1,
            _ => {}
        }
        summary.actions.push(action);
    }
    summary
}

fn prune_action_for(
    storage: &Storage,
    runner: &dyn WorktreeRunner,
    repo_path: &Path,
    row: &crate::worktree::LeaseRow,
    now: i64,
    threshold_secs: i64,
    dry_run: bool,
) -> PruneAction {
    let base = PruneAction {
        lease_id: row.lease_id.clone(),
        worktree_path: row.worktree_path.clone(),
        branch: row.branch.clone(),
        status: row.status.clone(),
        heartbeat_at: row.heartbeat_at,
        prunable: false,
        result: "noop".to_owned(),
        message: None,
    };
    match row.status.as_str() {
        LEASE_STATUS_ACTIVE => {
            return PruneAction {
                result: "skipped_active".to_owned(),
                message: Some("active leases are never pruned".to_owned()),
                ..base
            };
        }
        LEASE_STATUS_RELEASED => {
            return PruneAction {
                result: "skipped_released".to_owned(),
                message: Some("already released; no worktree to remove".to_owned()),
                ..base
            };
        }
        LEASE_STATUS_RETAINED => {}
        _ => {
            return PruneAction {
                result: "skipped_released".to_owned(),
                message: Some(format!(
                    "unrecognised lease status {}; treated as released",
                    row.status
                )),
                ..base
            };
        }
    }
    let age_secs = now.saturating_sub(row.heartbeat_at);
    if age_secs < threshold_secs {
        return PruneAction {
            result: "skipped_recent".to_owned(),
            message: Some(format!(
                "heartbeat age {age_secs}s is below stale threshold {threshold_secs}s"
            )),
            ..base
        };
    }
    // Retained + past threshold: run the clean probe.
    let worktree_path = PathBuf::from(&row.worktree_path);
    if !worktree_path.exists() {
        // The directory is already gone but the lease row still
        // records it. Mark the row as released so the operator can
        // see the audit trail converge.
        let mut action = PruneAction {
            prunable: true,
            result: "noop".to_owned(),
            message: Some("worktree directory already absent".to_owned()),
            ..base
        };
        if !dry_run {
            if let Err(error) = update_status(storage, &row.lease_id, LEASE_STATUS_RELEASED) {
                action.message = Some(format!("could not mark released: {error}"));
            } else if let Err(error) = reload_lease_branch(storage, &row.lease_id, &mut action) {
                let _ = error;
            }
        }
        return action;
    }
    let clean = match is_clean(runner, &worktree_path) {
        Ok(value) => value,
        Err(error) => {
            return PruneAction {
                prunable: false,
                result: "skipped_dirty".to_owned(),
                message: Some(format!("is_clean probe failed: {}", error.message)),
                ..base
            };
        }
    };
    if !clean {
        return PruneAction {
            prunable: false,
            result: "skipped_dirty".to_owned(),
            message: Some("worktree has uncommitted changes".to_owned()),
            ..base
        };
    }
    if dry_run {
        return PruneAction {
            prunable: true,
            result: "prunable".to_owned(),
            message: Some("dry-run: would call git worktree remove".to_owned()),
            ..base
        };
    }
    match worktree_remove(runner, repo_path, &worktree_path) {
        Ok(()) => {
            if let Err(error) = update_status(storage, &row.lease_id, LEASE_STATUS_RELEASED) {
                return PruneAction {
                    prunable: true,
                    result: "skipped_dirty".to_owned(),
                    message: Some(format!(
                        "worktree removed but lease flip failed: {}",
                        error.message
                    )),
                    ..base
                };
            }
            PruneAction {
                prunable: true,
                result: "pruned".to_owned(),
                message: Some("git worktree remove succeeded".to_owned()),
                ..base
            }
        }
        Err(error) => {
            super::report_local_warnings(
                "worktree prune",
                Some(format!(
                    "git worktree remove failed for {}: {}",
                    worktree_path.display(),
                    error.message
                )),
            );
            PruneAction {
                prunable: false,
                result: "skipped_dirty".to_owned(),
                message: Some(format!("worktree_remove error: {}", error.message)),
                ..base
            }
        }
    }
}

fn reload_lease_branch(
    storage: &Storage,
    lease_id: &str,
    action: &mut PruneAction,
) -> Result<(), WorktreeError> {
    if let Some(updated) = load_lease(storage, lease_id)? {
        action.status = updated.status;
    }
    Ok(())
}
