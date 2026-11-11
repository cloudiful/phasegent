//! The worktree directory registry.
//!
//! A directory is identified by its repository identity and canonical path.
//! Each directory references one or more durable tasks or unknown legacy
//! issue identities, and the sessions observed in it. The registry stores no
//! occupancy state: a directory is live because an associated task is open or
//! a session currently reports it, never because a row says so.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::OptionalExtension;

use crate::infra::storage::Storage;
use crate::pagination::PageRequest;
use crate::worktree::WorktreeError;

use super::registry_schema::{ensure_registry, table_exists};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryTask {
    pub task_id: i64,
    pub issue: u64,
    pub provider: Option<String>,
    pub instance: Option<String>,
    pub project: Option<String>,
    pub external_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryLegacyIssue {
    pub issue: u64,
    pub provider: String,
    pub project: String,
    pub external_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryRecord {
    pub directory_id: String,
    pub repo_identity: String,
    pub checkout_path: String,
    pub worktree_path: String,
    pub branch: String,
    pub created_at: i64,
    pub tasks: Vec<DirectoryTask>,
    pub legacy_issues: Vec<DirectoryLegacyIssue>,
    pub sessions: Vec<String>,
}

impl DirectoryRecord {
    pub fn issue_numbers(&self) -> Vec<u64> {
        let mut issues: Vec<u64> = self
            .tasks
            .iter()
            .map(|task| task.issue)
            .chain(self.legacy_issues.iter().map(|issue| issue.issue))
            .collect();
        issues.sort_unstable();
        issues.dedup();
        issues
    }

    pub fn has_issue(&self, issue: u64) -> bool {
        self.issue_numbers().contains(&issue)
    }

    pub fn has_session(&self, session: &str) -> bool {
        let session = session.trim();
        !session.is_empty() && self.sessions.iter().any(|known| known == session)
    }

    pub fn is_scoped(&self) -> bool {
        !self.tasks.is_empty()
            || self
                .legacy_issues
                .iter()
                .any(|issue| !issue.provider.is_empty() && !issue.project.is_empty())
    }

    pub fn canonical_path(&self) -> std::path::PathBuf {
        super::canonical_or_self(Path::new(&self.worktree_path))
    }
}

#[derive(Debug, Clone)]
pub enum NewAssociation {
    Task {
        task_id: i64,
        issue: u64,
        provider: String,
        project: String,
        external_id: u64,
    },
    Legacy {
        issue: u64,
        provider: String,
        project: String,
        external_id: u64,
    },
    Unscoped {
        issue: u64,
    },
}

impl NewAssociation {
    pub fn issue(&self) -> u64 {
        match self {
            Self::Task { issue, .. } | Self::Legacy { issue, .. } | Self::Unscoped { issue } => {
                *issue
            }
        }
    }
}

pub struct NewDirectory<'a> {
    pub directory_id: &'a str,
    pub repo_identity: &'a str,
    pub checkout_path: &'a str,
    pub worktree_path: &'a str,
    pub branch: &'a str,
    pub created_at: i64,
    pub session: &'a str,
    pub associations: &'a [NewAssociation],
}

/// Stable directory id for an existing physical directory. Migration and
/// reconciliation both use it, so an upgraded store and a freshly registered
/// directory converge on one id.
pub fn stable_directory_id(repo_identity: &str, canonical_path: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in repo_identity
        .bytes()
        .chain(std::iter::once(0))
        .chain(canonical_path.bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("dir-{hash:016x}")
}

pub fn ensure_schema(storage: &Storage) -> Result<(), String> {
    ensure_registry(storage)
}

/// Every directory of one repository (or every recorded repository when
/// `identity` is `None`). This read never creates, migrates, or drops a store:
/// a legacy store is projected in memory and an absent store is empty.
pub fn load_records(
    storage: &Storage,
    identity: Option<&str>,
) -> Result<Vec<DirectoryRecord>, WorktreeError> {
    let connection = &storage.connection;
    let has_legacy = table_exists(connection, "worktree_leases")
        .map_err(|error| WorktreeError::new("storage", error))?;
    if registry_present(connection)? {
        let mut records = read_registry(connection, identity)?;
        if has_legacy {
            // A mixed store is read as the union of both proven sources. The
            // post-upgrade registry is newer, so it wins shared fields, while
            // every legacy-only directory and association remains visible.
            merge_records(&mut records, read_legacy(connection, identity)?);
        }
        return Ok(records);
    }
    if has_legacy {
        return read_legacy(connection, identity);
    }
    Ok(Vec::new())
}

fn merge_records(records: &mut Vec<DirectoryRecord>, legacy: Vec<DirectoryRecord>) {
    for incoming in legacy {
        if let Some(existing) = records
            .iter_mut()
            .find(|record| record.directory_id == incoming.directory_id)
        {
            for task in incoming.tasks {
                if !existing.tasks.iter().any(|known| {
                    known.task_id == task.task_id
                        && known.issue == task.issue
                        && known.provider == task.provider
                        && known.instance == task.instance
                        && known.project == task.project
                        && known.external_id == task.external_id
                }) {
                    existing.tasks.push(task);
                }
            }
            for issue in incoming.legacy_issues {
                if !existing.legacy_issues.iter().any(|known| {
                    known.issue == issue.issue
                        && known.provider == issue.provider
                        && known.project == issue.project
                        && known.external_id == issue.external_id
                }) {
                    existing.legacy_issues.push(issue);
                }
            }
            for session in incoming.sessions {
                if !existing.sessions.iter().any(|known| known == &session) {
                    existing.sessions.push(session);
                }
            }
        } else {
            records.push(incoming);
        }
    }
    for record in records.iter_mut() {
        record.tasks.sort_by_key(|task| task.task_id);
        record.tasks.dedup();
        record.legacy_issues.sort_by(|left, right| {
            (left.issue, &left.provider, &left.project, left.external_id).cmp(&(
                right.issue,
                &right.provider,
                &right.project,
                right.external_id,
            ))
        });
        record.legacy_issues.dedup();
        record.sessions.sort();
        record.sessions.dedup();
    }
    records.sort_by(|left, right| {
        (left.repo_identity.as_str(), left.worktree_path.as_str())
            .cmp(&(right.repo_identity.as_str(), right.worktree_path.as_str()))
    });
}

fn registry_present(connection: &rusqlite::Connection) -> Result<bool, WorktreeError> {
    let meta = table_exists(connection, "worktree_registry_meta")
        .map_err(|error| WorktreeError::new("storage", error))?;
    if !meta
        || !table_exists(connection, "worktree_directories")
            .map_err(|error| WorktreeError::new("storage", error))?
    {
        return Ok(false);
    }
    let version: Option<String> = connection
        .query_row(
            "SELECT value FROM worktree_registry_meta WHERE name = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| WorktreeError::new("storage", format!("registry version: {error}")))?;
    match version.as_deref() {
        Some(super::registry_schema::REGISTRY_VERSION) => Ok(true),
        Some(other) => Err(WorktreeError::new(
            "storage",
            format!("unsupported worktree registry version '{other}'"),
        )),
        None => Ok(false),
    }
}

fn read_registry(
    connection: &rusqlite::Connection,
    identity: Option<&str>,
) -> Result<Vec<DirectoryRecord>, WorktreeError> {
    let sql = if identity.is_some() {
        "SELECT directory_id, repo_identity, checkout_path, worktree_path, branch, created_at \
           FROM worktree_directories WHERE repo_identity = ?1 ORDER BY worktree_path, directory_id"
    } else {
        "SELECT directory_id, repo_identity, checkout_path, worktree_path, branch, created_at \
           FROM worktree_directories ORDER BY repo_identity, worktree_path, directory_id"
    };
    let mut statement = connection.prepare(sql).map_err(|error| {
        WorktreeError::new("storage", format!("prepare directory read: {error}"))
    })?;
    let decode = |row: &rusqlite::Row<'_>| {
        Ok(DirectoryRecord {
            directory_id: row.get(0)?,
            repo_identity: row.get(1)?,
            checkout_path: row.get(2)?,
            worktree_path: row.get(3)?,
            branch: row.get(4)?,
            created_at: row.get(5)?,
            tasks: Vec::new(),
            legacy_issues: Vec::new(),
            sessions: Vec::new(),
        })
    };
    let mut records = if let Some(identity) = identity {
        statement
            .query_map([identity], decode)
            .map_err(|error| WorktreeError::new("storage", format!("directory read: {error}")))?
            .collect::<Result<Vec<_>, _>>()
    } else {
        statement
            .query_map([], decode)
            .map_err(|error| WorktreeError::new("storage", format!("directory read: {error}")))?
            .collect::<Result<Vec<_>, _>>()
    }
    .map_err(|error| WorktreeError::new("storage", format!("directory row: {error}")))?;
    if records.is_empty() {
        return Ok(records);
    }
    let mut index: std::collections::HashMap<String, usize> = records
        .iter()
        .enumerate()
        .map(|(position, record)| (record.directory_id.clone(), position))
        .collect();
    attach_tasks(connection, &mut records, &index)?;
    attach_legacy(connection, &mut records, &index)?;
    attach_sessions(connection, &mut records, &index)?;
    let _ = &mut index;
    Ok(records)
}

fn attach_tasks(
    connection: &rusqlite::Connection,
    records: &mut [DirectoryRecord],
    index: &std::collections::HashMap<String, usize>,
) -> Result<(), WorktreeError> {
    let mut statement = connection
        .prepare(
            "SELECT t.directory_id, t.task_id, t.issue, b.provider, b.instance, b.project, \
                    b.external_id \
               FROM worktree_directory_tasks t \
               LEFT JOIN task_bindings b ON b.task_id = t.task_id \
              ORDER BY t.directory_id, t.task_id",
        )
        .map_err(|error| WorktreeError::new("storage", format!("prepare task read: {error}")))?;
    let mut rows = statement
        .query([])
        .map_err(|error| WorktreeError::new("storage", format!("task read: {error}")))?;
    while let Some(row) = rows
        .next()
        .map_err(|error| WorktreeError::new("storage", format!("task row: {error}")))?
    {
        let directory_id: String = row.get(0).map_err(row_error)?;
        let Some(position) = index.get(&directory_id) else {
            continue;
        };
        let external: Option<String> = row.get(6).map_err(row_error)?;
        records[*position].tasks.push(DirectoryTask {
            task_id: row.get(1).map_err(row_error)?,
            issue: positive(row.get::<_, i64>(2).map_err(row_error)?),
            provider: row.get(3).map_err(row_error)?,
            instance: row.get(4).map_err(row_error)?,
            project: row.get(5).map_err(row_error)?,
            external_id: external,
        });
    }
    Ok(())
}

fn attach_legacy(
    connection: &rusqlite::Connection,
    records: &mut [DirectoryRecord],
    index: &std::collections::HashMap<String, usize>,
) -> Result<(), WorktreeError> {
    let mut statement = connection
        .prepare(
            "SELECT directory_id, issue, provider, project, external_id \
               FROM worktree_directory_legacy_issues \
              ORDER BY directory_id, issue, provider, project, external_id",
        )
        .map_err(|error| WorktreeError::new("storage", format!("prepare legacy read: {error}")))?;
    let mut rows = statement
        .query([])
        .map_err(|error| WorktreeError::new("storage", format!("legacy read: {error}")))?;
    while let Some(row) = rows
        .next()
        .map_err(|error| WorktreeError::new("storage", format!("legacy row: {error}")))?
    {
        let directory_id: String = row.get(0).map_err(row_error)?;
        let Some(position) = index.get(&directory_id) else {
            continue;
        };
        records[*position].legacy_issues.push(DirectoryLegacyIssue {
            issue: positive(row.get::<_, i64>(1).map_err(row_error)?),
            provider: row.get(2).map_err(row_error)?,
            project: row.get(3).map_err(row_error)?,
            external_id: positive(row.get::<_, i64>(4).map_err(row_error)?),
        });
    }
    Ok(())
}

fn attach_sessions(
    connection: &rusqlite::Connection,
    records: &mut [DirectoryRecord],
    index: &std::collections::HashMap<String, usize>,
) -> Result<(), WorktreeError> {
    let mut statement = connection
        .prepare(
            "SELECT directory_id, session FROM worktree_directory_sessions \
              ORDER BY directory_id, session",
        )
        .map_err(|error| WorktreeError::new("storage", format!("prepare session read: {error}")))?;
    let mut rows = statement
        .query([])
        .map_err(|error| WorktreeError::new("storage", format!("session read: {error}")))?;
    while let Some(row) = rows
        .next()
        .map_err(|error| WorktreeError::new("storage", format!("session row: {error}")))?
    {
        let directory_id: String = row.get(0).map_err(row_error)?;
        let Some(position) = index.get(&directory_id) else {
            continue;
        };
        records[*position]
            .sessions
            .push(row.get(1).map_err(row_error)?);
    }
    Ok(())
}

/// Read a legacy store without writing it. The projection uses the same
/// directory identity a migration would produce, so a preview reports the same
/// directories before and after the one-time upgrade.
fn read_legacy(
    connection: &rusqlite::Connection,
    identity: Option<&str>,
) -> Result<Vec<DirectoryRecord>, WorktreeError> {
    let has_task_id =
        super::registry_schema::column_exists(connection, "worktree_lease_scopes", "task_id")
            .map_err(|error| WorktreeError::new("storage", error))?;
    // A value the new registry cannot represent is refused before projection,
    // so a negative or zero issue never becomes an invented `0` association.
    super::registry_schema::validate_legacy_value_domains(connection)
        .map_err(|error| WorktreeError::new("storage", error))?;
    if has_task_id {
        // A read-only projection is still evidence: a contradictory durable
        // task reference is refused rather than shown as an ordinary legacy
        // scope that a later cleanup could treat as trusted.
        super::registry_schema::validate_legacy_task_identities(connection)
            .map_err(|error| WorktreeError::new("storage", error))?;
    }
    let task_bindings: BTreeMap<i64, (String, String, String, String)> = if has_task_id {
        let mut statement = connection
            .prepare("SELECT task_id, provider, instance, project, external_id FROM task_bindings")
            .map_err(|error| {
                WorktreeError::new("storage", format!("prepare legacy task read: {error}"))
            })?;
        let mut rows = statement
            .query([])
            .map_err(|error| WorktreeError::new("storage", format!("legacy task read: {error}")))?;
        let mut map = BTreeMap::new();
        while let Some(row) = rows
            .next()
            .map_err(|error| WorktreeError::new("storage", format!("legacy task row: {error}")))?
        {
            let task_id: i64 = row.get(0).map_err(row_error)?;
            map.insert(
                task_id,
                (
                    row.get(1).map_err(row_error)?,
                    row.get(2).map_err(row_error)?,
                    row.get(3).map_err(row_error)?,
                    row.get(4).map_err(row_error)?,
                ),
            );
        }
        map
    } else {
        BTreeMap::new()
    };
    let sql = if identity.is_some() {
        "SELECT lease_id, repo_identity, issue, session, checkout_path, worktree_path, branch, \
                created_at FROM worktree_leases WHERE repo_identity = ?1 \
          ORDER BY created_at ASC, lease_id ASC"
    } else {
        "SELECT lease_id, repo_identity, issue, session, checkout_path, worktree_path, branch, \
                created_at FROM worktree_leases ORDER BY created_at ASC, lease_id ASC"
    };
    let mut statement = connection
        .prepare(sql)
        .map_err(|error| WorktreeError::new("storage", format!("prepare legacy read: {error}")))?;
    let mut rows = if let Some(identity) = identity {
        statement
            .query([identity])
            .map_err(|error| WorktreeError::new("storage", format!("legacy read: {error}")))?
    } else {
        statement
            .query([])
            .map_err(|error| WorktreeError::new("storage", format!("legacy read: {error}")))?
    };
    let mut directories: BTreeMap<(String, String), DirectoryRecord> = BTreeMap::new();
    let mut lease_ids: BTreeMap<String, (String, String)> = BTreeMap::new();
    while let Some(row) = rows
        .next()
        .map_err(|error| WorktreeError::new("storage", format!("legacy row: {error}")))?
    {
        let lease_id: String = row.get(0).map_err(row_error)?;
        let repo_identity: String = row.get(1).map_err(row_error)?;
        let session: String = row.get(3).map_err(row_error)?;
        let checkout_path: String = row.get(4).map_err(row_error)?;
        let worktree_path: String = row.get(5).map_err(row_error)?;
        let branch: String = row.get(6).map_err(row_error)?;
        let created_at: i64 = row.get(7).map_err(row_error)?;
        let canonical = super::canonical_or_self(Path::new(&worktree_path))
            .to_string_lossy()
            .into_owned();
        let key = (repo_identity.clone(), canonical.clone());
        let record = directories
            .entry(key.clone())
            .or_insert_with(|| DirectoryRecord {
                directory_id: stable_directory_id(&repo_identity, &canonical),
                repo_identity: repo_identity.clone(),
                checkout_path: checkout_path.clone(),
                worktree_path: canonical.clone(),
                branch: branch.clone(),
                created_at,
                tasks: Vec::new(),
                legacy_issues: Vec::new(),
                sessions: Vec::new(),
            });
        if created_at >= record.created_at {
            record.created_at = created_at;
            record.branch = branch;
            record.checkout_path = checkout_path;
        }
        if !session.trim().is_empty() && !record.sessions.iter().any(|s| s == session.trim()) {
            record.sessions.push(session.trim().to_owned());
        }
        lease_ids.insert(lease_id, key);
    }
    drop(rows);
    drop(statement);
    let scopes = if table_exists(connection, "worktree_lease_scopes")
        .map_err(|error| WorktreeError::new("storage", error))?
    {
        let sql = if has_task_id {
            "SELECT lease_id, provider, project, external_id, task_id FROM worktree_lease_scopes"
        } else {
            "SELECT lease_id, provider, project, external_id, NULL FROM worktree_lease_scopes"
        };
        let mut statement = connection.prepare(sql).map_err(|error| {
            WorktreeError::new("storage", format!("prepare legacy scope read: {error}"))
        })?;
        let mut rows = statement.query([]).map_err(|error| {
            WorktreeError::new("storage", format!("legacy scope read: {error}"))
        })?;
        let mut map: BTreeMap<String, (String, String, u64, Option<i64>)> = BTreeMap::new();
        while let Some(row) = rows
            .next()
            .map_err(|error| WorktreeError::new("storage", format!("legacy scope row: {error}")))?
        {
            map.insert(
                row.get(0).map_err(row_error)?,
                (
                    row.get(1).map_err(row_error)?,
                    row.get(2).map_err(row_error)?,
                    positive(row.get::<_, i64>(3).map_err(row_error)?),
                    row.get(4).map_err(row_error)?,
                ),
            );
        }
        map
    } else {
        BTreeMap::new()
    };
    // Attach each legacy row's association to its directory. The fetched row
    // values are re-read here because the directory map no longer keeps the
    // per-row issue; a second narrow pass over the table keeps that simple.
    let mut statement = connection
        .prepare(
            "SELECT lease_id, issue FROM worktree_leases ORDER BY created_at ASC, lease_id ASC",
        )
        .map_err(|error| {
            WorktreeError::new("storage", format!("prepare legacy issue read: {error}"))
        })?;
    let mut rows = statement
        .query([])
        .map_err(|error| WorktreeError::new("storage", format!("legacy issue read: {error}")))?;
    while let Some(row) = rows
        .next()
        .map_err(|error| WorktreeError::new("storage", format!("legacy issue row: {error}")))?
    {
        let lease_id: String = row.get(0).map_err(row_error)?;
        let issue = positive(row.get::<_, i64>(1).map_err(row_error)?);
        let Some(key) = lease_ids.get(&lease_id) else {
            continue;
        };
        let Some(record) = directories.get_mut(key) else {
            continue;
        };
        match scopes.get(&lease_id) {
            Some((provider, project, external_id, Some(task_id))) => {
                let Some((binding_provider, instance, binding_project, binding_external)) =
                    task_bindings.get(task_id)
                else {
                    return Err(WorktreeError::new(
                        "storage",
                        format!(
                            "legacy task {task_id} has no durable binding; the store cannot be read safely"
                        ),
                    ));
                };
                if binding_provider != provider
                    || binding_project != project
                    || binding_external != &external_id.to_string()
                    || *external_id != issue
                    || instance.trim().is_empty()
                {
                    return Err(WorktreeError::new(
                        "storage",
                        format!(
                            "legacy task {task_id} disagrees with its recorded scope and issue; the store cannot be read safely"
                        ),
                    ));
                }
                record.tasks.push(DirectoryTask {
                    task_id: *task_id,
                    issue,
                    provider: Some(provider.clone()),
                    instance: Some(instance.clone()),
                    project: Some(project.clone()),
                    external_id: Some(external_id.to_string()),
                });
            }
            Some((provider, project, external_id, None)) => {
                record.legacy_issues.push(DirectoryLegacyIssue {
                    issue,
                    provider: provider.clone(),
                    project: project.clone(),
                    external_id: *external_id,
                });
            }
            None => record.legacy_issues.push(DirectoryLegacyIssue {
                issue,
                provider: String::new(),
                project: String::new(),
                external_id: 0,
            }),
        }
    }
    let mut records: Vec<DirectoryRecord> = directories.into_values().collect();
    for record in &mut records {
        record.tasks.sort_by_key(|task| task.task_id);
        record.tasks.dedup();
        record.legacy_issues.sort_by(|left, right| {
            (left.issue, &left.provider, &left.project, left.external_id).cmp(&(
                right.issue,
                &right.provider,
                &right.project,
                right.external_id,
            ))
        });
        record.legacy_issues.dedup();
        record.sessions.sort();
        record.sessions.dedup();
    }
    Ok(records)
}

fn positive(value: i64) -> u64 {
    value.max(0) as u64
}

fn row_error(error: rusqlite::Error) -> WorktreeError {
    WorktreeError::new("storage", format!("directory row: {error}"))
}

/// Persist one directory and its first association in one immediate
/// transaction. A duplicate physical path under the same repository is a
/// conflict, never silently merged.
pub fn insert_directory(storage: &Storage, new: &NewDirectory<'_>) -> Result<(), WorktreeError> {
    validate_new(new)?;
    let transaction = rusqlite::Transaction::new_unchecked(
        &storage.connection,
        rusqlite::TransactionBehavior::Immediate,
    )
    .map_err(|error| WorktreeError::new("storage", format!("begin directory insert: {error}")))?;
    transaction
        .execute(
            "INSERT INTO worktree_directories \
                (directory_id, repo_identity, checkout_path, worktree_path, branch, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                new.directory_id,
                new.repo_identity,
                new.checkout_path,
                new.worktree_path,
                new.branch,
                new.created_at
            ],
        )
        .map_err(|error| WorktreeError::new("storage", format!("insert directory: {error}")))?;
    insert_associations(&transaction, new)?;
    transaction
        .commit()
        .map_err(|error| WorktreeError::new("storage", format!("commit directory: {error}")))
}

fn insert_associations(
    connection: &rusqlite::Connection,
    new: &NewDirectory<'_>,
) -> Result<(), WorktreeError> {
    for association in new.associations {
        match association {
            NewAssociation::Task {
                task_id,
                issue,
                provider,
                project,
                external_id,
            } => {
                let binding = crate::task_identity::binding_for_task_id_on(connection, *task_id)
                    .map_err(|error| WorktreeError::new("storage", error))?
                    .ok_or_else(|| {
                        WorktreeError::new(
                            "storage",
                            "the directory references an unknown durable task",
                        )
                    })?;
                let key = binding.key();
                if key.provider() != provider
                    || key.project() != project
                    || key.external_number() != Some(*external_id)
                    || key.external_number() != Some(*issue)
                {
                    return Err(WorktreeError::new(
                        "storage",
                        "the directory association does not match its durable task",
                    ));
                }
                let repo_key = verified_repo_key(new.repo_identity).ok_or_else(|| {
                    WorktreeError::new(
                        "storage",
                        "a durable task directory requires a verified repository key",
                    )
                })?;
                if !crate::task_identity::task_repo_membership_exists_on(
                    connection, &repo_key, *task_id,
                )
                .map_err(|error| WorktreeError::new("storage", error))?
                {
                    return Err(WorktreeError::new(
                        "storage",
                        "the durable task is not a member of this repository",
                    ));
                }
                connection
                    .execute(
                        "INSERT OR IGNORE INTO worktree_directory_tasks \
                            (directory_id, task_id, issue, created_at) VALUES (?1, ?2, ?3, ?4)",
                        rusqlite::params![new.directory_id, task_id, *issue as i64, new.created_at],
                    )
                    .map_err(|error| {
                        WorktreeError::new("storage", format!("insert task association: {error}"))
                    })?;
            }
            NewAssociation::Legacy {
                issue,
                provider,
                project,
                external_id,
            } => insert_legacy_association(
                connection,
                new.directory_id,
                *issue,
                provider,
                project,
                *external_id,
                new.created_at,
            )?,
            NewAssociation::Unscoped { issue } => insert_legacy_association(
                connection,
                new.directory_id,
                *issue,
                "",
                "",
                0,
                new.created_at,
            )?,
        }
    }
    if !new.session.trim().is_empty() {
        connection
            .execute(
                "INSERT OR IGNORE INTO worktree_directory_sessions \
                    (directory_id, session, created_at) VALUES (?1, ?2, ?3)",
                rusqlite::params![new.directory_id, new.session.trim(), new.created_at],
            )
            .map_err(|error| {
                WorktreeError::new("storage", format!("insert directory session: {error}"))
            })?;
    }
    Ok(())
}

fn insert_legacy_association(
    connection: &rusqlite::Connection,
    directory_id: &str,
    issue: u64,
    provider: &str,
    project: &str,
    external_id: u64,
    created_at: i64,
) -> Result<(), WorktreeError> {
    connection
        .execute(
            "INSERT OR IGNORE INTO worktree_directory_legacy_issues \
                (directory_id, issue, provider, project, external_id, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                directory_id,
                issue as i64,
                provider,
                project,
                external_id as i64,
                created_at
            ],
        )
        .map_err(|error| {
            WorktreeError::new("storage", format!("insert legacy association: {error}"))
        })
        .map(|_| ())
}

/// Attach an issue and session to an existing directory in one short
/// transaction. Used by the idempotent acquire path.
pub fn attach_association(
    storage: &Storage,
    directory_id: &str,
    repo_identity: &str,
    created_at: i64,
    session: &str,
    association: &NewAssociation,
) -> Result<(), WorktreeError> {
    let transaction = rusqlite::Transaction::new_unchecked(
        &storage.connection,
        rusqlite::TransactionBehavior::Immediate,
    )
    .map_err(|error| WorktreeError::new("storage", format!("begin directory attach: {error}")))?;
    let new = NewDirectory {
        directory_id,
        repo_identity,
        checkout_path: "",
        worktree_path: "",
        branch: "",
        created_at,
        session,
        associations: std::slice::from_ref(association),
    };
    // insert_associations only reads the association, the session and the
    // verified repository key; the path fields are inert here.
    insert_associations(&transaction, &new)?;
    transaction
        .commit()
        .map_err(|error| WorktreeError::new("storage", format!("commit directory attach: {error}")))
}

/// Delete one directory's registry evidence after its physical directory was
/// removed. Associations and sessions cascade.
pub fn remove_directory(
    storage: &Storage,
    repo_identity: &str,
    directory_id: &str,
) -> Result<u64, WorktreeError> {
    let transaction = rusqlite::Transaction::new_unchecked(
        &storage.connection,
        rusqlite::TransactionBehavior::Immediate,
    )
    .map_err(|error| WorktreeError::new("storage", format!("begin directory removal: {error}")))?;
    let removed = transaction
        .execute(
            "DELETE FROM worktree_directories WHERE repo_identity = ?1 AND directory_id = ?2",
            rusqlite::params![repo_identity, directory_id],
        )
        .map_err(|error| {
            WorktreeError::new("storage", format!("delete directory record: {error}"))
        })?;
    transaction.commit().map_err(|error| {
        WorktreeError::new("storage", format!("commit directory removal: {error}"))
    })?;
    Ok(removed as u64)
}

/// Every recorded repository identity, in deterministic order.
pub fn repo_identities(storage: &Storage) -> Result<Vec<String>, WorktreeError> {
    let mut identities = Vec::new();
    if registry_present(&storage.connection)? {
        identities.extend(scan_identities(
            storage,
            "worktree_directories",
            "prepare repo scan",
        )?);
    }
    if table_exists(&storage.connection, "worktree_leases")
        .map_err(|error| WorktreeError::new("storage", error))?
    {
        identities.extend(scan_identities(
            storage,
            "worktree_leases",
            "prepare legacy repo scan",
        )?);
    }
    identities.sort();
    identities.dedup();
    Ok(identities)
}

fn scan_identities(
    storage: &Storage,
    table: &str,
    what: &str,
) -> Result<Vec<String>, WorktreeError> {
    let sql = format!("SELECT DISTINCT repo_identity FROM {table} ORDER BY repo_identity");
    let mut statement = storage
        .connection
        .prepare(&sql)
        .map_err(|error| WorktreeError::new("storage", format!("{what}: {error}")))?;
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

pub fn records_for_issue(
    storage: &Storage,
    identity: Option<&str>,
    issue: u64,
) -> Result<Vec<DirectoryRecord>, WorktreeError> {
    let mut records = load_records(storage, identity)?;
    records.retain(|record| record.has_issue(issue));
    Ok(records)
}

pub fn record_for_path(
    storage: &Storage,
    identity: &str,
    path: &Path,
) -> Result<Option<DirectoryRecord>, WorktreeError> {
    let canonical = super::canonical_or_self(path);
    Ok(load_records(storage, Some(identity))?
        .into_iter()
        .find(|record| record.canonical_path() == canonical))
}

pub fn record_for_issue_session(
    storage: &Storage,
    identity: &str,
    issue: u64,
    session: &str,
) -> Result<Option<DirectoryRecord>, WorktreeError> {
    Ok(load_records(storage, Some(identity))?
        .into_iter()
        .rev()
        .find(|record| record.has_issue(issue) && record.has_session(session)))
}

pub fn page_records(
    storage: &Storage,
    identity: Option<&str>,
    issue: Option<u64>,
    request: PageRequest,
) -> Result<(Vec<DirectoryRecord>, usize), WorktreeError> {
    let mut records = load_records(storage, identity)?;
    if let Some(issue) = issue {
        records.retain(|record| record.has_issue(issue));
    }
    let total = records.len();
    let rows = records
        .into_iter()
        .skip(request.offset())
        .take(request.limit())
        .collect();
    Ok((rows, total))
}

fn validate_new(new: &NewDirectory<'_>) -> Result<(), WorktreeError> {
    if new.directory_id.trim().is_empty()
        || new.repo_identity.trim().is_empty()
        || new.worktree_path.trim().is_empty()
    {
        return Err(WorktreeError::new(
            "argument",
            "a directory needs an id, repository identity and path",
        ));
    }
    if new.associations.is_empty() {
        return Err(WorktreeError::new(
            "argument",
            "a directory needs at least one task or issue association",
        ));
    }
    if new
        .associations
        .iter()
        .any(|association| association.issue() == 0)
    {
        return Err(WorktreeError::new(
            "argument",
            "directory associations need a positive issue number",
        ));
    }
    Ok(())
}

pub(crate) fn verified_repo_key(identity: &str) -> Option<String> {
    super::scope::verified_repo_key_for_identity(identity)
}
