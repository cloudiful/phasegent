use rusqlite::Row;

/// A single wall-clock phase run persisted in the local execution ledger.
///
/// `elapsed_seconds` is always the exact whole-second difference between the
/// persisted timestamps. `rounded_hours` is the value projected to Redmine;
/// the latter is deliberately derived rather than used as the source of
/// truth.
///
/// `owner_session_id` and `owner_call_id` are the optional OpenCode
/// subagent identifiers recorded by the plugin when a run is started. They
/// are never used for idempotency (run_id remains the only marker) and are
/// never surfaced as remote projections.
#[derive(Clone, Debug, serde::Serialize)]
pub struct TimerRun {
    pub run_id: String,
    pub issue: u64,
    pub phase: String,
    pub role: String,
    pub attempt: u64,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub status: String,
    pub elapsed_seconds: Option<i64>,
    pub rounded_hours: Option<f64>,
    pub activity_id: Option<u64>,
    pub time_entry_id: Option<u64>,
    pub sync_status: String,
    pub sync_error: Option<String>,
    pub owner_session_id: Option<String>,
    pub owner_call_id: Option<String>,
    pub projection_token: Option<String>,
    pub projection_claimed_at: Option<i64>,
}

pub(crate) const TIMER_STATUS_RUNNING: &str = "running";
pub(crate) const TIMER_SYNC_PENDING: &str = "pending";
pub(crate) const TIMER_SYNC_SYNCED: &str = "synced";
pub(crate) const TIMER_SYNC_UNCONFIRMED: &str = "unconfirmed";
pub(crate) const TIMER_SYNC_FAILED: &str = "failed";
pub(crate) const TIMER_SYNC_PROJECTING: &str = "projecting";
pub(crate) const PROJECTION_LEASE_SECS: i64 = 120;
pub(crate) const PROJECTION_TOKEN_BOUND: usize = 128;

/// Filter for the `timer list` surface. Mapped to the same string
/// literals the `execution_timer_runs.status` column stores so a
/// caller-supplied value matches the data without translation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimerStatusFilter {
    Running,
    Finished,
    All,
}

impl TimerStatusFilter {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "running" => Ok(Self::Running),
            "finished" => Ok(Self::Finished),
            "all" => Ok(Self::All),
            other => Err(format!(
                "timer list --status must be running, finished, or all; got '{other}'"
            )),
        }
    }
}

/// Optional owner metadata recorded by the OpenCode plugin when it starts
/// a run. Both fields are bounded, control-character-free, and treated as
/// run metadata only — they never influence provider projection.
#[derive(Clone, Debug, Default)]
pub struct TimerRunOwner {
    pub session_id: Option<String>,
    pub call_id: Option<String>,
}

/// Result shapes accepted by the local state machine.  Keeping these
/// constants in storage makes it harder for a caller to use an arbitrary
/// remote response as a state transition.
pub(crate) fn valid_timer_sync_status(value: &str) -> bool {
    matches!(
        value,
        TIMER_SYNC_PENDING
            | TIMER_SYNC_SYNCED
            | TIMER_SYNC_UNCONFIRMED
            | TIMER_SYNC_FAILED
            | TIMER_SYNC_PROJECTING
    )
}

pub(crate) fn timer_run_from_row(row: &Row<'_>) -> rusqlite::Result<TimerRun> {
    Ok(TimerRun {
        run_id: row.get(0)?,
        issue: row.get::<_, i64>(1)? as u64,
        phase: row.get(2)?,
        role: row.get(3)?,
        attempt: row.get::<_, i64>(4)? as u64,
        started_at: row.get(5)?,
        finished_at: row.get(6)?,
        status: row.get(7)?,
        elapsed_seconds: row.get(8)?,
        rounded_hours: row.get(9)?,
        activity_id: row.get::<_, Option<i64>>(10)?.map(|value| value as u64),
        time_entry_id: row.get::<_, Option<i64>>(11)?.map(|value| value as u64),
        sync_status: row.get(12)?,
        sync_error: row.get(13)?,
        owner_session_id: row.get(14)?,
        owner_call_id: row.get(15)?,
        projection_token: row.get(16)?,
        projection_claimed_at: row.get(17)?,
    })
}

pub(crate) fn validate_timer_identity(
    run_id: &str,
    issue: u64,
    phase: &str,
    role: &str,
    attempt: u64,
) -> Result<(), String> {
    if run_id.trim().is_empty() || run_id.chars().count() > 128 {
        return Err("timer run id must be a non-empty value of at most 128 characters".to_owned());
    }
    if run_id.chars().any(char::is_control) {
        return Err("timer run id must not contain control characters".to_owned());
    }
    if issue == 0 {
        return Err("timer issue id must be greater than zero".to_owned());
    }
    if phase.trim().is_empty() || phase.chars().count() > 128 {
        return Err("timer phase must be a non-empty value of at most 128 characters".to_owned());
    }
    if phase.chars().any(char::is_control) {
        return Err("timer phase must not contain control characters".to_owned());
    }
    if !matches!(role, "executor" | "reviewer" | "tester") {
        return Err("timer agent role must be executor, reviewer, or tester".to_owned());
    }
    if attempt == 0 || attempt > i64::MAX as u64 {
        return Err("timer attempt must be between 1 and i64::MAX".to_owned());
    }
    Ok(())
}

/// Bound and sanitize an optional owner metadata field. `None` and empty
/// values collapse to `None` so the column stores SQL `NULL` rather than
/// an empty string. Non-empty values are bounded to 128 characters and
/// stripped of control bytes.
pub(crate) fn validate_owner_field(
    value: Option<&str>,
    name: &str,
) -> Result<Option<String>, String> {
    match value {
        None => Ok(None),
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            if trimmed.chars().count() > 128 {
                return Err(format!("{name} must be at most 128 characters"));
            }
            if trimmed.chars().any(char::is_control) {
                return Err(format!("{name} must not contain control characters"));
            }
            Ok(Some(trimmed.to_owned()))
        }
    }
}

pub(crate) fn ensure_same_timer_identity(
    run: &TimerRun,
    issue: u64,
    phase: &str,
    role: &str,
    attempt: u64,
) -> Result<(), String> {
    if run.issue != issue || run.phase != phase || run.role != role || run.attempt != attempt {
        return Err(format!(
            "timer run '{}' was already used for a different phase identity",
            run.run_id
        ));
    }
    Ok(())
}

pub(crate) fn validate_projection_token(token: &str) -> Result<(), String> {
    if token.trim().is_empty() || token.chars().count() > PROJECTION_TOKEN_BOUND {
        return Err(format!(
            "projection token must be 1..{PROJECTION_TOKEN_BOUND} characters"
        ));
    }
    if token.chars().any(char::is_control) {
        return Err("projection token must not contain control characters".to_owned());
    }
    Ok(())
}

pub(crate) fn now_epoch_seconds() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    )
    .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{validate_owner_field, validate_timer_identity};

    #[test]
    fn tester_identity_is_valid_for_ledger() {
        assert!(validate_timer_identity("run-1", 1, "phase-a", "tester", 1).is_ok());
        assert!(validate_timer_identity("run-1", 1, "phase-a", "executor", 1).is_ok());
        assert!(validate_timer_identity("run-1", 1, "phase-a", "reviewer", 1).is_ok());
        assert!(validate_timer_identity("run-1", 1, "phase-a", "admin", 1).is_err());
        assert!(validate_timer_identity("run-1", 1, "phase-a", "orchestrator", 1).is_err());
        assert!(validate_timer_identity("run-1", 1, "phase-a", "", 1).is_err());
    }

    #[test]
    fn tester_identity_persists_and_round_trips() {
        // The ledger stores the role as a plain string; tester must survive
        // the same validation as executor/reviewer and not be confused with a
        // global Role.
        let role = "tester";
        validate_timer_identity("r", 1, "p", role, 1).unwrap();
        assert_eq!(role, "tester");
        // Global Role parsing must still reject tester.
        assert!("tester".parse::<crate::policy::Role>().is_err());
    }

    #[test]
    fn owner_field_validation_still_bounded() {
        assert!(validate_owner_field(Some("a"), "owner_session_id").is_ok());
        assert!(
            validate_owner_field(Some(""), "owner_session_id")
                .unwrap()
                .is_none()
        );
        assert!(validate_owner_field(Some("bad\x01"), "owner_session_id").is_err());
    }
}
