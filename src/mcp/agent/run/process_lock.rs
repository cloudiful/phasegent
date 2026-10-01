//! Per-database, kernel-held exclusion between phasegent processes (issue 685
//! P2).
//!
//! The research run ledger is one durable table in one SQLite database, and a
//! recovery pass reads it as a whole: every non-terminal row without a live
//! entry in *this* process's flight registry is marked `interrupted`. That
//! judgement is only sound for the process that owns the database. Without an
//! owner, a second `phasegent mcp serve` over the same file would read the
//! first process's live `running` row, find no local flight for it, and mark it
//! interrupted — after which `resume_work_run` would happily reopen the row and
//! spawn a second ACP process for a session that is still alive.
//!
//! This module supplies the owner. [`ProcessLock::acquire`] takes an exclusive
//! advisory lock on a file beside the database, and the [`ProcessLock`] is held
//! for the run manager's whole lifetime, so:
//!
//! * only the owning process ever reaches recovery, and
//! * a competing process is refused outright — no row is read for recovery and
//!   no ACP process is spawned.
//!
//! The lock is advisory and kernel-owned, not a flag this crate maintains.
//! That is the whole point: the kernel drops it when the owning process exits,
//! however it exits, so a successor can acquire it without any stale-owner
//! cleanup and then recover the rows its predecessor left active. A pid or
//! heartbeat column could not make that guarantee — a crashed process leaves
//! both behind, and a live process's heartbeat can always be stale.
//!
//! The lock file lives in the database's own directory, which `Storage::open_at`
//! has already created owner-only (0700), so the file carries no data worth
//! tightening further: it is an empty rendezvous, and its name is the only
//! thing two processes need to agree on.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use fs2::FileExt;

/// The lock file this module owns, beside the database it guards.
pub(crate) const LOCK_FILE_NAME: &str = "acp-research-runs.lock";

/// Why the ledger could not be claimed. Every variant is a refusal, and a
/// refused claim never yields a [`ProcessLock`], so recovery and spawning are
/// both unreachable — the failure is closed by construction rather than by a
/// caller remembering to check.
#[derive(Debug)]
pub(crate) enum ProcessLockError {
    /// A live process already owns this database's ledger.
    Held,
    /// The lock file could not be created or locked at all.
    Unavailable(String),
}

impl ProcessLockError {
    /// Bounded, path-free reason. It is safe to hand to a model: it names no
    /// directory, no session, and no run.
    pub(crate) fn message(&self) -> String {
        match self {
            Self::Held => {
                "another phasegent process owns this database's research run ledger, so research \
                 delegation is unavailable here; research runs stay owned by that process"
                    .to_owned()
            }
            Self::Unavailable(detail) => format!(
                "the research run ledger could not be locked, so research delegation is \
                 unavailable: {detail}"
            ),
        }
    }
}

/// An exclusive advisory lock held on one database's run ledger.
///
/// Not `Clone`: the lock is one file handle, and the manager holds it behind an
/// `Arc` so every clone of the manager refers to the same held lock rather than
/// taking a second one. Dropping the last handle closes the file, which is what
/// releases the lock within a running process.
pub(crate) struct ProcessLock {
    path: PathBuf,
    /// Held, never read: the open handle *is* the lock.
    file: File,
}

impl ProcessLock {
    /// Claim `database`'s run ledger, creating the lock file if it is missing.
    ///
    /// Non-blocking on purpose. A caller that has to wait for another process
    /// to finish is a caller that should not be serving research calls at all,
    /// and blocking here would hide the contention behind a request that looks
    /// slow instead of one that fails closed.
    pub(crate) fn acquire(database: &Path) -> Result<Self, ProcessLockError> {
        let path = lock_path_for(database);
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| ProcessLockError::Unavailable(bounded(&error.to_string())))?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Self { path, file }),
            // Contention is recognised the way `fs2` recognises it, so the
            // classification follows the platform (`EWOULDBLOCK` on Unix,
            // `ERROR_LOCK_VIOLATION` on Windows) instead of guessing.
            Err(error) if error.kind() == fs2::lock_contended_error().kind() => {
                Err(ProcessLockError::Held)
            }
            Err(error) => Err(ProcessLockError::Unavailable(bounded(&error.to_string()))),
        }
    }

    /// The lock file this handle holds, for tests and diagnostics.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

/// `<database directory>/<LOCK_FILE_NAME>`.
///
/// A database path with no parent component (a bare filename) resolves to the
/// current directory, so the lock is never silently skipped.
pub(crate) fn lock_path_for(database: &Path) -> PathBuf {
    let directory = database
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    directory.join(LOCK_FILE_NAME)
}

/// Bound an OS error string before it reaches a message.
fn bounded(raw: &str) -> String {
    raw.chars().take(200).collect()
}
