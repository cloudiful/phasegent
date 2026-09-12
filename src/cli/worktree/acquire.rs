//! Executors for the mutating `worktree` subcommands owned by the
//! orchestrator: `acquire`, `release`, and `heartbeat`.
//!
//! Each executor maps a domain outcome to its stable JSON envelope and
//! keeps the structured-error vocabulary (`kind`, `message`) that the
//! parser-level tests assert on. The role gate lives in the parent
//! module's dispatcher, not here.

use std::path::PathBuf;

use serde::Serialize;

use crate::cli::{print_json, report_local_warnings, structured_error};
use crate::worktree::{
    AcquireOutcome, ProcessWorktreeRunner, ReleaseOutcome, heartbeat_lease, now_unix_secs,
    resolve_session, resolve_worktree_auto,
};

use super::open_storage;

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

/// One row of the JSON envelope returned by `release`.
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

pub(super) fn execute_acquire(
    issue: u64,
    session: Option<&str>,
    format: &str,
    isolate: bool,
) -> i32 {
    let _ = format; // only "json" is accepted at the parser layer
    // Resolve the session before any storage / git work so a blank or
    // overlong `PHASEGENT_SESSION_ID` fails fast with exit 2. The legacy
    // fallback emits a migration warning on stderr only; the stdout JSON
    // envelope is unchanged (issue 305 Task 1).
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
    report_local_warnings("worktree acquire", session.legacy_warning());
    let repo_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let runner = ProcessWorktreeRunner::new();
    // Resolve the persisted `worktree-auto` switch (env over SQLite,
    // default false) so `--isolate` and the setting share one gate.
    let storage = match open_storage() {
        Ok(storage) => storage,
        Err(message) => return super::config_error(&message),
    };
    let auto = match resolve_worktree_auto(&storage) {
        Ok(auto) => auto,
        Err(error) => {
            return structured_error(
                serde_json::json!({
                    "kind": error.kind,
                    "message": error.message,
                }),
                1,
            );
        }
    };
    match crate::worktree::acquire_lease(
        &runner,
        &repo_path,
        issue,
        &session.id,
        None,
        isolate,
        auto,
    ) {
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
    report_local_warnings("worktree heartbeat", session.legacy_warning());
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
