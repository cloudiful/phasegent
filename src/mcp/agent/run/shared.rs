//! The process-wide run manager shared by every MCP server instance.
//!
//! `mcp serve` over stdio builds one server, but the streamable HTTP transport
//! builds a new server — and therefore a new manager — per client connection.
//! A per-instance manager would give each HTTP session its own in-memory
//! flight registry: a run started by one connection would be invisible to the
//! next, so a `wait` or `cancel` would find no live process even while the run
//! was active.
//!
//! [`shared`] memoises one manager per database file, so every HTTP session in
//! this process reaches the same flights *and* the same held ledger lock. The
//! key is the storage path, so a test that points `PHASEGENT_DB_PATH` at its
//! own scratch database still gets its own manager and never observes another
//! test's runs.
//!
//! Exclusion *between* processes is not this module's job: it comes from the
//! kernel-held lock [`RunManager::new`] takes
//! ([`super::process_lock`]). Sharing one manager per process is what keeps
//! that lock claim singular — a second manager over the same database in the
//! same process would be refused by the lock exactly as a second process is.
//!
//! The cache is process-wide, so a hit costs one extra `Storage::open` from
//! the caller. That is the same per-call open the notification path already
//! pays, and it keeps one database to one manager without a lock on the hot
//! path.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::infra::storage::Storage;

use super::RunManager;

/// Memoised managers, keyed by the storage path they were built over.
fn registry() -> &'static Mutex<HashMap<String, RunManager>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, RunManager>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The manager for the database `storage` is open on, claiming it on first use.
///
/// The caller keeps its own connection for the reads it does inline (the
/// lease lookup, for one), so this opens a second one over the same path on a
/// miss and drops it again. That is the same per-call open the notification
/// path already pays, and it keeps one database to one manager without a lock
/// on the hot path.
///
/// A refusal is deliberately *not* memoised. The refusal is the lock saying
/// another process owns the ledger, and that process may exit at any moment;
/// caching the failure would strand this process until it restarted, while
/// retrying the claim costs one non-blocking lock attempt.
pub fn shared(storage: &Storage) -> Result<RunManager, String> {
    let key = storage.path.display().to_string();
    let mut managers = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(existing) = managers.get(&key) {
        return Ok(existing.clone());
    }
    let manager = RunManager::new(Storage::open_at(&storage.path)?)?;
    managers.insert(key, manager.clone());
    Ok(manager)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::agent::store_tests::temp_db_path;

    #[test]
    fn one_database_resolves_to_one_manager_and_another_to_its_own() {
        let first_path = temp_db_path("shared-a");
        let second_path = temp_db_path("shared-b");
        let a = shared(&Storage::open_at(&first_path).expect("open a")).expect("first manager");
        let b = shared(&Storage::open_at(&second_path).expect("open b")).expect("second manager");
        let repeat = shared(&Storage::open_at(&first_path).expect("reopen a")).expect("repeat");
        assert!(a.same_manager_as(&repeat), "one database, one manager");
        assert!(
            a.shares_ledger_lock_with(&repeat),
            "a repeated call must reuse the held ledger lock"
        );
        assert!(!a.same_manager_as(&b), "two databases, two managers");
    }

    /// A refusal is not memoised: the competing process may exit at any moment,
    /// so the next call has to try the claim again instead of replaying an old
    /// failure until this process restarts.
    #[test]
    fn a_refused_claim_is_retried_rather_than_cached() {
        let path = temp_db_path("shared-retry");
        let holder = RunManager::new(Storage::open_at(&path).expect("holder storage"))
            .expect("holder claims the ledger");
        let refused = shared(&Storage::open_at(&path).expect("blocked storage"));
        assert!(refused.is_err(), "the ledger is already claimed");
        drop(holder);
        let manager = shared(&Storage::open_at(&path).expect("retry storage"))
            .expect("the claim is retried once the holder is gone");
        assert!(manager.ledger_lock_path().exists());
    }
}
