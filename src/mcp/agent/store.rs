//! Durable asynchronous run state for research delegations (issue 685
//! AC 3).
//!
//! One `acp_research_runs` row is written before the ACP process is
//! spawned and updated as the run progresses, so a phasegent restart
//! leaves the run observable: a `running` row with no live in-process
//! handle reads back as `interrupted`. Scratch paths are stored in
//! the server-side database only and are never part of any result the
//! MCP surface returns.

use crate::infra::storage::Storage;
use rusqlite::{OptionalExtension, Row, params};

mod validate;

pub(crate) mod owner;
pub(crate) use validate::now_epoch_seconds;
use validate::validate_cwd;
pub use validate::validate_run_id;

/// Statuses stored in `acp_research_runs.status`.
pub const RUN_PENDING: &str = "pending";
pub const RUN_RUNNING: &str = "running";
pub const RUN_COMPLETED: &str = "completed";
pub const RUN_FAILED: &str = "failed";
pub const RUN_CANCELLED: &str = "cancelled";
pub const RUN_TIMED_OUT: &str = "timed_out";
pub const RUN_INTERRUPTED: &str = "interrupted";

/// Terminal statuses.
pub const TERMINAL_STATUSES: &[&str] = &[
    RUN_COMPLETED,
    RUN_FAILED,
    RUN_CANCELLED,
    RUN_TIMED_OUT,
    RUN_INTERRUPTED,
];

/// Terminal statuses a run may be resumed from. `completed` is
/// excluded: its transcript is the finished answer, and a second turn
/// on the same ACP session would silently replace it.
pub const RESUMABLE_STATUSES: &[&str] =
    &[RUN_FAILED, RUN_CANCELLED, RUN_TIMED_OUT, RUN_INTERRUPTED];

pub(crate) fn valid_run_status(status: &str) -> bool {
    status == RUN_PENDING || status == RUN_RUNNING || TERMINAL_STATUSES.contains(&status)
}

/// One persisted research run. The scratch path is deliberately not
/// derived: [`Debug`] omits it so no log line can carry it.
#[derive(Clone, serde::Serialize)]
pub struct WorkRun {
    pub run_id: String,
    pub status: String,
    /// Server-side only: the ACP process scratch cwd. Never serialized into a
    /// model-visible result.
    #[serde(skip_serializing)]
    pub scratch_cwd: String,
    pub acp_session_id: Option<String>,
    pub prompt: String,
    pub output: Option<String>,
    pub output_truncated: bool,
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub finished_at: Option<i64>,
}

impl std::fmt::Debug for WorkRun {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkRun")
            .field("run_id", &self.run_id)
            .field("status", &self.status)
            .field("acp_session_id", &self.acp_session_id)
            .field("output_truncated", &self.output_truncated)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .field("finished_at", &self.finished_at)
            .finish_non_exhaustive()
    }
}

/// Mutable fields of a run, kept narrow so callers cannot overwrite
/// identity or timestamps directly.
#[derive(Clone, Debug, Default)]
pub struct WorkRunUpdate {
    pub status: Option<String>,
    pub acp_session_id: Option<String>,
    pub output: Option<String>,
    pub output_truncated: Option<bool>,
    pub error: Option<String>,
    pub finished_at: Option<i64>,
}

pub(crate) fn work_run_from_row(row: &Row<'_>) -> rusqlite::Result<WorkRun> {
    Ok(WorkRun {
        run_id: row.get(0)?,
        status: row.get(1)?,
        scratch_cwd: row.get(2)?,
        acp_session_id: row.get(3)?,
        prompt: row.get(4)?,
        output: row.get(5)?,
        output_truncated: row.get::<_, i64>(6)? != 0,
        error: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        finished_at: row.get(10)?,
    })
}

const RUN_COLUMNS: &str = "run_id, status, scratch_cwd, acp_session_id, prompt, output, \
     output_truncated, error, created_at, updated_at, finished_at";

impl Storage {
    /// Persist a new run in `pending` state. Duplicate run ids are
    /// rejected so callers generate ids per attempt.
    pub fn create_work_run(
        &self,
        run_id: &str,
        scratch_cwd: &str,
        prompt: &str,
    ) -> Result<WorkRun, String> {
        validate_run_id(run_id)?;
        validate_cwd(scratch_cwd)?;
        if prompt.trim().is_empty() {
            return Err("research prompt must not be empty".to_owned());
        }
        if prompt.chars().count() > super::types::MAX_TRANSCRIPT_CHARS {
            return Err("research prompt exceeds the transcript bound".to_owned());
        }
        let now = now_epoch_seconds();
        self.connection
            .execute(
                "INSERT INTO acp_research_runs \
                    (run_id, status, scratch_cwd, prompt, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![run_id, RUN_PENDING, scratch_cwd, prompt, now],
            )
            .map_err(|error| {
                if error.to_string().contains("UNIQUE") {
                    format!("run id '{run_id}' already exists")
                } else {
                    format!("could not persist research run start: {error}")
                }
            })?;
        self.load_work_run(run_id)?
            .ok_or_else(|| "research run row disappeared after insert".to_owned())
    }

    pub fn load_work_run(&self, run_id: &str) -> Result<Option<WorkRun>, String> {
        let mut statement = self
            .connection
            .prepare(&format!(
                "SELECT {RUN_COLUMNS} FROM acp_research_runs WHERE run_id = ?1"
            ))
            .map_err(|error| format!("could not prepare research run load: {error}"))?;
        statement
            .query_row(params![run_id], work_run_from_row)
            .optional()
            .map_err(|error| format!("could not read research run: {error}"))
    }

    /// Apply a bounded update to a run. A run already in a terminal
    /// status never changes again except through the explicit
    /// same-status idempotent path.
    pub fn update_work_run(&self, run_id: &str, update: &WorkRunUpdate) -> Result<WorkRun, String> {
        let existing = self
            .load_work_run(run_id)?
            .ok_or_else(|| format!("research run '{run_id}' was not found"))?;
        if let Some(status) = &update.status {
            if !valid_run_status(status) {
                return Err(format!("invalid research run status '{status}'"));
            }
            if TERMINAL_STATUSES.contains(&existing.status.as_str())
                && existing.status != status.as_str()
            {
                return Err(format!(
                    "research run '{run_id}' is already {} and cannot move to {status}",
                    existing.status
                ));
            }
        }
        if let Some(output) = &update.output
            && output.chars().count() > super::types::MAX_TRANSCRIPT_CHARS
        {
            return Err("research run output exceeds the transcript bound".to_owned());
        }
        let now = now_epoch_seconds();
        self.connection
            .execute(
                "UPDATE acp_research_runs SET \
                    status = COALESCE(?2, status), \
                    acp_session_id = COALESCE(?3, acp_session_id), \
                    output = COALESCE(?4, output), \
                    output_truncated = COALESCE(?5, output_truncated), \
                    error = COALESCE(?6, error), \
                    finished_at = COALESCE(?7, finished_at), \
                    updated_at = ?8 \
                 WHERE run_id = ?1",
                params![
                    run_id,
                    update.status,
                    update.acp_session_id,
                    update.output,
                    update.output_truncated.map(|value| value as i64),
                    update.error,
                    update.finished_at,
                    now,
                ],
            )
            .map_err(|error| format!("could not update research run: {error}"))?;
        self.load_work_run(run_id)?
            .ok_or_else(|| "research run row disappeared after update".to_owned())
    }

    /// Reopen a resumable run so a new process can continue its ACP
    /// session, in a fresh server-created scratch directory. This is the
    /// single, explicit exception to the terminal freeze: the run keeps its
    /// id, its prompt, its partial output, and its persisted ACP session id;
    /// only the terminal markers are cleared and the scratch cwd is replaced.
    /// A run with no ACP session, a `completed` run, or a run that is not
    /// resumable is refused.
    pub fn resume_work_run(&self, run_id: &str, scratch_cwd: &str) -> Result<WorkRun, String> {
        let existing = self
            .load_work_run(run_id)?
            .ok_or_else(|| format!("research run '{run_id}' was not found"))?;
        if existing.acp_session_id.as_deref().is_none_or(str::is_empty) {
            return Err(format!(
                "research run '{run_id}' has no ACP session to resume"
            ));
        }
        if !RESUMABLE_STATUSES.contains(&existing.status.as_str()) {
            return Err(format!(
                "research run '{run_id}' is {} and cannot be resumed",
                existing.status
            ));
        }
        validate_cwd(scratch_cwd)?;
        self.connection
            .execute(
                "UPDATE acp_research_runs SET \
                     status = ?2, error = NULL, finished_at = NULL, scratch_cwd = ?4, \
                     updated_at = ?3 \
                 WHERE run_id = ?1",
                params![run_id, RUN_PENDING, now_epoch_seconds(), scratch_cwd],
            )
            .map_err(|error| format!("could not resume research run: {error}"))?;
        self.load_work_run(run_id)?
            .ok_or_else(|| "research run row disappeared after resume".to_owned())
    }

    /// List runs, newest first. `running_only` selects the recovery
    /// scan set; the cap keeps the result bounded.
    pub fn list_work_runs(&self, running_only: bool, limit: u32) -> Result<Vec<WorkRun>, String> {
        let clamped = limit.clamp(1, 1_000);
        let sql = if running_only {
            format!(
                "SELECT {RUN_COLUMNS} FROM acp_research_runs \
                 WHERE status IN ('{RUN_PENDING}', '{RUN_RUNNING}') \
                 ORDER BY created_at DESC LIMIT ?1"
            )
        } else {
            format!(
                "SELECT {RUN_COLUMNS} FROM acp_research_runs \
                 ORDER BY created_at DESC LIMIT ?1"
            )
        };
        let mut statement = self
            .connection
            .prepare(&sql)
            .map_err(|error| format!("could not prepare research run list: {error}"))?;
        let rows = statement
            .query_map(params![clamped as i64], work_run_from_row)
            .map_err(|error| format!("could not read research run list: {error}"))?;
        let mut runs = Vec::new();
        for row in rows {
            runs.push(row.map_err(|error| format!("could not decode research run row: {error}"))?);
        }
        Ok(runs)
    }
}
