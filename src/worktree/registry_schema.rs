//! Directory-registry DDL and the one-time legacy-store upgrade.
//!
//! The registry records physical worktree directories, the durable tasks or
//! unknown legacy issue identities associated with them, and the sessions
//! observed in them. It carries no lease state: no status, heartbeat,
//! expiration, or release reason. Runtime occupancy comes from the connected
//! OpenCode service and cleanup eligibility from provider-confirmed closure.

use crate::infra::storage::Storage;

pub(super) const REGISTRY_VERSION: &str = "1";
pub(super) const VERSION_KEY: &str = "schema_version";

pub(super) const DIRECTORY_COLUMNS: &[&str] = &[
    "directory_id",
    "repo_identity",
    "checkout_path",
    "worktree_path",
    "branch",
    "created_at",
];
pub(super) const TASK_COLUMNS: &[&str] = &["directory_id", "task_id", "issue", "created_at"];
pub(super) const LEGACY_COLUMNS: &[&str] = &[
    "directory_id",
    "issue",
    "provider",
    "project",
    "external_id",
    "created_at",
];
pub(super) const SESSION_COLUMNS: &[&str] = &["directory_id", "session", "created_at"];

pub(super) const REGISTRY_SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS worktree_registry_meta (
    name TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS worktree_directories (
    directory_id TEXT PRIMARY KEY,
    repo_identity TEXT NOT NULL CHECK (length(repo_identity) > 0),
    checkout_path TEXT NOT NULL,
    worktree_path TEXT NOT NULL CHECK (length(worktree_path) > 0),
    branch TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE (repo_identity, worktree_path)
);

CREATE INDEX IF NOT EXISTS worktree_directories_repo_idx
    ON worktree_directories (repo_identity, worktree_path);

CREATE TABLE IF NOT EXISTS worktree_directory_tasks (
    directory_id TEXT NOT NULL REFERENCES worktree_directories(directory_id) ON DELETE CASCADE,
    task_id INTEGER NOT NULL CHECK (task_id > 0),
    issue INTEGER NOT NULL CHECK (issue > 0),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (directory_id, task_id)
);

CREATE INDEX IF NOT EXISTS worktree_directory_tasks_task_idx
    ON worktree_directory_tasks (task_id);
CREATE INDEX IF NOT EXISTS worktree_directory_tasks_issue_idx
    ON worktree_directory_tasks (issue);

CREATE TABLE IF NOT EXISTS worktree_directory_legacy_issues (
    directory_id TEXT NOT NULL REFERENCES worktree_directories(directory_id) ON DELETE CASCADE,
    issue INTEGER NOT NULL CHECK (issue > 0),
    provider TEXT NOT NULL DEFAULT '',
    project TEXT NOT NULL DEFAULT '',
    external_id INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (directory_id, issue, provider, project, external_id)
);

CREATE INDEX IF NOT EXISTS worktree_directory_legacy_issues_issue_idx
    ON worktree_directory_legacy_issues (issue);

CREATE TABLE IF NOT EXISTS worktree_directory_sessions (
    directory_id TEXT NOT NULL REFERENCES worktree_directories(directory_id) ON DELETE CASCADE,
    session TEXT NOT NULL CHECK (length(session) > 0),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (directory_id, session)
);

CREATE INDEX IF NOT EXISTS worktree_directory_sessions_session_idx
    ON worktree_directory_sessions (session);
";

/// Create the registry on a write-capable store or upgrade one legacy store.
/// The whole upgrade is one immediate transaction: it either copies every
/// legacy row and drops the lease tables or leaves the store untouched.
pub(super) fn ensure_registry(storage: &Storage) -> Result<(), String> {
    let connection = &storage.connection;
    connection
        .execute("BEGIN IMMEDIATE", [])
        .map_err(|error| format!("could not begin worktree registry upgrade: {error}"))?;
    let result = upgrade(connection);
    match result {
        Ok(()) => connection
            .execute("COMMIT", [])
            .map(|_| ())
            .map_err(|error| format!("could not commit worktree registry upgrade: {error}")),
        Err(error) => {
            let _ = connection.execute("ROLLBACK", []);
            Err(error)
        }
    }
}

fn upgrade(connection: &rusqlite::Connection) -> Result<(), String> {
    connection
        .execute_batch(REGISTRY_SCHEMA)
        .map_err(|error| format!("could not initialise worktree registry: {error}"))?;
    let version = meta_value(connection, VERSION_KEY)?;
    if let Some(version) = &version
        && version != REGISTRY_VERSION
    {
        return Err(format!(
            "unsupported worktree registry version '{version}' (expected {REGISTRY_VERSION})"
        ));
    }
    // A legacy table that still exists always gets imported, even when a
    // marker already exists: a marker written by an earlier incomplete run
    // must never let its unconsumed rows be dropped unattested. Import is
    // idempotent, so a second pass converges on the same registry rows.
    if table_exists(connection, "worktree_leases")?
        || table_exists(connection, "worktree_lease_scopes")?
    {
        validate_legacy_shape(connection)?;
        import_legacy(connection)?;
        set_meta(connection, VERSION_KEY, REGISTRY_VERSION)?;
    } else if version.is_none() {
        set_meta(connection, VERSION_KEY, REGISTRY_VERSION)?;
    }
    validate_shape(connection)?;
    // A store upgraded by this transaction has already dropped the lease
    // tables; a marker-bearing store that still carries them is converged here
    // so the final runtime never keeps a lease table beside the registry.
    drop_legacy_tables(connection)?;
    Ok(())
}

fn import_legacy(connection: &rusqlite::Connection) -> Result<(), String> {
    if !table_exists(connection, "worktree_leases")? {
        return Ok(());
    }
    let has_task_id = column_exists(connection, "worktree_lease_scopes", "task_id")?;
    let sql = "SELECT lease_id, repo_identity, issue, session, checkout_path, worktree_path, \
               branch, created_at FROM worktree_leases ORDER BY created_at ASC, lease_id ASC";
    let mut statement = connection
        .prepare(sql)
        .map_err(|error| format!("could not read legacy worktree rows: {error}"))?;
    let mut rows = statement
        .query([])
        .map_err(|error| format!("could not read legacy worktree rows: {error}"))?;
    let mut legacy = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|error| format!("could not read legacy worktree row: {error}"))?
    {
        let lease_id: String = row.get(0).map_err(row_error)?;
        let identity: String = row.get(1).map_err(row_error)?;
        let issue: i64 = row.get(2).map_err(row_error)?;
        if issue <= 0 {
            return Err(format!(
                "legacy lease '{lease_id}' has an unrepresentable issue number {issue}; the legacy store was left untouched"
            ));
        }
        let session: String = row.get(3).map_err(row_error)?;
        let checkout: String = row.get(4).map_err(row_error)?;
        let path: String = row.get(5).map_err(row_error)?;
        let branch: String = row.get(6).map_err(row_error)?;
        let created: i64 = row.get(7).map_err(row_error)?;
        legacy.push(LegacyRow {
            lease_id,
            identity,
            issue: issue as u64,
            session,
            checkout,
            path,
            branch,
            created,
        });
    }
    drop(rows);
    drop(statement);

    let scope_sql = if has_task_id {
        "SELECT s.lease_id, s.provider, s.project, s.external_id, s.task_id \
           FROM worktree_lease_scopes s"
    } else {
        "SELECT s.lease_id, s.provider, s.project, s.external_id, NULL \
           FROM worktree_lease_scopes s"
    };
    let scopes = if table_exists(connection, "worktree_lease_scopes")? {
        let mut statement = connection
            .prepare(scope_sql)
            .map_err(|error| format!("could not read legacy worktree scopes: {error}"))?;
        let mut rows = statement
            .query([])
            .map_err(|error| format!("could not read legacy worktree scopes: {error}"))?;
        let mut scopes: std::collections::HashMap<String, LegacyScope> =
            std::collections::HashMap::new();
        while let Some(row) = rows
            .next()
            .map_err(|error| format!("could not read legacy worktree scope: {error}"))?
        {
            let lease_id: String = row.get(0).map_err(row_error)?;
            let provider: String = row.get(1).map_err(row_error)?;
            let project: String = row.get(2).map_err(row_error)?;
            let external: i64 = row.get(3).map_err(row_error)?;
            if external <= 0 {
                return Err(format!(
                    "legacy lease '{lease_id}' has an unrepresentable scoped external id {external}; the legacy store was left untouched"
                ));
            }
            let task_id: Option<i64> = row.get(4).map_err(row_error)?;
            scopes.insert(
                lease_id,
                LegacyScope {
                    provider,
                    project,
                    external_id: external as u64,
                    task_id,
                },
            );
        }
        scopes
    } else {
        std::collections::HashMap::new()
    };

    // Regroup by physical directory. The row's own path is canonicalised with
    // the same filesystem fallback every runtime comparison uses, so an absent
    // legacy path still maps to exactly one stable directory identity.
    use std::collections::BTreeMap;
    let mut directories: BTreeMap<(String, String), DirectoryImport> = BTreeMap::new();
    for row in legacy {
        let canonical = crate::worktree::canonical_or_self(std::path::Path::new(&row.path))
            .to_string_lossy()
            .into_owned();
        let key = (row.identity.clone(), canonical.clone());
        let directory = directories.entry(key).or_insert_with(|| DirectoryImport {
            identity: row.identity.clone(),
            path: canonical,
            checkout: row.checkout.clone(),
            branch: row.branch.clone(),
            created: row.created,
            associations: BTreeMap::new(),
            sessions: BTreeMap::new(),
        });
        if row.created >= directory.created {
            directory.branch = row.branch.clone();
            directory.created = row.created;
            directory.checkout = row.checkout.clone();
        }
        if !row.session.trim().is_empty() {
            directory
                .sessions
                .insert(row.session.trim().to_owned(), row.created);
        }
        let association = match scopes.get(&row.lease_id) {
            Some(scope) => AssociationImport::from_scope(row.issue, scope)?,
            None => AssociationImport::unscoped(row.issue),
        };
        directory
            .associations
            .entry(association.key())
            .or_insert((association, row.created));
    }

    for directory in directories.into_values() {
        let directory_id =
            crate::worktree::registry::stable_directory_id(&directory.identity, &directory.path);
        connection
            .execute(
                "INSERT INTO worktree_directories \
                    (directory_id, repo_identity, checkout_path, worktree_path, branch, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT(directory_id) DO NOTHING",
                rusqlite::params![
                    directory_id,
                    directory.identity,
                    directory.checkout,
                    directory.path,
                    directory.branch,
                    directory.created
                ],
            )
            .map_err(|error| format!("could not import legacy worktree directory: {error}"))?;
        for (session, created) in directory.sessions {
            connection
                .execute(
                    "INSERT INTO worktree_directory_sessions \
                        (directory_id, session, created_at) VALUES (?1, ?2, ?3) \
                     ON CONFLICT(directory_id, session) DO NOTHING",
                    rusqlite::params![directory_id, session, created],
                )
                .map_err(|error| format!("could not import legacy worktree session: {error}"))?;
        }
        for (association, created) in directory.associations.into_values() {
            association.insert(connection, &directory_id, created)?;
        }
    }
    Ok(())
}

struct LegacyRow {
    lease_id: String,
    identity: String,
    issue: u64,
    session: String,
    checkout: String,
    path: String,
    branch: String,
    created: i64,
}

struct LegacyScope {
    provider: String,
    project: String,
    external_id: u64,
    task_id: Option<i64>,
}

struct DirectoryImport {
    identity: String,
    path: String,
    checkout: String,
    branch: String,
    created: i64,
    associations: std::collections::BTreeMap<String, (AssociationImport, i64)>,
    sessions: std::collections::BTreeMap<String, i64>,
}

enum AssociationImport {
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
}

impl AssociationImport {
    fn from_scope(issue: u64, scope: &LegacyScope) -> Result<Self, String> {
        if let Some(task_id) = scope.task_id {
            if task_id <= 0 {
                return Err("legacy worktree scope has an invalid task id".to_owned());
            }
            return Ok(Self::Task {
                task_id,
                issue,
                provider: scope.provider.clone(),
                project: scope.project.clone(),
                external_id: scope.external_id,
            });
        }
        Ok(Self::Legacy {
            issue,
            provider: scope.provider.clone(),
            project: scope.project.clone(),
            external_id: scope.external_id,
        })
    }

    fn unscoped(issue: u64) -> Self {
        Self::Legacy {
            issue,
            provider: String::new(),
            project: String::new(),
            external_id: 0,
        }
    }

    fn key(&self) -> String {
        match self {
            Self::Task {
                task_id,
                issue,
                provider,
                project,
                external_id,
            } => format!("task:{task_id}:{issue}:{provider}:{project}:{external_id}"),
            Self::Legacy {
                issue,
                provider,
                project,
                external_id,
            } => format!("legacy:{issue}:{provider}:{project}:{external_id}"),
        }
    }

    fn insert(
        &self,
        connection: &rusqlite::Connection,
        directory_id: &str,
        created: i64,
    ) -> Result<(), String> {
        match self {
            Self::Task {
                task_id,
                issue,
                provider,
                project,
                external_id,
            } => {
                let binding = crate::task_identity::binding_for_task_id_on(connection, *task_id)
                    .map_err(|error| format!("could not read legacy task binding: {error}"))?;
                match binding {
                    Some(binding) => {
                        let key = binding.key();
                        if key.provider() != provider
                            || key.project() != project
                            || key.external_number() != Some(*external_id)
                            || key.external_number() != Some(*issue)
                        {
                            // Contradictory legacy evidence must refuse the
                            // whole upgrade: downgrading it to an unknown row
                            // would silently discard the task reference.
                            return Err(format!(
                                "legacy task {task_id} disagrees with its recorded issue and scope; the legacy store was left untouched"
                            ));
                        }
                    }
                    None => {
                        return Err(format!(
                            "legacy task {task_id} has no durable binding; the legacy store was left untouched"
                        ));
                    }
                }
                connection
                    .execute(
                        "INSERT INTO worktree_directory_tasks \
                            (directory_id, task_id, issue, created_at) VALUES (?1, ?2, ?3, ?4) \
                         ON CONFLICT(directory_id, task_id) DO NOTHING",
                        rusqlite::params![directory_id, task_id, *issue as i64, created],
                    )
                    .map_err(|error| {
                        format!("could not import legacy task association: {error}")
                    })?;
                Ok(())
            }
            Self::Legacy {
                issue,
                provider,
                project,
                external_id,
            } => insert_legacy(
                connection,
                directory_id,
                *issue,
                provider,
                project,
                *external_id,
                created,
            ),
        }
    }
}

fn insert_legacy(
    connection: &rusqlite::Connection,
    directory_id: &str,
    issue: u64,
    provider: &str,
    project: &str,
    external_id: u64,
    created: i64,
) -> Result<(), String> {
    connection
        .execute(
            "INSERT INTO worktree_directory_legacy_issues \
                (directory_id, issue, provider, project, external_id, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(directory_id, issue, provider, project, external_id) DO NOTHING",
            rusqlite::params![
                directory_id,
                issue as i64,
                provider,
                project,
                external_id as i64,
                created
            ],
        )
        .map_err(|error| format!("could not import legacy issue association: {error}"))?;
    Ok(())
}

/// The legacy schemas this build can read. A shape it does not recognise is
/// refused before any row is copied or any table is dropped, so the old store
/// stays intact and an operator can inspect it.
fn validate_legacy_shape(connection: &rusqlite::Connection) -> Result<(), String> {
    const LEASE_COLUMNS: &[&str] = &[
        "lease_id",
        "repo_identity",
        "issue",
        "session",
        "checkout_path",
        "worktree_path",
        "branch",
        "created_at",
    ];
    if table_exists(connection, "worktree_leases")? {
        for column in LEASE_COLUMNS {
            if !column_exists(connection, "worktree_leases", column)? {
                return Err(format!(
                    "unsupported worktree lease schema: missing column '{column}'; the legacy store was left untouched"
                ));
            }
        }
    }
    if table_exists(connection, "worktree_lease_scopes")? {
        for column in ["lease_id", "provider", "project", "external_id"] {
            if !column_exists(connection, "worktree_lease_scopes", column)? {
                return Err(format!(
                    "unsupported worktree lease scope schema: missing column '{column}'; the legacy store was left untouched"
                ));
            }
        }
        if !table_exists(connection, "worktree_leases")? {
            let orphaned: i64 = connection
                .query_row("SELECT COUNT(*) FROM worktree_lease_scopes", [], |row| {
                    row.get(0)
                })
                .map_err(|error| format!("could not inspect legacy worktree scopes: {error}"))?;
            if orphaned > 0 {
                return Err(format!(
                    "worktree lease scopes without their lease rows cannot be imported ({orphaned} row(s)); the legacy store was left untouched"
                ));
            }
        } else {
            let orphaned: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM worktree_lease_scopes s \
                       LEFT JOIN worktree_leases l ON l.lease_id = s.lease_id \
                      WHERE l.lease_id IS NULL",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| format!("could not inspect legacy worktree scopes: {error}"))?;
            if orphaned > 0 {
                return Err(format!(
                    "legacy worktree scope rows reference a missing lease ({orphaned} row(s)); the legacy store was left untouched"
                ));
            }
        }
        validate_legacy_task_identities(connection)?;
    }
    validate_legacy_value_domains(connection)?;
    Ok(())
}

/// A non-null legacy task reference is authoritative only while it still names
/// the exact durable binding it recorded. A missing, non-positive or
/// contradictory binding refuses the whole upgrade and leaves the old tables
/// intact; downgrading it to an ordinary legacy scope would turn untrusted
/// evidence into permission. The lease's own issue number is part of the
/// tuple: a task that names a different issue than the lease is contradictory
/// evidence, never an ordinary association.
pub(super) fn validate_legacy_task_identities(
    connection: &rusqlite::Connection,
) -> Result<(), String> {
    if !table_exists(connection, "worktree_lease_scopes")?
        || !column_exists(connection, "worktree_lease_scopes", "task_id")?
    {
        return Ok(());
    }
    let mut statement = connection
        .prepare(
            "SELECT s.lease_id, s.provider, s.project, s.external_id, s.task_id, \
                    b.provider, b.project, b.external_id, b.instance, \
                    l.issue \
               FROM worktree_lease_scopes s \
               LEFT JOIN task_bindings b ON b.task_id = s.task_id \
               LEFT JOIN worktree_leases l ON l.lease_id = s.lease_id \
              WHERE s.task_id IS NOT NULL",
        )
        .map_err(|error| format!("could not inspect legacy task references: {error}"))?;
    let mut rows = statement
        .query([])
        .map_err(|error| format!("could not inspect legacy task references: {error}"))?;
    while let Some(row) = rows
        .next()
        .map_err(|error| format!("could not read legacy task reference: {error}"))?
    {
        let lease_id: String = row.get(0).map_err(row_error)?;
        let provider: String = row.get(1).map_err(row_error)?;
        let project: String = row.get(2).map_err(row_error)?;
        let external_id: i64 = row.get(3).map_err(row_error)?;
        let task_id: i64 = row.get(4).map_err(row_error)?;
        if task_id <= 0 {
            return Err(format!(
                "legacy lease '{lease_id}' has an invalid durable task id {task_id}; the legacy store was left untouched"
            ));
        }
        let binding_provider: Option<String> = row.get(5).map_err(row_error)?;
        let Some(binding_provider) = binding_provider else {
            return Err(format!(
                "legacy lease '{lease_id}' references durable task {task_id} with no binding; the legacy store was left untouched"
            ));
        };
        let binding_project: String = row.get(6).map_err(row_error)?;
        let binding_external: String = row.get(7).map_err(row_error)?;
        let binding_instance: Option<String> = row.get(8).map_err(row_error)?;
        let lease_issue: Option<i64> = row.get(9).map_err(row_error)?;
        let Some(binding_instance) = binding_instance.filter(|value| !value.trim().is_empty())
        else {
            return Err(format!(
                "legacy lease '{lease_id}' durable task {task_id} has no instance identity; the legacy store was left untouched"
            ));
        };
        let Some(lease_issue) = lease_issue else {
            return Err(format!(
                "legacy lease '{lease_id}' has a dangling scope without its lease row; the legacy store was left untouched"
            ));
        };
        if lease_issue <= 0 {
            return Err(format!(
                "legacy lease '{lease_id}' has an unrepresentable issue number {lease_issue}; the legacy store was left untouched"
            ));
        }
        if external_id <= 0 {
            return Err(format!(
                "legacy lease '{lease_id}' has an unrepresentable scoped external id {external_id}; the legacy store was left untouched"
            ));
        }
        if binding_provider != provider
            || binding_project != project
            || binding_external != external_id.to_string()
            || lease_issue.to_string() != binding_external
            || binding_instance.trim().is_empty()
        {
            return Err(format!(
                "legacy lease '{lease_id}' durable task {task_id} disagrees with its recorded scope and issue; the legacy store was left untouched"
            ));
        }
    }
    Ok(())
}

/// Every legacy row must have a representable issue number, and every scoped
/// row a representable external id.
///
/// The supported old schema has no positive CHECK, so a zero or negative value
/// is possible. The new registry's CHECK cannot represent it and an
/// `INSERT OR IGNORE` would silently drop the row while the migration still
/// drops the old table. Refusing here keeps the original evidence intact.
pub(super) fn validate_legacy_value_domains(
    connection: &rusqlite::Connection,
) -> Result<(), String> {
    if table_exists(connection, "worktree_leases")? {
        let mut statement = connection
            .prepare("SELECT lease_id, issue FROM worktree_leases WHERE issue <= 0")
            .map_err(|error| format!("could not inspect legacy issue values: {error}"))?;
        let mut rows = statement
            .query([])
            .map_err(|error| format!("could not inspect legacy issue values: {error}"))?;
        if let Some(row) = rows
            .next()
            .map_err(|error| format!("could not read legacy issue value: {error}"))?
        {
            let lease_id: String = row.get(0).map_err(row_error)?;
            let issue: i64 = row.get(1).map_err(row_error)?;
            return Err(format!(
                "legacy lease '{lease_id}' has an unrepresentable issue number {issue}; the legacy store was left untouched"
            ));
        }
    }
    if table_exists(connection, "worktree_lease_scopes")?
        && column_exists(connection, "worktree_lease_scopes", "external_id")?
    {
        let mut statement = connection
            .prepare(
                "SELECT lease_id, external_id FROM worktree_lease_scopes WHERE external_id <= 0",
            )
            .map_err(|error| format!("could not inspect legacy scope values: {error}"))?;
        let mut rows = statement
            .query([])
            .map_err(|error| format!("could not inspect legacy scope values: {error}"))?;
        if let Some(row) = rows
            .next()
            .map_err(|error| format!("could not read legacy scope value: {error}"))?
        {
            let lease_id: String = row.get(0).map_err(row_error)?;
            let external_id: i64 = row.get(1).map_err(row_error)?;
            return Err(format!(
                "legacy lease '{lease_id}' has an unrepresentable scoped external id {external_id}; the legacy store was left untouched"
            ));
        }
        if table_exists(connection, "worktree_leases")? {
            let mut statement = connection
                .prepare(
                    "SELECT s.lease_id, s.external_id, l.issue \
                       FROM worktree_lease_scopes s \
                       JOIN worktree_leases l ON l.lease_id = s.lease_id \
                      WHERE s.external_id != l.issue",
                )
                .map_err(|error| format!("could not inspect legacy scope identity: {error}"))?;
            let mut rows = statement
                .query([])
                .map_err(|error| format!("could not inspect legacy scope identity: {error}"))?;
            if let Some(row) = rows
                .next()
                .map_err(|error| format!("could not read legacy scope identity: {error}"))?
            {
                let lease_id: String = row.get(0).map_err(row_error)?;
                let external_id: i64 = row.get(1).map_err(row_error)?;
                let issue: i64 = row.get(2).map_err(row_error)?;
                return Err(format!(
                    "legacy lease '{lease_id}' scoped external id {external_id} disagrees with its lease issue {issue}; the legacy store was left untouched"
                ));
            }
        }
    }
    Ok(())
}

fn drop_legacy_tables(connection: &rusqlite::Connection) -> Result<(), String> {
    // Dropping the sidecar first keeps the foreign-key direction irrelevant;
    // the legacy DDL never declared one.
    connection
        .execute_batch(
            "DROP TABLE IF EXISTS worktree_lease_scopes;
             DROP TABLE IF EXISTS worktree_leases;",
        )
        .map_err(|error| format!("could not retire legacy worktree lease tables: {error}"))
}

fn validate_shape(connection: &rusqlite::Connection) -> Result<(), String> {
    for (table, columns) in [
        ("worktree_directories", DIRECTORY_COLUMNS),
        ("worktree_directory_tasks", TASK_COLUMNS),
        ("worktree_directory_legacy_issues", LEGACY_COLUMNS),
        ("worktree_directory_sessions", SESSION_COLUMNS),
    ] {
        if !table_exists(connection, table)? {
            return Err(format!(
                "worktree registry is missing its '{table}' table; run a write-capable worktree command to repair it"
            ));
        }
        for column in columns {
            if !column_exists(connection, table, column)? {
                return Err(format!(
                    "worktree registry table '{table}' is missing column '{column}'; run a write-capable worktree command to repair it"
                ));
            }
        }
    }
    Ok(())
}

fn meta_value(connection: &rusqlite::Connection, name: &str) -> Result<Option<String>, String> {
    use rusqlite::OptionalExtension;
    connection
        .query_row(
            "SELECT value FROM worktree_registry_meta WHERE name = ?1",
            [name],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("could not read worktree registry metadata: {error}"))
}

fn set_meta(connection: &rusqlite::Connection, name: &str, value: &str) -> Result<(), String> {
    connection
        .execute(
            "INSERT INTO worktree_registry_meta (name, value) VALUES (?1, ?2) \
             ON CONFLICT(name) DO UPDATE SET value = excluded.value",
            rusqlite::params![name, value],
        )
        .map_err(|error| format!("could not record worktree registry metadata: {error}"))
        .map(|_| ())
}

pub(super) fn table_exists(connection: &rusqlite::Connection, table: &str) -> Result<bool, String> {
    connection
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [table],
            |row| row.get::<_, i64>(0),
        )
        .map(|found| found == 1)
        .map_err(|error| format!("could not inspect phasegent schema: {error}"))
}

pub(super) fn column_exists(
    connection: &rusqlite::Connection,
    table: &str,
    column: &str,
) -> Result<bool, String> {
    if !table_exists(connection, table)? {
        return Ok(false);
    }
    connection
        .query_row(
            &format!("SELECT EXISTS (SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1)"),
            [column],
            |row| row.get::<_, i64>(0),
        )
        .map(|found| found == 1)
        .map_err(|error| format!("could not inspect phasegent schema: {error}"))
}

fn row_error(error: rusqlite::Error) -> String {
    format!("could not decode legacy worktree row: {error}")
}
