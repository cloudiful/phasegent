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
    LEASE_STATUS_ACTIVE, LEASE_STATUS_RELEASED, LEASE_STATUS_RETAINED, LeaseRow, WorktreeError,
    now_unix_secs,
};

pub(crate) use super::acquire::{acquire_lease_seeded, release_active_leases_for_issue_with};

/// Seconds in a day, shared by the prune stale-window computation so
/// the read-only scan and the transactional recovery derive the same
/// cutoff from `stale_days` (issue 337 Phase 2).
pub(crate) const SECONDS_PER_DAY: i64 = 86_400;

/// A resolved stale window: the age threshold and the absolute cutoff
/// derived from the same `stale_days`. Bundling them keeps the
/// read-only candidate scan and the transactional recovery on one
/// definition of "stale".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StaleWindow {
    pub threshold_secs: i64,
    pub before: i64,
}

/// Build the stale window for `now` and `stale_days`, saturating at the
/// integer bounds so an absurd day count cannot overflow.
pub(crate) fn stale_window(now: i64, stale_days: u32) -> StaleWindow {
    let threshold_secs = i64::from(stale_days).saturating_mul(SECONDS_PER_DAY);
    StaleWindow {
        threshold_secs,
        before: now.saturating_sub(threshold_secs),
    }
}

/// Hard cap on `list()` / `status()` results so a runaway query never
/// floods the response. Mirrors the timer-ledger 64-row ceiling.
pub(super) const MAX_LEASES_PER_QUERY: i64 = 256;
/// Hard cap on leases scanned when checking for an existing
/// `(repo, issue, session)` match. Phase 1 always has a tiny per-repo
/// population, but the cap is a defensive upper bound.
pub(super) const MAX_ACTIVE_LEASES_PER_REPO: i64 = 256;

/// Column list every lease read selects, so the row shape and the
/// decoder stay paired in one place.
const LEASE_SELECT: &str = "SELECT lease_id, repo_identity, issue, session, checkout_path, worktree_path, \
     branch, status, created_at, heartbeat_at, release_reason FROM worktree_leases";

/// Run one bounded lease query and decode every row. `label` names the
/// failing step in the structured storage error.
fn query_leases(
    connection: &rusqlite::Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
    label: &str,
) -> Result<Vec<LeaseRow>, WorktreeError> {
    let mut statement = connection
        .prepare(sql)
        .map_err(|error| WorktreeError::new("storage", format!("prepare {label} list: {error}")))?;
    let rows = statement
        .query_map(params, decode_lease_row)
        .map_err(|error| WorktreeError::new("storage", format!("{label} list: {error}")))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(
            row.map_err(|error| WorktreeError::new("storage", format!("{label} row: {error}")))?,
        );
    }
    Ok(out)
}

/// Operator-facing guidance for a `(repo_identity, worktree_path)`
/// collision. SQLite reports the raw index name (`insert lease: UNIQUE
/// constraint failed: worktree_leases.<index>`), which names neither
/// the cause nor the fix. Every lease-insert caller (the reuse-current
/// path and the new-worktree compensation path) gets this wording
/// instead; the `storage` kind, and therefore the CLI exit code, is
/// unchanged. Since issue 628 P4 the uniqueness invariant covers
/// `active` rows only (see `WORKTREE_LEASES_SCHEMA`), so this guidance
/// always means a *live* lease already owns the checkout.
const REPO_PATH_CONFLICT_GUIDANCE: &str = "current directory already has a worktree lease for this repository; add --isolate to acquire a separate worktree, or review existing leases with `worktree status` / `worktree list`";

#[allow(dead_code)]
pub fn ensure_schema(storage: &Storage) -> Result<(), String> {
    super::lease_schema::ensure_lease_schema(storage)
}

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
             WHERE repo_identity = ?1 AND issue = ?2 AND session = ?3 AND status = ?4 AND NOT EXISTS (SELECT 1 FROM worktree_lease_scopes s WHERE s.lease_id = worktree_leases.lease_id) \
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

pub(crate) fn find_active_lease_for_scope(
    storage: &Storage,
    identity: &str,
    session: &str,
    scope: &super::LeaseScope,
) -> Result<Option<LeaseRow>, WorktreeError> {
    // The task id is part of the identity: a same-number task on another
    // instance is a different scope and must never be reused as this one.
    // `IS` compares NULL with NULL, so a legacy sidecar without a task still
    // matches a legacy scope. An older store has no task column at all, so
    // the legacy query is used there.
    let has_task_id = crate::worktree::lease_scope::scope_has_task_id(&storage.connection)?;
    let sql = if has_task_id {
        format!(
            "{LEASE_SELECT} WHERE repo_identity = ?1 AND session = ?2 AND status = ?3 AND EXISTS (SELECT 1 FROM worktree_lease_scopes s WHERE s.lease_id = worktree_leases.lease_id AND s.provider = ?4 AND s.project = ?5 AND s.external_id = ?6 AND s.task_id IS ?7) ORDER BY created_at DESC LIMIT 1"
        )
    } else {
        format!(
            "{LEASE_SELECT} WHERE repo_identity = ?1 AND session = ?2 AND status = ?3 AND EXISTS (SELECT 1 FROM worktree_lease_scopes s WHERE s.lease_id = worktree_leases.lease_id AND s.provider = ?4 AND s.project = ?5 AND s.external_id = ?6) ORDER BY created_at DESC LIMIT 1"
        )
    };
    storage
        .connection
        .query_row(
            &sql,
            rusqlite::params![
                identity,
                session,
                LEASE_STATUS_ACTIVE,
                scope.provider(),
                scope.project(),
                scope.external_id() as i64,
                scope.task_id()
            ],
            decode_lease_row,
        )
        .optional()
        .map_err(|e| WorktreeError::new("storage", format!("scoped active lookup: {e}")))
}

/// One member of the complete ownership proof for the canonical directories
/// an exact-scope retention would affect.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ScopedRetentionMember {
    pub lease_id: String,
    pub issue: u64,
    pub status: String,
    pub directory: std::path::PathBuf,
    pub scope: Option<ScopedScopeProjection>,
}

/// The scope projection a retention proof compares.
///
/// It carries the durable task identity so a lease whose task binding or
/// instance changed between the approved proof and the update aborts the
/// transaction instead of being released on stale evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ScopedScopeProjection {
    pub provider: String,
    pub project: String,
    pub external_id: u64,
    pub task_id: Option<i64>,
    pub instance: Option<String>,
}

impl ScopedScopeProjection {
    fn of(scope: &super::LeaseScope) -> Self {
        Self {
            provider: scope.provider().to_owned(),
            project: scope.project().to_owned(),
            external_id: scope.external_id(),
            task_id: scope.task_id(),
            instance: scope.instance().map(str::to_owned),
        }
    }
}

/// The complete affected-directory membership, scope, and associated-issue
/// proof approved before retention. The retention transaction rebuilds this
/// exact projection and refuses every update when it changed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ScopedRetentionProof {
    pub directories: Vec<std::path::PathBuf>,
    pub members: Vec<ScopedRetentionMember>,
    /// Same-number rows outside the affected directories: a late arrival here
    /// still makes ownership ambiguous and must abort the transaction.
    pub same_number_conflicts: Vec<ScopedRetentionMember>,
}

/// Build the complete affected-directory proof for one exact-scope issue.
pub(crate) fn scoped_retention_proof(
    issue: u64,
    all: &[LeaseRow],
    scopes: &std::collections::HashMap<String, super::LeaseScope>,
) -> ScopedRetentionProof {
    let paths = crate::worktree::CanonicalPaths::default();
    let mut directories: Vec<std::path::PathBuf> = all
        .iter()
        .filter(|row| row.issue == issue && scopes.contains_key(&row.lease_id))
        .map(|row| paths.resolve(std::path::Path::new(&row.worktree_path)))
        .collect();
    directories.sort();
    directories.dedup();
    let mut members: Vec<ScopedRetentionMember> = all
        .iter()
        .filter(|row| {
            let directory = paths.resolve(std::path::Path::new(&row.worktree_path));
            directories.binary_search(&directory).is_ok()
        })
        .map(|row| ScopedRetentionMember {
            lease_id: row.lease_id.clone(),
            issue: row.issue,
            status: row.status.clone(),
            directory: paths.resolve(std::path::Path::new(&row.worktree_path)),
            scope: scopes.get(&row.lease_id).map(ScopedScopeProjection::of),
        })
        .collect();
    members.sort_by(|left, right| {
        (&left.directory, &left.lease_id).cmp(&(&right.directory, &right.lease_id))
    });
    let member_ids: std::collections::HashSet<&str> = members
        .iter()
        .map(|member| member.lease_id.as_str())
        .collect();
    let mut same_number_conflicts: Vec<ScopedRetentionMember> = all
        .iter()
        .filter(|row| row.issue == issue && !member_ids.contains(row.lease_id.as_str()))
        .map(|row| ScopedRetentionMember {
            lease_id: row.lease_id.clone(),
            issue: row.issue,
            status: row.status.clone(),
            directory: paths.resolve(std::path::Path::new(&row.worktree_path)),
            scope: scopes.get(&row.lease_id).map(ScopedScopeProjection::of),
        })
        .collect();
    same_number_conflicts.sort_by(|left, right| {
        (&left.directory, &left.lease_id).cmp(&(&right.directory, &right.lease_id))
    });
    ScopedRetentionProof {
        directories,
        members,
        same_number_conflicts,
    }
}

/// Retain only the explicitly verified active lease ids under the exact
/// sidecar scope. Before any update the immediate transaction rebuilds the
/// complete affected-directory proof and aborts all updates when a new legacy
/// or scoped row, a changed path/status/scope, a changed associated issue, or
/// unreadable evidence differs from the approved proof. Every selected id must
/// still carry that exact provider/project/external id. Newly arrived rows
/// outside the verified set stay active.
#[allow(clippy::too_many_arguments)]
pub(crate) fn retain_active_leases_for_scope_ids(
    storage: &Storage,
    identity: &str,
    repo_key: Option<&str>,
    scope: &super::LeaseScope,
    lease_ids: &[String],
    approved: &ScopedRetentionProof,
    reason: &str,
    now: i64,
) -> Result<u64, WorktreeError> {
    let transaction = rusqlite::Transaction::new_unchecked(
        &storage.connection,
        rusqlite::TransactionBehavior::Immediate,
    )
    .map_err(|error| WorktreeError::new("storage", format!("begin scoped retain: {error}")))?;
    let rows = query_leases(
        &transaction,
        &format!("{LEASE_SELECT} WHERE repo_identity = ?1 ORDER BY created_at DESC"),
        &[&identity],
        "repo-all",
    )?;
    let scopes = crate::worktree::lease_scope::scope_map_for_connection_with_key(
        &transaction,
        identity,
        repo_key,
    )?;
    let current = scoped_retention_proof(scope.external_id(), &rows, &scopes);
    if current != *approved {
        return Err(WorktreeError::new(
            "storage",
            "scoped ownership proof changed before retention",
        ));
    }
    for lease_id in lease_ids {
        let proof: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM worktree_lease_scopes WHERE lease_id = ?1 AND provider = ?2 AND project = ?3 AND external_id = ?4",
                rusqlite::params![lease_id, scope.provider(), scope.project(), scope.external_id() as i64],
                |row| row.get(0),
            )
            .map_err(|error| WorktreeError::new("storage", format!("verify scoped retain: {error}")))?;
        if proof != 1 {
            return Err(WorktreeError::new(
                "storage",
                "scoped lease proof changed before retention",
            ));
        }
    }
    let mut count = 0u64;
    for lease_id in lease_ids {
        count += transaction
            .execute(
                "UPDATE worktree_leases SET status = ?2, heartbeat_at = ?3, release_reason = ?4 WHERE repo_identity = ?1 AND lease_id = ?5 AND issue = ?6 AND status = 'active'",
                rusqlite::params![
                    identity,
                    LEASE_STATUS_RETAINED,
                    now,
                    reason,
                    lease_id,
                    scope.external_id() as i64
                ],
            )
            .map_err(|error| WorktreeError::new("storage", format!("retain verified leases: {error}")))?
            as u64;
    }
    transaction
        .commit()
        .map_err(|error| WorktreeError::new("storage", format!("commit scoped retain: {error}")))?;
    Ok(count)
}

/// Retain the active leases for one exact sidecar scope through the same
/// validated transaction boundary as scoped sync. The complete scope map is
/// parsed first, so malformed or mismatched evidence (for example a sidecar
/// `external_id` that disagrees with its lease issue) aborts the whole
/// release before any row changes. The approved proof is rebuilt inside the
/// immediate transaction and every selected id is updated only while its
/// lease issue, sidecar scope and `active` status still match.
pub(crate) fn retain_active_leases_for_scope(
    storage: &Storage,
    identity: &str,
    repo_key: Option<&str>,
    scope: &super::LeaseScope,
    reason: &str,
    now: i64,
) -> Result<u64, WorktreeError> {
    let rows = query_leases(
        &storage.connection,
        &format!("{LEASE_SELECT} WHERE repo_identity = ?1 ORDER BY created_at DESC"),
        &[&identity],
        "repo-all",
    )?;
    let scopes = crate::worktree::lease_scope::scope_map_for_connection_with_key(
        &storage.connection,
        identity,
        repo_key,
    )?;
    let approved = scoped_retention_proof(scope.external_id(), &rows, &scopes);
    let lease_ids: Vec<String> = rows
        .iter()
        .filter(|row| row.status.as_str() == LEASE_STATUS_ACTIVE)
        .filter(|row| scopes.get(&row.lease_id) == Some(scope))
        .map(|row| row.lease_id.clone())
        .collect();
    retain_active_leases_for_scope_ids(
        storage, identity, repo_key, scope, &lease_ids, &approved, reason, now,
    )
}

/// The `active` lease occupying `worktree_path` under `identity`, if
/// any (issue 651 P2).
///
/// Path-scoped conflict lookup for the lease-first acquire table: at
/// most one row can match because the active-only
/// `(repo_identity, worktree_path)` partial index enforces it, so the
/// newest row wins defensively and `None` means the checkout path is
/// free. Only `active` rows match — terminal history never conflicts,
/// and a lease on any other path is invisible here by construction.
#[allow(dead_code)]
pub(super) fn find_active_lease_for_path(
    storage: &Storage,
    identity: &str,
    worktree_path: &str,
) -> Result<Option<LeaseRow>, WorktreeError> {
    let mut statement = storage
        .connection
        .prepare(
            "SELECT lease_id, repo_identity, issue, session, checkout_path, worktree_path, \
                    branch, status, created_at, heartbeat_at, release_reason \
              FROM worktree_leases \
              WHERE repo_identity = ?1 AND worktree_path = ?2 AND status = ?3 \
              ORDER BY created_at DESC LIMIT 1",
        )
        .map_err(|error| WorktreeError::new("storage", format!("prepare path lookup: {error}")))?;
    let row: Option<LeaseRow> = statement
        .query_row(
            rusqlite::params![identity, worktree_path, LEASE_STATUS_ACTIVE],
            decode_lease_row,
        )
        .optional()
        .map_err(|error| WorktreeError::new("storage", format!("path lookup: {error}")))?;
    Ok(row)
}

/// The active lease a read-only `worktree probe` resolves for
/// `(repo_identity, issue)`. When `session` is `Some` the row must match
/// it; otherwise the newest active row for the issue is used. `None`
/// means no matching active lease exists, so the probe returns its
/// stable empty result instead of guessing a worktree path.
#[allow(dead_code)]
pub fn find_active_lease_for_probe(
    storage: &Storage,
    identity: &str,
    issue: u64,
    session: Option<&str>,
) -> Result<Option<LeaseRow>, WorktreeError> {
    let mut statement = storage
        .connection
        .prepare(&format!(
            "{LEASE_SELECT} \
             WHERE repo_identity = ?1 AND issue = ?2 AND status = ?3 \
               AND NOT EXISTS (SELECT 1 FROM worktree_lease_scopes s WHERE s.lease_id = worktree_leases.lease_id) \
               AND (?4 IS NULL OR session = ?4) \
             ORDER BY created_at DESC LIMIT 1"
        ))
        .map_err(|error| WorktreeError::new("storage", format!("prepare probe lookup: {error}")))?;
    let row: Option<LeaseRow> = statement
        .query_row(
            rusqlite::params![identity, issue as i64, LEASE_STATUS_ACTIVE, session],
            decode_lease_row,
        )
        .optional()
        .map_err(|error| WorktreeError::new("storage", format!("probe lookup: {error}")))?;
    Ok(row)
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
    insert_lease_on(&storage.connection, lease)
}

fn insert_lease_on(
    connection: &rusqlite::Connection,
    lease: NewLease<'_>,
) -> Result<(), WorktreeError> {
    connection
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
        .map_err(insert_lease_error)?;
    Ok(())
}

/// Map a failed lease insert onto a structured error. The only
/// expected collision is the active-only `(repo_identity,
/// worktree_path)` unique index rejecting a second *live* lease for
/// one checkout; it is rewritten to actionable guidance while every
/// other SQLite failure keeps the `insert lease:` prefix and the raw
/// detail.
pub(crate) fn insert_lease_scoped(
    storage: &Storage,
    lease: NewLease<'_>,
    scope: &super::LeaseScope,
    repo_key: Option<&str>,
) -> Result<(), WorktreeError> {
    if lease.issue != scope.external_id() {
        return Err(WorktreeError::new(
            "argument",
            "lease issue and scoped external id must match",
        ));
    }
    let tx =
        rusqlite::Transaction::new_unchecked(&storage.connection, TransactionBehavior::Immediate)
            .map_err(|e| WorktreeError::new("storage", format!("begin scoped lease: {e}")))?;
    ensure_no_unscoped_collision(&tx, lease.identity, lease.issue)?;
    // The durable task is validated inside the write boundary: the task
    // must exist, be canonical, match the scope, and be an explicit member
    // of this physical repository. A task scope without a verified
    // repository key cannot be proven, so it is refused.
    if let Some(task_id) = scope.task_id() {
        let repo_key = repo_key.ok_or_else(|| {
            WorktreeError::new(
                "storage",
                "a durable task scope requires a verified repository key",
            )
        })?;
        let binding = crate::task_identity::binding_for_task_id_on(&tx, task_id)
            .map_err(|e| WorktreeError::new("storage", e))?
            .ok_or_else(|| {
                WorktreeError::new("storage", "lease scope references an unknown durable task")
            })?;
        let key = binding.key();
        if key.provider() != scope.provider()
            || key.project() != scope.project()
            || key.external_number() != Some(scope.external_id())
        {
            return Err(WorktreeError::new(
                "storage",
                "lease scope does not match its durable task",
            ));
        }
        if !crate::task_identity::task_repo_membership_exists_on(&tx, repo_key, task_id)
            .map_err(|e| WorktreeError::new("storage", e))?
        {
            return Err(WorktreeError::new(
                "storage",
                "the durable task is not a member of this repository",
            ));
        }
    }
    let id = lease.lease_id;
    let created = lease.created_at;
    insert_lease_on(&tx, lease)?;
    // The durable task id is stored with the sidecar so the lease keeps its
    // exact task identity. An older store has no task column, so a task
    // scope cannot be recorded there and is refused instead of silently
    // degraded to a legacy scope.
    let has_task_id = crate::worktree::lease_scope::scope_has_task_id(&tx)?;
    if has_task_id {
        tx.execute("INSERT INTO worktree_lease_scopes (lease_id, provider, project, external_id, task_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", rusqlite::params![id, scope.provider(), scope.project(), scope.external_id() as i64, scope.task_id(), created])
            .map_err(|e| WorktreeError::new("storage", format!("insert lease scope: {e}")))?;
    } else {
        if scope.is_task_scoped() {
            return Err(WorktreeError::new(
                "storage",
                "the lease store cannot record a durable task scope",
            ));
        }
        tx.execute("INSERT INTO worktree_lease_scopes (lease_id, provider, project, external_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5)", rusqlite::params![id, scope.provider(), scope.project(), scope.external_id() as i64, created])
            .map_err(|e| WorktreeError::new("storage", format!("insert lease scope: {e}")))?;
    }
    tx.commit()
        .map_err(|e| WorktreeError::new("storage", format!("commit scoped lease: {e}")))
}

pub(crate) fn ensure_no_unscoped_collision(
    connection: &rusqlite::Connection,
    identity: &str,
    issue: u64,
) -> Result<(), WorktreeError> {
    let found: bool = connection.query_row("SELECT EXISTS (SELECT 1 FROM worktree_leases l WHERE l.repo_identity = ?1 AND l.issue = ?2 AND NOT EXISTS (SELECT 1 FROM worktree_lease_scopes s WHERE s.lease_id = l.lease_id))", rusqlite::params![identity, issue as i64], |row| row.get(0))
        .map_err(|e| WorktreeError::new("storage", format!("check lease scope: {e}")))?;
    if found {
        return Err(WorktreeError::new(
            "scope_conflict",
            "unscoped lease has the same issue number; establish its ownership before scoped acquire",
        ));
    }
    Ok(())
}

fn insert_lease_error(error: rusqlite::Error) -> WorktreeError {
    if is_repo_path_conflict(&error) {
        return WorktreeError::new("storage", REPO_PATH_CONFLICT_GUIDANCE);
    }
    WorktreeError::new("storage", format!("insert lease: {error}"))
}

/// True when `error` is the `(repo_identity, worktree_path)` unique
/// index rejecting a duplicate checkout. SQLite reports this either
/// with the index name or with the two column names depending on the
/// linked SQLite version, so both shapes are recognised; both the
/// pre-P4 full-table index name and the P4 active-only partial index
/// name match because a mid-migration database may still carry the
/// old one (and trigger-based tests raise with it).
fn is_repo_path_conflict(error: &rusqlite::Error) -> bool {
    let rusqlite::Error::SqliteFailure(code, message) = error else {
        return false;
    };
    if code.code != rusqlite::ErrorCode::ConstraintViolation {
        return false;
    }
    message.as_deref().is_some_and(|text| {
        text.contains("worktree_leases_repo_path_idx")
            || text.contains("worktree_leases_active_repo_path_idx")
            || (text.contains("repo_identity") && text.contains("worktree_path"))
    })
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
    query_leases(
        connection,
        &format!(
            "{LEASE_SELECT} WHERE repo_identity = ?1 AND status = ?2 AND heartbeat_at < ?3 \
             ORDER BY heartbeat_at ASC LIMIT ?4"
        ),
        &[
            &identity,
            &LEASE_STATUS_ACTIVE,
            &stale_before,
            &MAX_LEASES_PER_QUERY,
        ],
        "stale",
    )
}

/// Atomically flip every active lease for `identity` whose heartbeat is
/// older than `stale_before` to `retained`, record `reason`, and return
/// the flipped rows. All updates run in one `BEGIN IMMEDIATE` write
/// transaction and each re-checks `status = 'active' AND heartbeat_at <
/// stale_before`, so a heartbeat that refreshed the row first is never
/// clobbered and a lease flips at most once under concurrent racers. A
/// blank `reason` is rejected so the recovery stays attributable. The
/// worktree directory and branch are never touched (issue 305 Task 2).
/// Which stale active rows an invocation may release.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub(crate) enum StaleRecoveryScope<'a> {
    /// Preserve the historical all-rows behavior for direct test callers.
    #[allow(dead_code)]
    All,
    /// Omitted/default and explicit legacy providers release only unscoped
    /// rows, never a scoped Forgejo lease.
    LegacyOnly,
    /// One exact scope: a legacy provider/project scope, or one durable task.
    ///
    /// A legacy selector (`task_id: None`) never sweeps a task row: the
    /// durable task owns its own leases, and an explicit provider selection
    /// is not a permission over them. A task selector owns exactly that task
    /// on exactly that instance.
    Exact {
        provider: &'a str,
        project: &'a str,
        /// The durable task this selector owns, when the group is
        /// task-bearing.
        task_id: Option<i64>,
        /// The canonical instance of that task, when the group is task-bearing.
        instance: Option<&'a str>,
    },
    /// An explicit provider without a usable exact scope releases nothing.
    Nothing,
}

impl StaleRecoveryScope<'_> {
    pub(crate) fn allows(&self, scope: Option<&super::LeaseScope>) -> bool {
        match (self, scope) {
            (Self::All, _) => true,
            (Self::LegacyOnly, None) => true,
            (Self::LegacyOnly, Some(_)) => false,
            (
                Self::Exact {
                    provider,
                    project,
                    task_id,
                    instance,
                },
                Some(scope),
            ) => {
                if scope.provider() != *provider || scope.project() != *project {
                    return false;
                }
                match task_id {
                    Some(task_id) => {
                        scope.task_id() == Some(*task_id)
                            && instance
                                .map(|instance| scope.instance() == Some(instance))
                                .unwrap_or(true)
                    }
                    None => scope.task_id().is_none(),
                }
            }
            (Self::Exact { .. }, None) => false,
            (Self::Nothing, _) => false,
        }
    }
}

/// The scope-aware form used by production prune: only rows the invocation's
/// provider selection is allowed to own are flipped.
pub(crate) fn recover_stale_active_leases_for_scope(
    storage: &mut Storage,
    identity: &str,
    repo_key: Option<&str>,
    stale_before: i64,
    reason: &str,
    now: i64,
    recovery_scope: StaleRecoveryScope<'_>,
) -> Result<Vec<LeaseRow>, WorktreeError> {
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(WorktreeError::new(
            "argument",
            "stale recovery requires a non-empty reason",
        ));
    }
    storage
        .connection
        .set_transaction_behavior(TransactionBehavior::Immediate);
    let transaction = storage
        .connection
        .unchecked_transaction()
        .map_err(|error| WorktreeError::new("storage", format!("begin stale recovery: {error}")))?;
    let candidates = select_stale_active_leases(&transaction, identity, stale_before)?;
    // Rebuild and validate the COMPLETE sidecar map on the transaction's own
    // connection before any update. A missing row is unscoped; a malformed,
    // mismatched or unreadable row aborts every update instead of being
    // silently treated as unscoped legacy ownership.
    let scopes = crate::worktree::lease_scope::scope_map_for_connection_with_key(
        &transaction,
        identity,
        repo_key,
    )?;
    let mut flipped = Vec::with_capacity(candidates.len());
    for row in candidates {
        let lease_scope = scopes.get(&row.lease_id);
        if !recovery_scope.allows(lease_scope) {
            continue;
        }
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

#[allow(dead_code)]
pub(crate) fn recover_stale_active_leases(
    storage: &mut Storage,
    identity: &str,
    stale_before: i64,
    reason: &str,
    now: i64,
) -> Result<Vec<LeaseRow>, WorktreeError> {
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(WorktreeError::new(
            "argument",
            "stale recovery requires a non-empty reason",
        ));
    }
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

/// Release absent-path rows only after revalidating the complete ownership
/// evidence inside one immediate transaction: exact scanned row identity,
/// status and path, physical absence again, a readable complete sidecar map,
/// and no repository-wide same-number scoped/unscoped ambiguity.
pub(crate) fn directory_ownership_proof(
    canonical: &std::path::Path,
    all: &[LeaseRow],
    scopes: &std::collections::HashMap<String, super::LeaseScope>,
) -> ScopedRetentionProof {
    let paths = crate::worktree::CanonicalPaths::default();
    let member =
        |row: &LeaseRow| paths.resolve(std::path::Path::new(&row.worktree_path)) == canonical;
    let mut members: Vec<ScopedRetentionMember> = all
        .iter()
        .filter(|row| member(row))
        .map(|row| ScopedRetentionMember {
            lease_id: row.lease_id.clone(),
            issue: row.issue,
            status: row.status.clone(),
            directory: canonical.to_path_buf(),
            scope: scopes.get(&row.lease_id).map(ScopedScopeProjection::of),
        })
        .collect();
    members.sort_by(|left, right| left.lease_id.cmp(&right.lease_id));
    let issues: std::collections::HashSet<u64> =
        members.iter().map(|member| member.issue).collect();
    let mut same_number_conflicts: Vec<ScopedRetentionMember> = all
        .iter()
        .filter(|row| issues.contains(&row.issue) && !member(row))
        .map(|row| ScopedRetentionMember {
            lease_id: row.lease_id.clone(),
            issue: row.issue,
            status: row.status.clone(),
            directory: paths.resolve(std::path::Path::new(&row.worktree_path)),
            scope: scopes.get(&row.lease_id).map(ScopedScopeProjection::of),
        })
        .collect();
    same_number_conflicts.sort_by(|left, right| {
        (&left.directory, &left.lease_id).cmp(&(&right.directory, &right.lease_id))
    });
    ScopedRetentionProof {
        directories: vec![canonical.to_path_buf()],
        members,
        same_number_conflicts,
    }
}

pub(crate) fn release_absent_leases_proven(
    storage: &Storage,
    identity: &str,
    repo_key: Option<&str>,
    targets: &[(String, String, String)],
    approved: &ScopedRetentionProof,
) -> Result<Vec<String>, WorktreeError> {
    let transaction = rusqlite::Transaction::new_unchecked(
        &storage.connection,
        TransactionBehavior::Immediate,
    )
    .map_err(|error| WorktreeError::new("storage", format!("begin absent release: {error}")))?;
    let rows = query_leases(
        &transaction,
        &format!("{LEASE_SELECT} WHERE repo_identity = ?1 ORDER BY created_at DESC"),
        &[&identity],
        "repo-all",
    )?;
    let scopes = crate::worktree::lease_scope::scope_map_for_connection_with_key(
        &transaction,
        identity,
        repo_key,
    )?;
    let canonical = approved.directories.first().ok_or_else(|| {
        WorktreeError::new("storage", "absent-release ownership proof has no directory")
    })?;
    let current = directory_ownership_proof(canonical, &rows, &scopes);
    if current != *approved {
        return Err(WorktreeError::new(
            "storage",
            "directory ownership proof changed before absent-path release",
        ));
    }
    for (lease_id, status, worktree_path) in targets {
        let row = rows
            .iter()
            .find(|row| row.lease_id == *lease_id)
            .ok_or_else(|| {
                WorktreeError::new("storage", "absent-path lease disappeared before release")
            })?;
        if row.status != *status || row.worktree_path != *worktree_path {
            return Err(WorktreeError::new(
                "storage",
                "absent-path lease identity or status changed before release",
            ));
        }
        match std::fs::symlink_metadata(std::path::Path::new(&row.worktree_path)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(WorktreeError::new(
                    "storage",
                    "absent-path directory reappeared before release",
                ));
            }
            Err(error) => {
                return Err(WorktreeError::new(
                    "storage",
                    format!("could not verify absent-path directory: {error}"),
                ));
            }
        }
    }
    let mut flipped = Vec::new();
    for (lease_id, status, worktree_path) in targets {
        let updated = transaction
            .execute(
                "UPDATE worktree_leases SET status = ?4, heartbeat_at = ?5, release_reason = ?6 \
                 WHERE repo_identity = ?1 AND lease_id = ?2 AND status = ?3 AND worktree_path = ?7",
                rusqlite::params![
                    identity,
                    lease_id,
                    status,
                    LEASE_STATUS_RELEASED,
                    now_unix_secs(),
                    "worktree directory absent",
                    worktree_path
                ],
            )
            .map_err(|error| {
                WorktreeError::new("storage", format!("release absent lease: {error}"))
            })?;
        if updated != 1 {
            return Err(WorktreeError::new(
                "storage",
                "absent-path lease changed during release",
            ));
        }
        flipped.push(lease_id.clone());
    }
    transaction.commit().map_err(|error| {
        WorktreeError::new("storage", format!("commit absent release: {error}"))
    })?;
    Ok(flipped)
}

/// Atomically flip every `active` lease matching
/// `(repo_identity, issue)` to `retained`, across every session, record
/// `reason`, and return the flipped row count.
///
/// The flip runs in one `BEGIN IMMEDIATE` transaction with a single
/// conditional `UPDATE` on `status = 'active'`, so a lease owned by
/// another issue or repository is never touched and a lease flips at
/// most once under a concurrent racer. Closing an issue converges its
/// lifecycle, so no session filter is applied here (issue 537 Phase 2);
/// session-scoped operations (heartbeat, acquire reuse) keep their own
/// guards. Terminal rows are left untouched as audit records; the
/// worktree directory and branch are never modified (issue 305 Task 3).
pub(crate) fn retain_active_leases_for_issue(
    storage: &Storage,
    identity: &str,
    issue: u64,
    reason: &str,
    now: i64,
) -> Result<u64, WorktreeError> {
    let transaction = rusqlite::Transaction::new_unchecked(
        &storage.connection,
        TransactionBehavior::Immediate,
    )
    .map_err(|error| WorktreeError::new("storage", format!("begin lease release: {error}")))?;
    let updated = transaction
        .execute(
            "UPDATE worktree_leases \
             SET status = ?4, heartbeat_at = ?5, release_reason = ?6 \
             WHERE repo_identity = ?1 AND issue = ?2 AND status = ?3 AND NOT EXISTS (SELECT 1 FROM worktree_lease_scopes s WHERE s.lease_id = worktree_leases.lease_id)",
            rusqlite::params![
                identity,
                issue as i64,
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

const PAGE_FILTER: &str = "WHERE (?1 IS NULL OR repo_identity = ?1) AND (?2 IS NULL OR (issue = ?2 AND status = 'active' AND NOT EXISTS (SELECT 1 FROM worktree_lease_scopes s WHERE s.lease_id = worktree_leases.lease_id)))";

pub(crate) fn count_leases(
    storage: &Storage,
    identity: Option<&str>,
    issue: Option<u64>,
) -> Result<usize, WorktreeError> {
    count_selected(&storage.connection, identity, issue)
}

fn count_selected(
    connection: &rusqlite::Connection,
    identity: Option<&str>,
    issue: Option<u64>,
) -> Result<usize, WorktreeError> {
    connection
        .query_row(
            &format!("SELECT COUNT(*) FROM worktree_leases {PAGE_FILTER}"),
            rusqlite::params![identity, issue.map(|n| n as i64)],
            |row| row.get(0),
        )
        .map_err(|error| WorktreeError::new("storage", format!("count leases: {error}")))
}

pub(crate) fn page_leases(
    storage: &Storage,
    identity: Option<&str>,
    issue: Option<u64>,
    request: crate::pagination::PageRequest,
) -> Result<(Vec<LeaseRow>, usize), WorktreeError> {
    let transaction = storage
        .connection
        .unchecked_transaction()
        .map_err(|error| WorktreeError::new("storage", format!("begin lease page: {error}")))?;
    let total = count_selected(&transaction, identity, issue)?;
    let rows = query_leases(
        &transaction,
        &format!(
            "{LEASE_SELECT} {PAGE_FILTER} ORDER BY created_at DESC, lease_id ASC LIMIT ?3 OFFSET ?4"
        ),
        &[
            &identity,
            &issue.map(|n| n as i64),
            &(request.limit() as i64),
            &(request.offset() as i64),
        ],
        "page",
    )?;
    transaction
        .commit()
        .map_err(|error| WorktreeError::new("storage", format!("finish lease page: {error}")))?;
    Ok((rows, total))
}

pub fn list_for_issue(storage: &Storage, issue: u64) -> Result<Vec<LeaseRow>, WorktreeError> {
    query_leases(
        &storage.connection,
        &format!(
            "{LEASE_SELECT} WHERE issue = ?1 AND status = ?2 AND NOT EXISTS (SELECT 1 FROM worktree_lease_scopes s WHERE s.lease_id = worktree_leases.lease_id) ORDER BY created_at DESC LIMIT ?3"
        ),
        &[&(issue as i64), &LEASE_STATUS_ACTIVE, &MAX_LEASES_PER_QUERY],
        "issue",
    )
}

#[allow(dead_code)]
pub fn list_for_repo(storage: &Storage, identity: &str) -> Result<Vec<LeaseRow>, WorktreeError> {
    query_leases(
        &storage.connection,
        &format!("{LEASE_SELECT} WHERE repo_identity = ?1 ORDER BY created_at DESC LIMIT ?2"),
        &[&identity, &MAX_LEASES_PER_QUERY],
        "repo",
    )
}

/// Every lease row for `identity`, without the [`MAX_LEASES_PER_QUERY`]
/// ceiling. The capped [`list_for_repo`] stays the read for every other
/// caller; this uncapped read exists for the prune pass, which must consider
/// every recorded row (a directory whose only active reference sits beyond
/// row 256 must still block removal).
pub fn list_all_for_repo(
    storage: &Storage,
    identity: &str,
) -> Result<Vec<LeaseRow>, WorktreeError> {
    query_leases(
        &storage.connection,
        &format!("{LEASE_SELECT} WHERE repo_identity = ?1 ORDER BY created_at DESC"),
        &[&identity],
        "repo-all",
    )
}

/// Every repository identity the lease table records, in a deterministic
/// order. This is the storage enumeration behind `issue sync --all` and the
/// global `worktree prune`: it reads the table the same way `list_for_repo`
/// does, scoped to the distinct identities, so a caller never has to walk the
/// filesystem looking for checkouts.
pub fn repo_identities(storage: &Storage) -> Result<Vec<String>, WorktreeError> {
    let mut statement = storage
        .connection
        .prepare("SELECT DISTINCT repo_identity FROM worktree_leases ORDER BY repo_identity")
        .map_err(|error| WorktreeError::new("storage", format!("prepare repo scan: {error}")))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| WorktreeError::new("storage", format!("repo scan: {error}")))?;
    let mut identities = Vec::new();
    for row in rows {
        identities.push(
            row.map_err(|error| WorktreeError::new("storage", format!("repo row: {error}")))?,
        );
    }
    Ok(identities)
}

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
