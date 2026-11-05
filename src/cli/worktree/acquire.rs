//! Executors for the mutating `worktree` subcommands owned by the
//! orchestrator: `acquire`, `release`, and `heartbeat`.
//!
//! Each executor maps a domain outcome to its stable JSON envelope and
//! keeps the structured-error vocabulary (`kind`, `message`) that the
//! parser-level tests assert on. The role gate lives in the parent
//! module's dispatcher, not here.

use std::path::PathBuf;

use serde::Serialize;

use crate::cli::{print_json, structured_error};
use crate::worktree::{
    AcquireOutcome, ProcessWorktreeRunner, ReleaseOutcome, heartbeat_lease, now_unix_secs,
    resolve_session,
};

/// One row of the JSON envelope returned by `acquire`. Field order
/// is stable for downstream parsing.
#[derive(Debug, Serialize)]
pub(crate) struct AcquireJson {
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

#[derive(Debug, Serialize)]
pub(crate) struct ReleaseJson {
    pub lease_id: String,
    pub status: String,
    pub forced: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl From<ReleaseOutcome> for ReleaseJson {
    fn from(outcome: ReleaseOutcome) -> Self {
        Self {
            lease_id: outcome.lease_id,
            status: outcome.status,
            forced: outcome.forced,
            reason: outcome.reason,
        }
    }
}

/// Envelope returned by `heartbeat`. Only the fields an operator needs to
/// confirm ownership and freshness are surfaced.
#[derive(Debug, Serialize)]
pub(crate) struct HeartbeatJson {
    pub lease_id: String,
    pub issue: u64,
    pub session: String,
    pub status: String,
    pub heartbeat_at: i64,
}

impl From<crate::worktree::LeaseRow> for HeartbeatJson {
    fn from(row: crate::worktree::LeaseRow) -> Self {
        Self {
            lease_id: row.lease_id,
            issue: row.issue,
            session: row.session,
            status: row.status,
            heartbeat_at: row.heartbeat_at,
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

/// The explicit-provider acquire: a `--provider forgejo` invocation leases
/// under the exact `(forgejo, owner/repo, issue)` sidecar key, so the
/// acquisition is isolated from a same-number legacy lease. Any other
/// provider selection keeps the legacy numeric path exactly as before.
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
) -> i32 {
    let repo_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if context.provider == Some(crate::providers::ProviderKind::Forgejo) {
        let repository = match context.owned_forgejo_repository(crate::policy::Role::Orchestrator) {
            Ok(Some(repository)) => repository,
            Ok(None) => {
                return structured_error(
                    serde_json::json!({"kind": "argument", "message": "Forgejo scope could not be resolved"}),
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
            Some(&repository),
        )
    } else {
        execute_acquire_at(
            issue, session, format, isolate, base, setup, runner, &repo_path,
        )
    }
}

/// The Forgejo-scoped acquire: the lease carries the exact
/// `(forgejo, owner/repo, issue)` sidecar key. The owner/repo is the
/// validated explicit provider config (explicit args first, then the
/// provider-owned stored row) — never the legacy generic StoredConfig
/// fields — and the acquisition fails closed when the scope cannot be
/// proven, so a same-number legacy lease is never guessed over.
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
    repository: Option<&str>,
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
    let repository = match repository {
        Some(repository) => repository.to_owned(),
        None => {
            return structured_error(
                serde_json::json!({"kind": "argument", "message": "Forgejo scope could not be resolved"}),
                1,
            );
        }
    };
    let scope = match crate::worktree::LeaseScope::new("forgejo", &repository, issue) {
        Ok(scope) => scope,
        Err(error) => {
            return structured_error(
                serde_json::json!({"kind": error.kind, "message": error.message}),
                1,
            );
        }
    };
    crate::output::progress::stage("worktree acquire: preparing the scoped worktree");
    let options = crate::worktree::AcquireOptions {
        cache_base: None,
        isolate,
        base,
        reuse: false,
    };
    let outcome = if let Some(setup) = setup
        && let Some(repo_scope) = setup.scopes.first()
    {
        crate::worktree::acquire_lease_scoped(
            runner,
            &setup.storage,
            &repo_scope.repo_identity,
            repo_path,
            &session.id,
            options,
            &scope,
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
        if let Err(error) = crate::worktree::leases::ensure_schema(&storage) {
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
        crate::worktree::acquire_lease_scoped(
            runner,
            &storage,
            &identity,
            repo_path,
            &session.id,
            options,
            &scope,
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
    let options = crate::worktree::AcquireOptions {
        cache_base: None,
        isolate,
        base,
        // The explicit `worktree acquire` command keeps the default
        // conflict isolation: an occupied checkout path creates a
        // dedicated worktree. `--reuse` states that default
        // explicitly at the parser layer (it is mutually exclusive
        // with `--isolate` there) and needs no separate gate here
        // because the default table never bypasses an active lease.
        reuse: false,
    };
    let outcome = if let Some(setup) = setup
        && let Some(scope) = setup.scopes.first()
    {
        crate::worktree::leases::acquire_lease_seeded(
            runner,
            &setup.storage,
            &scope.repo_identity,
            repo_path,
            issue,
            &session.id,
            options,
        )
    } else {
        crate::worktree::acquire_lease_with(runner, repo_path, issue, &session.id, options)
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

pub(super) fn execute_release(
    lease: &str,
    retain: bool,
    force: bool,
    reason: Option<String>,
) -> i32 {
    let outcome = if force {
        // `--reason` is guaranteed non-empty by the parser when
        // `--force` is set; re-check defensively so a future caller
        // cannot persist an empty justification.
        match reason
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            Some(justification) => {
                crate::worktree::release_lease_forced(lease, retain, justification)
            }
            None => {
                return structured_error(
                    serde_json::json!({
                        "kind":"argument",
                        "operation":"worktree release",
                        "message":"worktree release --force requires a non-empty --reason"
                    }),
                    2,
                );
            }
        }
    } else {
        crate::worktree::release_lease(lease, retain)
    };
    match outcome {
        Ok(outcome) => {
            let payload = ReleaseJson::from(outcome);
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

pub(super) fn execute_heartbeat(lease: &str, session: Option<&str>) -> i32 {
    // Heartbeat extends a lease the caller owns, so it needs the same
    // explicit identity as acquire: a missing identity fails fast with
    // exit 2 instead of refreshing under a fabricated shared owner.
    let session = match resolve_session(session) {
        Ok(context) => context,
        Err(error) => {
            return structured_error(
                serde_json::json!({
                    "kind": error.kind,
                    "operation": "worktree heartbeat",
                    "message": error.message,
                }),
                2,
            );
        }
    };
    match heartbeat_lease(lease, &session.id, now_unix_secs()) {
        Ok(row) => print_json(&HeartbeatJson::from(row)),
        Err(error) => structured_error(
            serde_json::json!({
                "kind": error.kind,
                "operation": "worktree heartbeat",
                "message": error.message,
            }),
            1,
        ),
    }
}
