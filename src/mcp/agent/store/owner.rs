//! Durable ownership of an explorer run.
//!
//! A run belongs to the OpenCode session that started it. The binding is
//! persisted rather than kept in memory so it survives a phasegent restart:
//! an `interrupted` row is resumable, and a resume must still be refused to
//! anyone but the session that owns it.
//!
//! Ownership lives in its own table rather than as a run column for two
//! reasons. The run writer keeps its identity fields immutable, and a run with
//! no owner row is simply unusable — every status/wait/cancel/resume call
//! resolves the owner first and fails closed — so the binding can never be
//! half-applied and silently grant access.
//!
//! The stored value is server-side only. It is never serialized into a
//! model-visible result, never written to a log line, and never echoed in an
//! error message: a refusal says the run is not available to this session.

use rusqlite::{OptionalExtension, params};

use crate::infra::storage::Storage;

use super::super::store::now_epoch_seconds;

/// Upper bound on an owner session id, mirroring the worktree lease's own
/// session bound so one identity is never spelled two ways.
pub(crate) const MAX_OWNER_SESSION_CHARS: usize = 128;

/// Validate a host-bound owner session id: trimmed, non-empty, bounded, and
/// free of control characters.
pub fn validate_owner_session(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("an explorer run requires a host-bound owner session".to_owned());
    }
    if trimmed.chars().count() > MAX_OWNER_SESSION_CHARS {
        return Err(format!(
            "the owner session must be at most {MAX_OWNER_SESSION_CHARS} characters"
        ));
    }
    if trimmed.chars().any(char::is_control) {
        return Err("the owner session must not contain control characters".to_owned());
    }
    Ok(trimmed.to_owned())
}

/// Whether `session` owns `run_id`. An unknown run and a run owned by another
/// session are both `false`: the caller cannot tell them apart from the result,
/// and neither may act on the run.
pub fn run_is_owned_by(storage: &Storage, run_id: &str, session: &str) -> Result<bool, String> {
    let owner: Option<String> = storage
        .connection
        .query_row(
            "SELECT owner_session_id FROM acp_explorer_run_owners WHERE run_id = ?1",
            params![run_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("could not read explorer run owner: {error}"))?;
    Ok(owner.as_deref() == Some(session))
}

impl Storage {
    /// Record `run_id` as owned by `session`. Called in the same call that
    /// registers the run, so a run whose registration is refused leaves no
    /// owner row behind. Re-binding an existing run to a different session is
    /// refused rather than silently transferred.
    pub fn bind_run_owner(&self, run_id: &str, session: &str) -> Result<(), String> {
        let session = validate_owner_session(session)?;
        if self.load_work_run(run_id)?.is_none() {
            return Err(format!("explorer run '{run_id}' was not found"));
        }
        let existing: Option<String> = self
            .connection
            .query_row(
                "SELECT owner_session_id FROM acp_explorer_run_owners WHERE run_id = ?1",
                params![run_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("could not read explorer run owner: {error}"))?;
        if let Some(current) = existing {
            if current != session {
                return Err(format!(
                    "explorer run '{run_id}' is already owned by another session"
                ));
            }
            return Ok(());
        }
        self.connection
            .execute(
                "INSERT INTO acp_explorer_run_owners \
                    (run_id, owner_session_id, created_at) \
                 VALUES (?1, ?2, ?3)",
                params![run_id, session, now_epoch_seconds()],
            )
            .map_err(|error| format!("could not bind explorer run owner: {error}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::agent::store::RUN_PENDING;
    use crate::mcp::agent::store_tests::open_storage;

    fn seeded() -> Storage {
        let storage = open_storage("owner");
        storage
            .create_work_run("run-a", "/tmp/wt-a", "prompt")
            .expect("seed run");
        storage
    }

    #[test]
    fn owner_session_validation_refuses_blank_overlong_and_control_values() {
        assert_eq!(validate_owner_session(" ses_1 ").unwrap(), "ses_1");
        assert!(validate_owner_session("   ").is_err());
        assert!(validate_owner_session(&"s".repeat(MAX_OWNER_SESSION_CHARS + 1)).is_err());
        assert!(validate_owner_session("ses\n1").is_err());
    }

    #[test]
    fn binding_records_the_owner_and_ownership_is_exact() {
        let storage = seeded();
        storage
            .bind_run_owner("run-a", "ses_owner")
            .expect("bind owner");
        assert!(run_is_owned_by(&storage, "run-a", "ses_owner").unwrap());
        assert!(!run_is_owned_by(&storage, "run-a", "ses_other").unwrap());
        assert!(!run_is_owned_by(&storage, "missing", "ses_owner").unwrap());
    }

    #[test]
    fn binding_is_idempotent_for_the_same_session_and_refuses_a_transfer() {
        let storage = seeded();
        storage.bind_run_owner("run-a", "ses_owner").unwrap();
        storage
            .bind_run_owner("run-a", "ses_owner")
            .expect("re-binding the same owner is idempotent");
        let error = storage
            .bind_run_owner("run-a", "ses_other")
            .expect_err("a run is never transferred");
        assert!(error.contains("already owned"), "{error}");
    }

    #[test]
    fn binding_requires_an_existing_run_and_a_valid_session() {
        let storage = seeded();
        assert!(storage.bind_run_owner("absent", "ses_owner").is_err());
        assert!(storage.bind_run_owner("run-a", "  ").is_err());
        // The refused binds left no owner row, so the run stays unusable.
        assert!(!run_is_owned_by(&storage, "run-a", "ses_owner").unwrap());
    }

    /// An orphaned run row is exactly the shape a crash between the run insert
    /// and the owner bind would leave, and it must be unusable rather than
    /// claimable.
    #[test]
    fn a_run_without_an_owner_row_is_not_owned_by_anyone() {
        let storage = seeded();
        assert_eq!(
            storage.load_work_run("run-a").unwrap().unwrap().status,
            RUN_PENDING
        );
        assert!(!run_is_owned_by(&storage, "run-a", "ses_owner").unwrap());
        assert!(!run_is_owned_by(&storage, "run-a", "").unwrap());
    }
}
