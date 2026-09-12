//! Executor for the `worktree` command group (issue #239 Phase 2).
//!
//! Subcommand gating lives here (command-level, not capability-level)
//! so `acquire` / `release` / `heartbeat` / `prune` are
//! orchestrator-only while `status` / `list` mirror the issue-status
//! read surface (orchestrator, executor, reviewer; tester denied).
//! Branch deletion is never invoked; only `git worktree remove` is used
//! on clean candidates. `.env` and secret material are never read,
//! copied, or written by any code path here (issue #239 Decisions).
//!
//! The subcommand executors are split by concern (issue 337 Phase 2):
//!
//! * [`acquire`] — `acquire` / `release` / `heartbeat`.
//! * [`query`] — read-only `status` / `list`.
//! * [`prune`] — `prune` and its candidate-scan / action-apply model.
//!
//! Mutating subcommands route through `acquire_lease` /
//! `release_lease` / the `prune` execution model. The lease table is
//! created lazily through `ensure_schema` so the executor can run
//! against a pre-Phase-1 database without a migration step.

use std::path::PathBuf;

use crate::command::WorktreeCommand;
use crate::infra::storage::Storage;
use crate::policy::Role;
use crate::worktree::ProcessWorktreeRunner;
use crate::worktree::WorktreeError;
use crate::worktree::leases::ensure_schema;
use crate::worktree::repo_identity;

mod acquire;
mod prune;
mod query;

// The focused integration tests drive these types and functions
// directly instead of a full CLI round-trip, so the bin target may see
// the re-exports as unused.
#[allow(unused_imports)]
pub(crate) use acquire::AcquireJson;
#[allow(unused_imports)]
pub(crate) use prune::{
    PruneAction, PruneCombinedSummary, PruneDisposition, PruneMode, PruneSummary,
    ReleaseStaleAction, ReleaseStaleSummary, prune_pass, scan_worktree_candidates,
};

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
            acquire::execute_acquire(issue, session.as_deref(), &format, isolate)
        }
        WorktreeCommand::Release {
            lease,
            retain,
            force,
            reason,
        } => {
            if role != Role::Orchestrator {
                return permission_error(role, "worktree release");
            }
            acquire::execute_release(&lease, retain, force, reason)
        }
        WorktreeCommand::Status { issue } => {
            if !is_read_role(role) {
                return permission_error(role, "worktree status");
            }
            query::execute_status(issue)
        }
        WorktreeCommand::List { repo } => {
            if !is_read_role(role) {
                return permission_error(role, "worktree list");
            }
            query::execute_list(repo.as_deref())
        }
        WorktreeCommand::Prune {
            repo,
            stale_days,
            release_stale,
            remove,
            reason,
        } => {
            if role != Role::Orchestrator {
                return permission_error(role, "worktree prune");
            }
            prune::execute_prune(repo.as_deref(), stale_days, release_stale, remove, reason)
        }
        WorktreeCommand::Heartbeat { lease, session } => {
            if role != Role::Orchestrator {
                return permission_error(role, "worktree heartbeat");
            }
            acquire::execute_heartbeat(&lease, session.as_deref())
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
