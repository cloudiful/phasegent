//! Lease-row storage helpers for the worktree module.
//!
//! The DDL is created lazily via [`ensure_schema`] (a `CREATE TABLE
//! IF NOT EXISTS` block) so opening the database before Phase 1
//! still succeeds; there is no row in the central `MIGRATIONS` block
//! and the table is created the first time a worktree helper runs.
//! All queries pass parameters through `rusqlite::params!` so a
//! caller-supplied string can never reach the SQL parser unsanitised.

use rusqlite::{OptionalExtension, TransactionBehavior};

use crate::infra::storage::Storage;
use crate::worktree::{
    LEASE_STATUS_ACTIVE, LEASE_STATUS_RETAINED, LeaseRow, WorktreeError, now_unix_secs,
};

/// Hard cap on `list()` / `status()` results so a runaway query never
/// floods the response. Mirrors the timer-ledger 64-row ceiling.
#[allow(dead_code)]
pub(super) const MAX_LEASES_PER_QUERY: i64 = 256;
/// Hard cap on leases scanned when checking for an existing
/// `(repo, issue, session)` match. Phase 1 always has a tiny per-repo
/// population, but the cap is a defensive upper bound.
#[allow(dead_code)]
pub(super) const MAX_ACTIVE_LEASES_PER_REPO: i64 = 256;

/// Inline `CREATE TABLE IF NOT EXISTS` for the worktree lease table.
/// The DDL stays self-contained in this module (per Phase 1 scope) so
/// opening the database before Phase 1 still succeeds: there is no
/// migration row in the central `MIGRATIONS` block, and the table is
/// created lazily the first time a worktree helper runs.
#[allow(dead_code)]
pub fn ensure_schema(storage: &Storage) -> Result<(), String> {
    storage
        .connection
        .execute_batch(WORKTREE_LEASES_SCHEMA)
        .map_err(|error| format!("could not initialise worktree lease table: {error}"))?;
    ensure_release_reason_column(storage)
}

/// Additive migration for `worktree_leases.release_reason`: databases
/// created before the force-release surface gain a NULL column on
/// open. Idempotent via `PRAGMA table_info`, mirroring the storage
/// `MIGRATIONS` runner without pulling it in.
fn ensure_release_reason_column(storage: &Storage) -> Result<(), String> {
    let mut statement = storage
        .connection
        .prepare(
            "SELECT name FROM pragma_table_info('worktree_leases') WHERE name = 'release_reason'",
        )
        .map_err(|error| format!("could not inspect worktree lease table: {error}"))?;
    let present: bool = statement
        .exists([])
        .map_err(|error| format!("could not inspect worktree lease table: {error}"))?;
    if !present {
        storage
            .connection
            .execute(
                "ALTER TABLE worktree_leases ADD COLUMN release_reason TEXT",
                [],
            )
            .map_err(|error| format!("could not migrate worktree lease table: {error}"))?;
    }
    Ok(())
}

#[allow(dead_code)]
const WORKTREE_LEASES_SCHEMA: &str = "\
-- `release_reason` records the operator justification for a forced
-- release (`worktree release --force --reason`); ordinary releases
-- keep it NULL. Lease rows are audit records and are never deleted.
CREATE TABLE IF NOT EXISTS worktree_leases (
    lease_id TEXT PRIMARY KEY,
    repo_identity TEXT NOT NULL,
    issue INTEGER NOT NULL,
    session TEXT NOT NULL DEFAULT '',
    checkout_path TEXT NOT NULL,
    worktree_path TEXT NOT NULL,
    branch TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    heartbeat_at INTEGER NOT NULL,
    release_reason TEXT
);

CREATE UNIQUE INDEX IF NOT EXISTS worktree_leases_repo_path_idx
    ON worktree_leases (repo_identity, worktree_path);

CREATE INDEX IF NOT EXISTS worktree_leases_repo_status_idx
    ON worktree_leases (repo_identity, status);
";

#[allow(dead_code)]
pub(super) fn find_active_lease(
    storage: &Storage,
    identity: &str,
    issue: u64,
    session: &str,
) -> Result<Option<LeaseRow>, WorktreeError> {
    let mut statement = storage
        .connection
        .prepare(
            "SELECT lease_id, repo_identity, issue, session, checkout_path, worktree_path, \
                    branch, status, created_at, heartbeat_at, release_reason \
             FROM worktree_leases \
             WHERE repo_identity = ?1 AND issue = ?2 AND session = ?3 AND status = ?4 \
             ORDER BY created_at DESC LIMIT 1",
        )
        .map_err(|error| {
            WorktreeError::new("storage", format!("prepare active lookup: {error}"))
        })?;
    let mut rows = statement
        .query(rusqlite::params![
            identity,
            issue as i64,
            session,
            LEASE_STATUS_ACTIVE
        ])
        .map_err(|error| WorktreeError::new("storage", format!("active lookup: {error}")))?;
    if let Some(row) = rows
        .next()
        .map_err(|error| WorktreeError::new("storage", format!("active row: {error}")))?
    {
        return Ok(Some(decode_lease_row(row)?));
    }
    Ok(None)
}

#[allow(dead_code)]
pub(super) fn count_other_active_leases(
    storage: &Storage,
    identity: &str,
    issue: u64,
    session: &str,
) -> Result<i64, WorktreeError> {
    let mut statement = storage
        .connection
        .prepare(
            "SELECT COUNT(*) FROM worktree_leases \
             WHERE repo_identity = ?1 AND status = ?2 \
               AND NOT (issue = ?3 AND session = ?4) \
             LIMIT ?5",
        )
        .map_err(|error| WorktreeError::new("storage", format!("prepare count: {error}")))?;
    let total: i64 = statement
        .query_row(
            rusqlite::params![
                identity,
                LEASE_STATUS_ACTIVE,
                issue as i64,
                session,
                MAX_ACTIVE_LEASES_PER_REPO
            ],
            |row| row.get(0),
        )
        .map_err(|error| WorktreeError::new("storage", format!("count: {error}")))?;
    Ok(total)
}

#[allow(dead_code)]
pub(crate) fn load_lease(
    storage: &Storage,
    lease_id: &str,
) -> Result<Option<LeaseRow>, WorktreeError> {
    let mut statement = storage
        .connection
        .prepare(
            "SELECT lease_id, repo_identity, issue, session, checkout_path, worktree_path, \
                    branch, status, created_at, heartbeat_at, release_reason \
             FROM worktree_leases WHERE lease_id = ?1",
        )
        .map_err(|error| WorktreeError::new("storage", format!("prepare load: {error}")))?;
    let row: Option<LeaseRow> = statement
        .query_row(rusqlite::params![lease_id], decode_lease_row)
        .optional()
        .map_err(|error| WorktreeError::new("storage", format!("load: {error}")))?;
    Ok(row)
}

#[allow(dead_code)]
pub fn insert_lease(storage: &Storage, lease: NewLease<'_>) -> Result<(), WorktreeError> {
    storage
        .connection
        .execute(
            "INSERT INTO worktree_leases \
                (lease_id, repo_identity, issue, session, checkout_path, worktree_path, branch, \
                 status, created_at, heartbeat_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                lease.lease_id,
                lease.identity,
                lease.issue as i64,
                lease.session,
                lease.checkout_path,
                lease.worktree_path,
                lease.branch,
                lease.status,
                lease.created_at,
                lease.heartbeat_at,
            ],
        )
        .map_err(|error| WorktreeError::new("storage", format!("insert lease: {error}")))?;
    Ok(())
}

/// Borrowed view of every field the `worktree_leases` table stores.
/// Using a struct keeps `insert_lease` under the clippy
/// `too_many_arguments` threshold (which is 7 by default) and lets
/// the caller thread the same `NewLease` value through both
/// `idempotent reuse` and `new_worktree` acquire paths without
/// losing readability.
#[derive(Debug, Clone)]
pub struct NewLease<'a> {
    pub lease_id: &'a str,
    pub identity: &'a str,
    pub issue: u64,
    pub session: &'a str,
    pub checkout_path: &'a str,
    pub worktree_path: &'a str,
    pub branch: &'a str,
    pub status: &'a str,
    pub created_at: i64,
    pub heartbeat_at: i64,
}

#[allow(dead_code)]
pub(super) fn refresh_heartbeat(storage: &Storage, lease_id: &str) -> Result<(), WorktreeError> {
    let now = now_unix_secs();
    storage
        .connection
        .execute(
            "UPDATE worktree_leases SET heartbeat_at = ?2 WHERE lease_id = ?1",
            rusqlite::params![lease_id, now],
        )
        .map_err(|error| WorktreeError::new("storage", format!("heartbeat: {error}")))?;
    Ok(())
}

#[allow(dead_code)]
/// Persist the operator justification for a forced release. Only
/// called on the forced path after the status flip; ordinary
/// releases keep `release_reason` NULL.
pub(crate) fn record_release_reason(
    storage: &Storage,
    lease_id: &str,
    reason: &str,
) -> Result<(), WorktreeError> {
    storage
        .connection
        .execute(
            "UPDATE worktree_leases SET release_reason = ?2 WHERE lease_id = ?1",
            rusqlite::params![lease_id, reason],
        )
        .map_err(|error| WorktreeError::new("storage", format!("record reason: {error}")))?;
    Ok(())
}

pub(crate) fn update_status(
    storage: &Storage,
    lease_id: &str,
    target_status: &str,
) -> Result<(), WorktreeError> {
    let now = now_unix_secs();
    storage
        .connection
        .execute(
            "UPDATE worktree_leases SET status = ?2, heartbeat_at = ?3 WHERE lease_id = ?1",
            rusqlite::params![lease_id, target_status, now],
        )
        .map_err(|error| WorktreeError::new("storage", format!("release: {error}")))?;
    Ok(())
}

/// Refresh `heartbeat_at` for an active lease owned by `session`.
///
/// The update is one conditional SQLite statement (`WHERE lease_id = ?
/// AND status = 'active' AND session = ?`), so a foreign session can
/// never extend the row and a heartbeat racing a stale recovery resolves
/// to at most one winner. Returns `true` iff exactly one row was
/// updated (issue 305 Task 2).
pub(crate) fn heartbeat_active_lease(
    storage: &Storage,
    lease_id: &str,
    session: &str,
    now: i64,
) -> Result<bool, WorktreeError> {
    let updated = storage
        .connection
        .execute(
            "UPDATE worktree_leases SET heartbeat_at = ?4 \
             WHERE lease_id = ?1 AND status = ?2 AND session = ?3",
            rusqlite::params![lease_id, LEASE_STATUS_ACTIVE, session, now],
        )
        .map_err(|error| WorktreeError::new("storage", format!("heartbeat lease: {error}")))?;
    Ok(updated == 1)
}

/// Active leases for `identity` whose heartbeat is older than
/// `stale_before`, oldest first. Read-only; the apply path re-checks the
/// predicate inside its write transaction (issue 305 Task 2).
pub(crate) fn stale_active_leases(
    storage: &Storage,
    identity: &str,
    stale_before: i64,
) -> Result<Vec<LeaseRow>, WorktreeError> {
    select_stale_active_leases(&storage.connection, identity, stale_before)
}

fn select_stale_active_leases(
    connection: &rusqlite::Connection,
    identity: &str,
    stale_before: i64,
) -> Result<Vec<LeaseRow>, WorktreeError> {
    let mut statement = connection
        .prepare(
            "SELECT lease_id, repo_identity, issue, session, checkout_path, worktree_path, \
                    branch, status, created_at, heartbeat_at, release_reason \
             FROM worktree_leases \
             WHERE repo_identity = ?1 AND status = ?2 AND heartbeat_at < ?3 \
             ORDER BY heartbeat_at ASC LIMIT ?4",
        )
        .map_err(|error| WorktreeError::new("storage", format!("prepare stale list: {error}")))?;
    let rows = statement
        .query_map(
            rusqlite::params![
                identity,
                LEASE_STATUS_ACTIVE,
                stale_before,
                MAX_LEASES_PER_QUERY
            ],
            decode_lease_row,
        )
        .map_err(|error| WorktreeError::new("storage", format!("stale list: {error}")))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(
            row.map_err(|error| WorktreeError::new("storage", format!("stale row: {error}")))?,
        );
    }
    Ok(out)
}

/// Atomically flip every active lease for `identity` whose heartbeat is
/// older than `stale_before` to `retained`, record `reason`, and return
/// the flipped rows. All updates run in one `BEGIN IMMEDIATE` write
/// transaction and each re-checks `status = 'active' AND heartbeat_at <
/// stale_before`, so a heartbeat that refreshed the row first is never
/// clobbered and a lease flips at most once under concurrent racers. The
/// worktree directory and branch are never touched (issue 305 Task 2).
pub(crate) fn recover_stale_active_leases(
    storage: &mut Storage,
    identity: &str,
    stale_before: i64,
    reason: &str,
    now: i64,
) -> Result<Vec<LeaseRow>, WorktreeError> {
    storage
        .connection
        .set_transaction_behavior(TransactionBehavior::Immediate);
    let transaction = storage
        .connection
        .unchecked_transaction()
        .map_err(|error| WorktreeError::new("storage", format!("begin stale recovery: {error}")))?;
    let candidates = select_stale_active_leases(&transaction, identity, stale_before)?;
    let mut flipped = Vec::with_capacity(candidates.len());
    for row in candidates {
        let updated = transaction
            .execute(
                "UPDATE worktree_leases SET status = ?4, heartbeat_at = ?5, release_reason = ?6 \
                 WHERE lease_id = ?1 AND status = ?2 AND heartbeat_at < ?3",
                rusqlite::params![
                    row.lease_id,
                    LEASE_STATUS_ACTIVE,
                    stale_before,
                    LEASE_STATUS_RETAINED,
                    now,
                    reason
                ],
            )
            .map_err(|error| WorktreeError::new("storage", format!("flip stale lease: {error}")))?;
        if updated == 1 {
            flipped.push(LeaseRow {
                status: LEASE_STATUS_RETAINED.to_owned(),
                heartbeat_at: now,
                release_reason: Some(reason.to_owned()),
                ..row
            });
        }
    }
    transaction.commit().map_err(|error| {
        WorktreeError::new("storage", format!("commit stale recovery: {error}"))
    })?;
    Ok(flipped)
}

/// Atomically flip every `active` lease matching
/// `(repo_identity, issue, session)` to `retained`, record `reason`, and
/// return the flipped row count.
///
/// The flip runs in one `BEGIN IMMEDIATE` transaction with a single
/// conditional `UPDATE` on `status = 'active'`, so a lease owned by
/// another session, issue, or repository is never touched and a lease
/// flips at most once under a concurrent racer. Terminal rows are left
/// untouched as audit records; the worktree directory and branch are
/// never modified (issue 305 Task 3).
pub(crate) fn retain_active_leases_for_issue_session(
    storage: &mut Storage,
    identity: &str,
    issue: u64,
    session: &str,
    reason: &str,
    now: i64,
) -> Result<u64, WorktreeError> {
    storage
        .connection
        .set_transaction_behavior(TransactionBehavior::Immediate);
    let transaction = storage
        .connection
        .unchecked_transaction()
        .map_err(|error| WorktreeError::new("storage", format!("begin lease release: {error}")))?;
    let updated = transaction
        .execute(
            "UPDATE worktree_leases \
             SET status = ?5, heartbeat_at = ?6, release_reason = ?7 \
             WHERE repo_identity = ?1 AND issue = ?2 AND session = ?3 AND status = ?4",
            rusqlite::params![
                identity,
                issue as i64,
                session,
                LEASE_STATUS_ACTIVE,
                LEASE_STATUS_RETAINED,
                now,
                reason
            ],
        )
        .map_err(|error| WorktreeError::new("storage", format!("release issue leases: {error}")))?;
    transaction
        .commit()
        .map_err(|error| WorktreeError::new("storage", format!("commit lease release: {error}")))?;
    Ok(updated as u64)
}

#[allow(dead_code)]
pub fn list_for_issue(storage: &Storage, issue: u64) -> Result<Vec<LeaseRow>, WorktreeError> {
    let mut statement = storage
        .connection
        .prepare(
            "SELECT lease_id, repo_identity, issue, session, checkout_path, worktree_path, \
                    branch, status, created_at, heartbeat_at, release_reason \
             FROM worktree_leases \
             WHERE issue = ?1 AND status = ?2 \
             ORDER BY created_at DESC LIMIT ?3",
        )
        .map_err(|error| WorktreeError::new("storage", format!("prepare issue list: {error}")))?;
    let rows = statement
        .query_map(
            rusqlite::params![issue as i64, LEASE_STATUS_ACTIVE, MAX_LEASES_PER_QUERY],
            decode_lease_row,
        )
        .map_err(|error| WorktreeError::new("storage", format!("issue list: {error}")))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(
            row.map_err(|error| WorktreeError::new("storage", format!("issue row: {error}")))?,
        );
    }
    Ok(out)
}

#[allow(dead_code)]
pub fn list_for_repo(storage: &Storage, identity: &str) -> Result<Vec<LeaseRow>, WorktreeError> {
    let mut statement = storage
        .connection
        .prepare(
            "SELECT lease_id, repo_identity, issue, session, checkout_path, worktree_path, \
                    branch, status, created_at, heartbeat_at, release_reason \
             FROM worktree_leases \
             WHERE repo_identity = ?1 \
             ORDER BY created_at DESC LIMIT ?2",
        )
        .map_err(|error| WorktreeError::new("storage", format!("prepare repo list: {error}")))?;
    let rows = statement
        .query_map(
            rusqlite::params![identity, MAX_LEASES_PER_QUERY],
            decode_lease_row,
        )
        .map_err(|error| WorktreeError::new("storage", format!("repo list: {error}")))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|error| WorktreeError::new("storage", format!("repo row: {error}")))?);
    }
    Ok(out)
}

#[allow(dead_code)]
fn decode_lease_row(row: &rusqlite::Row<'_>) -> Result<LeaseRow, rusqlite::Error> {
    Ok(LeaseRow {
        lease_id: row.get(0)?,
        repo_identity: row.get(1)?,
        issue: {
            let raw: i64 = row.get(2)?;
            raw.max(0) as u64
        },
        session: row.get(3)?,
        checkout_path: row.get(4)?,
        worktree_path: row.get(5)?,
        branch: row.get(6)?,
        status: row.get(7)?,
        created_at: row.get(8)?,
        heartbeat_at: row.get(9)?,
        release_reason: row.get(10)?,
    })
}
