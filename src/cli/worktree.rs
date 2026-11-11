//! Executor for the `worktree` command group (issue #239 Phase 2).
//!
//! Subcommand gating lives here (command-level, not capability-level)
//! so `acquire` / `prune` are orchestrator-only while `status` / `list` /
//! `probe` mirror the issue-status read surface (orchestrator, executor,
//! reviewer). Branch deletion is never invoked; only `git worktree remove`
//! is used on clean candidates. `.env` and secret material are never read,
//! copied, or written by any code path here (issue #239 Decisions).
//!
//! The subcommand executors are split by concern (issue 337 Phase 2):
//!
//! * [`acquire`] — `acquire`.
//! * [`query`] — read-only `status` / `list`.
//! * [`probe`] — read-only `probe` diagnostics (issue 595).
//! * [`prune`] — `prune` and its candidate-scan / action-apply model.
//!
//! Mutating subcommands route through the directory registry and the
//! `prune` execution model. The registry is
//! created lazily through `ensure_schema` so the executor can run
//! against a pre-Phase-1 database without a migration step.

use std::path::PathBuf;

use crate::command::WorktreeCommand;
use crate::infra::storage::Storage;
use crate::policy::{CallerContext, Role};
use crate::worktree::ProcessWorktreeRunner;
use crate::worktree::WorktreeError;
use crate::worktree::repo_identity;

pub(crate) mod acquire;
mod probe;
mod prune;
pub(crate) mod query;

// The focused integration tests drive these types and functions
// directly instead of a full CLI round-trip, so the bin target may see
// the re-exports as unused.
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use crate::cli::issue_closure::test_support::ScriptedIssueStateSource;
#[allow(unused_imports)]
pub(crate) use acquire::AcquireJson;
#[allow(unused_imports)]
pub(crate) use probe::{ProbeJson, build_probe};
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use prune::scan_worktree_candidates;
#[allow(unused_imports)]
pub(crate) use prune::{
    DirectoryCandidate, PruneAction, PruneAggregate, PruneDisposition, PruneMode, PruneRepoOutcome,
    PruneScope, PruneSummary, PruneTotals, execute_prune_with_source, prune_pass,
    prune_pass_with_policy, render_text,
};

#[allow(dead_code)]
pub(crate) fn execute_worktree(role_value: Option<Role>, command: WorktreeCommand) -> i32 {
    let caller = CallerContext::from_role(role_value);
    execute_worktree_with_caller(caller, command)
}

pub(crate) fn execute_worktree_with_caller(caller: CallerContext, command: WorktreeCommand) -> i32 {
    execute_worktree_scoped(
        caller,
        &crate::cli::provider_context::ProviderContext::default(),
        command,
    )
}

fn validate_context_for_worktree(
    context: &crate::cli::provider_context::ProviderContext,
    credential: Role,
    command: &WorktreeCommand,
) -> Result<(), crate::providers::api::PhasegentError> {
    match command {
        WorktreeCommand::Acquire { .. }
        | WorktreeCommand::Prune { .. }
        | WorktreeCommand::List { .. } => context.validate(credential),
        _ => Ok(()),
    }
}

/// Validate the raw provider flags an acquisition can never honour.
///
/// The close-status id is parsed by the ordinary pinned Redmine route, so a
/// non-numeric, zero or out-of-range value is rejected there. The provider
/// selections are resolved by the ordinary numbered route, so a blank one is
/// rejected there as well. An acquisition resolves only identity, so it
/// validates the raw values itself: bound and legacy numbers alike refuse
/// before the taxi pass or any registry write, independent of the resolved
/// default provider. The Forgejo prohibition on the flag stays with the
/// provider context and is unchanged.
fn validate_acquire_flags(
    context: &crate::cli::provider_context::ProviderContext,
) -> Result<(), crate::providers::api::PhasegentError> {
    use crate::providers::api::PhasegentError;
    if let Some(value) = context.close_status_id.as_deref() {
        let id = value
            .parse::<u64>()
            .map_err(|_| PhasegentError::config("Redmine close status id must be numeric"))?;
        if id == 0 {
            return Err(PhasegentError::config(
                "Redmine close status id must be greater than zero",
            ));
        }
    }
    if let Some(value) = context.project_id.as_deref()
        && value.trim().is_empty()
    {
        return Err(PhasegentError::config("Redmine project id cannot be empty"));
    }
    if let Some(value) = context.api_base.as_deref()
        && value.trim().is_empty()
    {
        return Err(PhasegentError::config("provider endpoint cannot be empty"));
    }
    if let Some(value) = context.repository.as_deref()
        && value.trim().is_empty()
    {
        return Err(PhasegentError::config(
            "provider repository cannot be empty",
        ));
    }
    Ok(())
}

pub(crate) fn execute_worktree_scoped(
    caller: CallerContext,
    context: &crate::cli::provider_context::ProviderContext,
    command: WorktreeCommand,
) -> i32 {
    // Managed sessions without a role fail closed before any storage or
    // reconciliation access.
    if caller.is_managed_without_role() {
        let operation = match &command {
            WorktreeCommand::Acquire { .. } => "worktree acquire",
            WorktreeCommand::Status { .. } => "worktree status",
            WorktreeCommand::List { .. } => "worktree list",
            WorktreeCommand::Probe { .. } => "worktree probe",
            WorktreeCommand::Prune { .. } => "worktree prune",
        };
        return super::managed_denial(operation);
    }
    // Issue 552 Phase 2: `acquire` / `list` / `prune` run one
    // reconciliation pass for the repository they operate on (`--repo`
    // when supplied, otherwise the current directory) first. The pass
    // never writes stdout and never blocks: a failure is a bounded stderr
    // warning, and `--no-sync` skips it. The pass is orchestrator-authorized
    // (Operator shares it; like `issue sync` and the mutating worktree
    // subcommands), so a read-only role can never trigger a registry write or
    // a removal by inspecting directories.
    let credential = caller
        .credential_role()
        .expect("authorized caller has a credential");
    // A malformed raw close-status or provider selection is refused for
    // bound and legacy numbers alike, before the task resolution, the
    // complete raw-context validation, the taxi reconciliation or any registry
    // write.
    if matches!(&command, WorktreeCommand::Acquire { .. })
        && let Err(error) = validate_acquire_flags(context)
    {
        return crate::cli::provider_error(error);
    }
    // A durable task owns its acquisition. Resolve it strictly read-only with
    // the same branch-reference then unique-membership rules and the same raw
    // explicit filters as the ordinary numbered route, before any
    // default-provider admission, taxi reconciliation or registry write. A
    // known task is authoritative, so a changed or implicit default never
    // rejects or re-points it; every other command and number keeps the
    // complete raw-context validation below.
    let acquire_task = match &command {
        WorktreeCommand::Acquire { issue, .. } => {
            match crate::cli::task_route::resolve_number_task(context, *issue) {
                Ok(task) => task,
                Err(error) => return crate::cli::provider_error(error),
            }
        }
        _ => None,
    };
    // Validate the complete raw context before taxi reconciliation or any
    // acquisition/recovery mutation, even when the taxi has no candidates or
    // the command is a supported --no-sync form.
    if acquire_task.is_none()
        && let Err(error) = validate_context_for_worktree(context, credential, &command)
    {
        return crate::cli::provider_error(error);
    }
    let runner = ProcessWorktreeRunner::new();
    let mut sync_setup = None;
    if caller.is_orchestrator_authorized()
        && let Some((operation, repo)) = command.sync_taxi()
    {
        let (warnings, setup) =
            crate::cli::issue::sync::taxi_sync_scoped(credential, context, repo, &runner);
        sync_setup = setup;
        for warning in warnings {
            super::report_local_warnings(operation, Some(warning));
        }
    }
    match command {
        WorktreeCommand::Acquire {
            issue,
            session,
            base,
            format,
            isolate,
            no_sync: _,
        } => {
            if !caller.is_orchestrator_authorized() {
                return permission_error(credential, "worktree acquire");
            }
            acquire::execute_acquire_scoped(
                issue,
                session.as_deref(),
                &format,
                isolate,
                base.as_deref(),
                sync_setup.as_ref(),
                context,
                &runner,
                acquire_task.as_ref(),
            )
        }
        WorktreeCommand::Status { issue, page } => {
            if !caller.is_worktree_read_authorized() {
                return permission_error(credential, "worktree status");
            }
            query::execute_status_page(issue, page)
        }
        WorktreeCommand::List {
            repo,
            no_sync: _,
            page,
        } => {
            if !caller.is_worktree_read_authorized() {
                return permission_error(credential, "worktree list");
            }
            query::execute_list_page(repo.as_deref(), sync_setup.as_ref(), &runner, page)
        }
        WorktreeCommand::Probe {
            path,
            issue,
            session,
        } => {
            if !caller.is_worktree_read_authorized() {
                return permission_error(credential, "worktree probe");
            }
            probe::execute_probe(path.as_deref(), issue, session.as_deref())
        }
        WorktreeCommand::Prune {
            repo,
            // Accepted compatibility no-op: removal is the default now, driven
            // by `dry_run`.
            remove: _,
            dry_run,
            format,
            verbose,
            no_sync: _,
        } => {
            if !caller.is_orchestrator_authorized() {
                return permission_error(credential, "worktree prune");
            }
            prune::execute_prune(
                repo.as_deref(),
                dry_run,
                &format,
                verbose,
                credential,
                context,
            )
        }
    }
}

#[allow(dead_code)]
fn is_read_role(role: Role) -> bool {
    matches!(role, Role::Orchestrator | Role::Executor | Role::Reviewer)
}

/// Shared structured `permission` envelope for command-level role gates.
/// `sync` reuses it so `issue sync` and the `worktree` write paths stay
/// byte-identical for the same denial.
pub(crate) fn permission_error(role: Role, operation: &str) -> i32 {
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
    repo: Option<&str>,
    runner: &dyn crate::worktree::WorktreeRunner,
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
    match repo_identity(runner, &repo_path) {
        Ok(identity) => Ok(identity),
        Err(error) => Err(serde_json::json!({
            "kind": error.kind,
            "message": error.message,
        })),
    }
}
