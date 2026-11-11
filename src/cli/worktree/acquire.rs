//! Executor for the mutating `worktree acquire` subcommand owned by the
//! orchestrator.
//!
//! The executor maps a directory outcome to its stable JSON envelope and
//! keeps the structured-error vocabulary (`kind`, `message`) that the
//! parser-level tests assert on. The role gate lives in the parent
//! module's dispatcher, not here.

use std::path::PathBuf;

use serde::Serialize;

use crate::cli::{print_json, structured_error};
use crate::worktree::{AcquireOutcome, ProcessWorktreeRunner, resolve_session};

/// One row of the JSON envelope returned by `acquire`. Field order
/// is stable for downstream parsing.
#[derive(Debug, Serialize)]
pub(crate) struct AcquireJson {
    pub directory_id: String,
    pub path: String,
    pub branch: String,
    pub repo_identity: String,
    pub created: bool,
    pub reason: String,
}

impl From<AcquireOutcome> for AcquireJson {
    fn from(outcome: AcquireOutcome) -> Self {
        Self {
            directory_id: outcome.directory_id,
            path: outcome.path,
            branch: outcome.branch,
            repo_identity: outcome.repo_identity,
            created: outcome.created,
            reason: outcome.reason,
        }
    }
}

#[allow(dead_code)]
pub(super) fn execute_acquire(
    issue: u64,
    session: Option<&str>,
    format: &str,
    isolate: bool,
    base: Option<&str>,
) -> i32 {
    execute_acquire_seeded(
        issue,
        session,
        format,
        isolate,
        base,
        None,
        &ProcessWorktreeRunner::new(),
    )
}

pub(super) fn execute_acquire_seeded(
    issue: u64,
    session: Option<&str>,
    format: &str,
    isolate: bool,
    base: Option<&str>,
    setup: Option<&crate::cli::issue::sync::SyncSetup>,
    runner: &dyn crate::worktree::WorktreeRunner,
) -> i32 {
    let repo_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    execute_acquire_at(
        issue, session, format, isolate, base, setup, runner, &repo_path,
    )
}

/// The scoped acquire dispatcher.
///
/// A number that resolved to a durable task in this repository is acquired
/// under that exact task scope for every provider, so a changed default never
/// changes what the directory scope names. Without a task, an explicit `--provider
/// forgejo` keeps the explicit `(forgejo, owner/repo, issue)` sidecar scope,
/// and every other selection keeps the legacy numeric path exactly as before.
#[allow(clippy::too_many_arguments)]
pub(super) fn execute_acquire_scoped(
    issue: u64,
    session: Option<&str>,
    format: &str,
    isolate: bool,
    base: Option<&str>,
    setup: Option<&crate::cli::issue::sync::SyncSetup>,
    context: &crate::cli::provider_context::ProviderContext,
    runner: &dyn crate::worktree::WorktreeRunner,
    task: Option<&crate::task_identity::TaskBinding>,
) -> i32 {
    let repo_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let occupancy = crate::cli::issue_closure::ClosureOccupancy::task_aware(
        crate::policy::Role::Orchestrator,
        context.clone(),
    );
    if let Some(binding) = task {
        return execute_acquire_at_scoped(
            issue,
            session,
            format,
            isolate,
            base,
            setup,
            runner,
            &repo_path,
            Some(binding),
            None,
            Some(&occupancy),
        );
    }
    // A provider without legacy numeric leases always records its exact
    // resolved scope; a legacy-capable provider (Redmine, Local) keeps the
    // unscoped numeric path. The decision is a capability, never a kind or
    // selection-string comparison.
    let legacy_numeric_leases = match context.capabilities(crate::policy::Role::Orchestrator) {
        Ok(capabilities) => capabilities.legacy_numeric_leases,
        Err(error) => return structured_error(error.json(), 1),
    };
    if !legacy_numeric_leases {
        let scope = match context.resolved_scope(crate::policy::Role::Orchestrator) {
            Ok(Some(scope)) => scope,
            Ok(None) => {
                return structured_error(
                    serde_json::json!({"kind": "argument", "message": "the selected provider scope could not be resolved"}),
                    1,
                );
            }
            Err(error) => return structured_error(error.json(), 1),
        };
        execute_acquire_at_scoped(
            issue,
            session,
            format,
            isolate,
            base,
            setup,
            runner,
            &repo_path,
            None,
            Some(&scope),
            Some(&occupancy),
        )
    } else {
        execute_acquire_at(
            issue, session, format, isolate, base, setup, runner, &repo_path,
        )
    }
}

/// The scoped acquire: either a durable task owns the directory, or an explicit
/// Forgejo selection does.
///
/// A task scope records the exact task identity, so a same-number task on
/// another instance is a different scope and is never reused as this one. The
/// legacy Forgejo scope keeps the validated explicit provider config (explicit
/// args first, then the provider-owned stored row) and fails closed when the
/// scope cannot be proven, so a same-number legacy lease is never guessed
/// over. An explicit selection that contradicts the task was already refused
/// by admission, before this executor or the taxi runs.
#[allow(clippy::too_many_arguments)]
fn execute_acquire_at_scoped(
    issue: u64,
    session: Option<&str>,
    format: &str,
    isolate: bool,
    base: Option<&str>,
    setup: Option<&crate::cli::issue::sync::SyncSetup>,
    runner: &dyn crate::worktree::WorktreeRunner,
    repo_path: &std::path::Path,
    task: Option<&crate::task_identity::TaskBinding>,
    scope: Option<&crate::cli::provider_context::ResolvedScope>,
    foreign_state: Option<&dyn crate::worktree::occupancy::ForeignIssueState>,
) -> i32 {
    let _ = format; // only "json" is accepted at the parser layer
    let session = match resolve_session(session) {
        Ok(context) => context,
        Err(error) => {
            return structured_error(
                serde_json::json!({
                    "kind": error.kind,
                    "operation": "worktree acquire",
                    "message": error.message,
                }),
                2,
            );
        }
    };
    let scope = match task {
        Some(binding) => {
            let key = binding.key();
            if key.external_number() != Some(issue) {
                return structured_error(
                    serde_json::json!({
                        "kind": "argument",
                        "message": format!(
                            "the durable task was resolved for number {}, not {issue}",
                            key.external_number().unwrap_or(0),
                        ),
                    }),
                    1,
                );
            }
            match crate::worktree::WorktreeScope::for_task(
                key.provider(),
                key.project(),
                issue,
                binding.task_id(),
                key.instance(),
            ) {
                Ok(scope) => scope,
                Err(error) => {
                    return structured_error(
                        serde_json::json!({"kind": error.kind, "message": error.message}),
                        1,
                    );
                }
            }
        }
        None => {
            let resolved = match scope {
                Some(scope) => scope,
                None => {
                    return structured_error(
                        serde_json::json!({"kind": "argument", "message": "the selected provider scope could not be resolved"}),
                        1,
                    );
                }
            };
            match crate::worktree::WorktreeScope::new(
                resolved.provider.as_str(),
                &resolved.project,
                issue,
            ) {
                Ok(scope) => scope,
                Err(error) => {
                    return structured_error(
                        serde_json::json!({"kind": error.kind, "message": error.message}),
                        1,
                    );
                }
            }
        }
    };
    crate::output::progress::stage("worktree acquire: preparing the scoped worktree");
    let host_api = crate::worktree::opencode::ProcessOpenCodeApi::new();
    let options = crate::worktree::AcquireOptions {
        cache_base: None,
        isolate,
        base,
        reuse: false,
    };
    let outcome = if let Some(setup) = setup
        && let Some(repo_scope) = setup.scopes.first()
    {
        crate::worktree::acquire_directory_scoped(
            runner,
            &setup.storage,
            &repo_scope.repo_identity,
            repo_path,
            &session.id,
            options,
            &scope,
            foreign_state,
            Some(&host_api),
        )
    } else {
        let storage = match crate::infra::storage::Storage::open() {
            Ok(storage) => storage,
            Err(error) => {
                return structured_error(
                    serde_json::json!({"kind": "storage", "message": error}),
                    1,
                );
            }
        };
        if let Err(error) = crate::worktree::ensure_schema(&storage) {
            return structured_error(serde_json::json!({"kind": "storage", "message": error}), 1);
        }
        let identity = match crate::worktree::repo_identity(runner, repo_path) {
            Ok(identity) => identity,
            Err(error) => {
                return structured_error(
                    serde_json::json!({"kind": error.kind, "message": error.message}),
                    1,
                );
            }
        };
        crate::worktree::acquire_directory_scoped(
            runner,
            &storage,
            &identity,
            repo_path,
            &session.id,
            options,
            &scope,
            foreign_state,
            Some(&host_api),
        )
    };
    match outcome {
        Ok(outcome) => {
            let payload = AcquireJson::from(outcome);
            print_json(&payload)
        }
        Err(error) => structured_error(
            serde_json::json!({
                "kind": error.kind,
                "message": error.message,
            }),
            1,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_acquire_at(
    issue: u64,
    session: Option<&str>,
    format: &str,
    isolate: bool,
    base: Option<&str>,
    setup: Option<&crate::cli::issue::sync::SyncSetup>,
    runner: &dyn crate::worktree::WorktreeRunner,
    repo_path: &std::path::Path,
) -> i32 {
    let _ = format; // only "json" is accepted at the parser layer
    // Resolve the session before any storage / git work so a missing,
    // blank, or overlong identity fails fast with exit 2 and a clear
    // message naming `--session` / `PHASEGENT_SESSION_ID` (issue 651
    // P4). There is no fabricated fallback owner: unrelated callers
    // must never collapse onto one shared session.
    let session = match resolve_session(session) {
        Ok(context) => context,
        Err(error) => {
            return structured_error(
                serde_json::json!({
                    "kind": error.kind,
                    "operation": "worktree acquire",
                    "message": error.message,
                }),
                2,
            );
        }
    };
    crate::output::progress::stage("worktree acquire: preparing the worktree");
    let occupancy = crate::cli::issue_closure::ClosureOccupancy::task_aware(
        crate::policy::Role::Orchestrator,
        crate::cli::provider_context::ProviderContext::default(),
    );
    let host_api = crate::worktree::opencode::ProcessOpenCodeApi::new();
    let options = crate::worktree::AcquireOptions {
        cache_base: None,
        isolate,
        base,
        // The explicit `worktree acquire` command keeps the default
        // conflict isolation: an occupied checkout path creates a
        // dedicated worktree. `--reuse` states that default
        // explicitly at the parser layer (it is mutually exclusive
        // with `--isolate` there) and needs no separate gate here
        // because the default decision table never bypasses an unresolved
        // occupation.
        reuse: false,
    };
    let outcome = if let Some(setup) = setup
        && let Some(scope) = setup.scopes.first()
    {
        crate::worktree::acquire_directory_seeded(
            runner,
            &setup.storage,
            &scope.repo_identity,
            repo_path,
            issue,
            &session.id,
            options,
            Some(&occupancy),
            Some(&host_api),
        )
    } else {
        crate::worktree::acquire_directory_with_foreign_state(
            runner,
            repo_path,
            issue,
            &session.id,
            options,
            Some(&occupancy),
            Some(&host_api),
        )
    };
    match outcome {
        Ok(outcome) => {
            let payload = AcquireJson::from(outcome);
            print_json(&payload)
        }
        Err(error) => structured_error(
            serde_json::json!({
                "kind": error.kind,
                "message": error.message,
            }),
            1,
        ),
    }
}
