//! Durable canonical task identity storage (issue 1201 P5).
//!
//! A task is identified by the immutable four-tuple
//! `(provider, instance, project, external_id)` and addressed by an opaque
//! local `task_id`. This module only owns storage: it adds no runtime
//! resolver and no command calls it yet. P6 wires the reader/writer into
//! creation, binding and numbered lookups.

use rusqlite::{OptionalExtension, TransactionBehavior};

use crate::infra::storage::Storage;
use crate::providers::ProviderKind;

const MAX_FIELD_CHARS: usize = 512;

/// An immutable canonical task identity.
///
/// Construct through [`TaskKey::new`], which trims inputs, lowercases the
/// provider, normalizes the instance endpoint, and validates the canonical
/// positive decimal external id. The fields stay private so a malformed key
/// can never reach SQL directly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TaskKey {
    provider: String,
    instance: String,
    project: String,
    external_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TaskBinding {
    task_id: i64,
    key: TaskKey,
    created_at: i64,
}

impl TaskKey {
    pub(crate) fn new(
        provider: &str,
        instance: &str,
        project: &str,
        external_id: &str,
    ) -> Result<Self, String> {
        let provider_raw = validate_field(provider, "provider")?.to_ascii_lowercase();
        let kind = provider_raw
            .parse::<ProviderKind>()
            .map_err(|error| format!("invalid task provider: {error}"))?;
        let project = validate_field(project, "project")?;
        let external_id = validate_external_id(external_id)?;
        if kind == ProviderKind::Local {
            if validate_field(instance, "instance")? != "local" {
                return Err("Local task instance must be 'local'".to_owned());
            }
            let instance = canonical_instance(kind, None, &project)?;
            return Ok(Self {
                provider: provider_raw,
                instance,
                project,
                external_id,
            });
        }
        let instance = canonical_instance(kind, Some(instance), &project)?;
        Ok(Self {
            provider: provider_raw,
            instance,
            project,
            external_id,
        })
    }

    pub(crate) fn external_number(&self) -> Option<u64> {
        self.external_id.parse().ok()
    }

    pub(crate) fn provider(&self) -> &str {
        &self.provider
    }

    pub(crate) fn instance(&self) -> &str {
        &self.instance
    }

    pub(crate) fn project(&self) -> &str {
        &self.project
    }

    pub(crate) fn external_id(&self) -> &str {
        &self.external_id
    }
}

impl TaskBinding {
    pub(crate) fn task_id(&self) -> i64 {
        self.task_id
    }

    pub(crate) fn key(&self) -> &TaskKey {
        &self.key
    }
}

/// Normalize a non-secret instance endpoint into its canonical identity.
///
/// Local is the credential-free sentinel `local`; Redmine and Forgejo keep
/// their validated http(s) base, including any significant port and subpath,
/// with a trailing slash removed. Credentials, query strings and fragments
/// are never accepted or stored.
pub(crate) fn canonical_instance(
    provider: ProviderKind,
    endpoint: Option<&str>,
    project: &str,
) -> Result<String, String> {
    match provider {
        ProviderKind::Local => {
            if project != "default" {
                return Err("Local task project must be 'default'".to_owned());
            }
            Ok("local".to_owned())
        }
        ProviderKind::Forgejo => {
            let endpoint = endpoint
                .ok_or_else(|| "Forgejo task identity requires an instance endpoint".to_owned())?;
            let config = crate::providers::forgejo::ForgejoConfig::new(endpoint, project).map_err(
                |error| {
                    format!(
                        "invalid Forgejo task instance: {}",
                        error
                            .json()
                            .get("message")
                            .and_then(|value| value.as_str())
                            .unwrap_or("invalid endpoint or project")
                    )
                },
            )?;
            Ok(config.api_base().to_owned())
        }
        ProviderKind::Redmine => {
            let endpoint = endpoint
                .ok_or_else(|| "Redmine task identity requires an instance endpoint".to_owned())?;
            let url = url::Url::parse(endpoint)
                .map_err(|error| format!("invalid Redmine task instance: {error}"))?;
            if !url.username().is_empty() || url.password().is_some() {
                return Err("Redmine task instance must not include userinfo".to_owned());
            }
            crate::remote::normalize_redmine_api_base(endpoint)
                .map_err(|error| format!("invalid Redmine task instance: {error}"))
        }
    }
}

fn validate_field(value: &str, field: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!("task {field} must be non-empty"));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(format!("task {field} must not contain control characters"));
    }
    if trimmed.chars().count() > MAX_FIELD_CHARS {
        return Err(format!("task {field} is too long"));
    }
    Ok(trimmed.to_owned())
}

fn validate_external_id(value: &str) -> Result<String, String> {
    let trimmed = validate_field(value, "external id")?;
    if trimmed.len() > 1 && trimmed.starts_with('0') {
        return Err("task external id must not have leading zeros".to_owned());
    }
    let number = trimmed
        .parse::<u64>()
        .map_err(|_| "task external id must be a positive decimal number".to_owned())?;
    if number == 0 || number > i64::MAX as u64 {
        return Err("task external id must be a positive decimal number".to_owned());
    }
    Ok(trimmed)
}

fn validate_now(now: i64) -> Result<(), String> {
    if now <= 0 {
        return Err("task timestamp must be greater than zero".to_owned());
    }
    Ok(())
}

fn validate_repo_key(repo_key: &str) -> Result<&str, String> {
    if repo_key.trim().is_empty() {
        return Err("task repo key must be non-empty".to_owned());
    }
    if repo_key.chars().any(char::is_control) {
        return Err("task repo key must not contain control characters".to_owned());
    }
    if repo_key.chars().count() > MAX_FIELD_CHARS {
        return Err("task repo key is too long".to_owned());
    }
    Ok(repo_key)
}

#[cfg(test)]
impl TaskBinding {
    pub(crate) fn for_test(key: TaskKey, task_id: i64, created_at: i64) -> Self {
        Self {
            task_id,
            key,
            created_at,
        }
    }
}

fn select_binding(
    connection: &rusqlite::Connection,
    key: &TaskKey,
) -> Result<Option<TaskBinding>, String> {
    let row: Option<(i64, String, String, String, String, i64)> = connection
        .query_row(
            "SELECT task_id, provider, instance, project, external_id, created_at \
               FROM task_bindings \
              WHERE provider = ?1 AND instance = ?2 AND project = ?3 AND external_id = ?4",
            rusqlite::params![key.provider, key.instance, key.project, key.external_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("could not read task binding: {error}"))?;
    row.map(decode_binding).transpose()
}

fn decode_binding(
    (task_id, provider, instance, project, external_id, created_at): (
        i64,
        String,
        String,
        String,
        String,
        i64,
    ),
) -> Result<TaskBinding, String> {
    if task_id <= 0 {
        return Err("stored task binding has a non-positive task id".to_owned());
    }
    if created_at <= 0 {
        return Err("stored task binding has an invalid timestamp".to_owned());
    }
    let key = TaskKey::new(&provider, &instance, &project, &external_id)
        .map_err(|error| format!("stored task binding is corrupt: {error}"))?;
    if key.provider != provider
        || key.instance != instance
        || key.project != project
        || key.external_id != external_id
    {
        return Err("stored task binding is not canonical".to_owned());
    }
    Ok(TaskBinding {
        task_id,
        key,
        created_at,
    })
}

/// Return the binding for `key`, creating the canonical row when absent.
///
/// One immediate transaction serializes concurrent inserters: the loser's
/// `ON CONFLICT DO NOTHING` is a no-op and it reads the winner's `task_id`,
/// so separate connections converge on one row without a lock error under
/// the established busy-timeout policy.
pub(crate) fn ensure_binding(
    storage: &Storage,
    key: &TaskKey,
    now: i64,
) -> Result<TaskBinding, String> {
    validate_now(now)?;
    let transaction =
        rusqlite::Transaction::new_unchecked(&storage.connection, TransactionBehavior::Immediate)
            .map_err(|error| format!("could not begin task binding transaction: {error}"))?;
    let binding = ensure_binding_with(&transaction, key, now)?;
    transaction
        .commit()
        .map_err(|error| format!("could not commit task binding: {error}"))?;
    Ok(binding)
}

/// Transaction-compatible binding insert/read so an adopter can persist the
/// task, membership and branch reference atomically on one connection.
pub(crate) fn ensure_binding_with(
    connection: &rusqlite::Connection,
    key: &TaskKey,
    now: i64,
) -> Result<TaskBinding, String> {
    validate_now(now)?;
    connection
        .execute(
            "INSERT INTO task_bindings (provider, instance, project, external_id, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(provider, instance, project, external_id) DO NOTHING",
            rusqlite::params![
                key.provider,
                key.instance,
                key.project,
                key.external_id,
                now
            ],
        )
        .map_err(|error| format!("could not insert task binding: {error}"))?;
    select_binding(connection, key)?
        .ok_or_else(|| "task binding disappeared during insert".to_owned())
}

/// Add `(repo_key, task_id)` membership. A duplicate is a no-op, while a
/// missing referenced binding is rejected inside the same transaction (the
/// check does not rely on foreign-key enforcement being enabled).
pub(crate) fn add_repo_membership(
    storage: &Storage,
    repo_key: &str,
    task_id: i64,
    now: i64,
) -> Result<(), String> {
    let transaction =
        rusqlite::Transaction::new_unchecked(&storage.connection, TransactionBehavior::Immediate)
            .map_err(|error| format!("could not begin task membership transaction: {error}"))?;
    add_repo_membership_with(&transaction, repo_key, task_id, now)?;
    transaction
        .commit()
        .map_err(|error| format!("could not commit task membership: {error}"))?;
    Ok(())
}

/// Transaction-compatible membership insert with an explicit referenced
/// binding check, so the caller's adoption transaction stays all-or-nothing
/// even when SQLite foreign-key enforcement is absent.
pub(crate) fn add_repo_membership_with(
    connection: &rusqlite::Connection,
    repo_key: &str,
    task_id: i64,
    now: i64,
) -> Result<(), String> {
    let repo_key = validate_repo_key(repo_key)?;
    validate_now(now)?;
    if task_id <= 0 {
        return Err("task id must be greater than zero".to_owned());
    }
    let exists: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM task_bindings WHERE task_id = ?1",
            [task_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("could not verify task binding: {error}"))?;
    if exists != 1 {
        return Err(format!("task binding {task_id} does not exist"));
    }
    connection
        .execute(
            "INSERT INTO task_repo_memberships (repo_key, task_id, created_at) \
             VALUES (?1, ?2, ?3) \
             ON CONFLICT(repo_key, task_id) DO NOTHING",
            rusqlite::params![repo_key, task_id, now],
        )
        .map_err(|error| format!("could not insert task membership: {error}"))?;
    Ok(())
}

/// Look up a binding by its opaque local id, validating the persisted row.
pub(crate) fn binding_for_task_id(
    storage: &Storage,
    task_id: i64,
) -> Result<Option<TaskBinding>, String> {
    binding_for_task_id_on(&storage.connection, task_id)
}

/// Look up a binding by its opaque local id on an arbitrary connection, so a
/// transaction-bound reader can validate a persisted reference without
/// opening a second store.
pub(crate) fn binding_for_task_id_on(
    connection: &rusqlite::Connection,
    task_id: i64,
) -> Result<Option<TaskBinding>, String> {
    if task_id <= 0 {
        return Err("task id must be greater than zero".to_owned());
    }
    let row: Option<(i64, String, String, String, String, i64)> = connection
        .query_row(
            "SELECT task_id, provider, instance, project, external_id, created_at \
               FROM task_bindings WHERE task_id = ?1",
            [task_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("could not read task binding: {error}"))?;
    row.map(decode_binding).transpose()
}

/// Every explicit membership binding for one bare number in one physical
/// repository, ordered by `task_id`. Zero, one or many rows are returned so
/// P6 can decide ambiguity; no global number lookup happens here.
pub(crate) fn bindings_for_repo_number(
    storage: &Storage,
    repo_key: &str,
    number: u64,
) -> Result<Vec<TaskBinding>, String> {
    bindings_for_repo_number_with(&storage.connection, repo_key, number)
}

/// Transaction-compatible repo-number membership read used by adoption
/// conflict checks before any row is written.
pub(crate) fn bindings_for_repo_number_with(
    connection: &rusqlite::Connection,
    repo_key: &str,
    number: u64,
) -> Result<Vec<TaskBinding>, String> {
    let repo_key = validate_repo_key(repo_key)?;
    if number == 0 || number > i64::MAX as u64 {
        return Err("task repo number must be a positive decimal number".to_owned());
    }
    let mut statement = connection
        .prepare(
            "SELECT b.task_id, b.provider, b.instance, b.project, b.external_id, b.created_at \
               FROM task_repo_memberships m \
               JOIN task_bindings b ON b.task_id = m.task_id \
              WHERE m.repo_key = ?1 AND b.external_id = ?2 \
              ORDER BY b.task_id",
        )
        .map_err(|error| format!("could not prepare task repo lookup: {error}"))?;
    let rows = statement
        .query_map(rusqlite::params![repo_key, number.to_string()], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .map_err(|error| format!("could not read task repo lookup: {error}"))?;
    let mut bindings = Vec::new();
    for row in rows {
        let raw = row.map_err(|error| format!("could not read task repo row: {error}"))?;
        bindings.push(decode_binding(raw)?);
    }
    Ok(bindings)
}

/// Every durable task for one bare number, across every repository.
///
/// A number-only command (a timer names only an issue number) has no
/// repository to scope the lookup, so this returns the complete set: zero,
/// one or many tasks. Several tasks for one number are an ambiguity the
/// caller must refuse rather than guess.
pub(crate) fn bindings_for_number(
    connection: &rusqlite::Connection,
    number: u64,
) -> Result<Vec<TaskBinding>, String> {
    if number == 0 || number > i64::MAX as u64 {
        return Err("task number must be a positive decimal number".to_owned());
    }
    let mut statement = connection
        .prepare(
            "SELECT b.task_id, b.provider, b.instance, b.project, b.external_id, b.created_at \
               FROM task_bindings b \
              WHERE b.external_id = ?1 \
              ORDER BY b.task_id",
        )
        .map_err(|error| format!("could not prepare task number lookup: {error}"))?;
    let rows = statement
        .query_map(rusqlite::params![number.to_string()], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .map_err(|error| format!("could not read task number lookup: {error}"))?;
    let mut bindings = Vec::new();
    for row in rows {
        let raw = row.map_err(|error| format!("could not read task number row: {error}"))?;
        bindings.push(decode_binding(raw)?);
    }
    Ok(bindings)
}

/// Whether the task is an explicit member of the physical repository.
///
/// The lookup runs on the caller's own connection, so a membership proof
/// inside an immediate transaction reads exactly the state that transaction
/// holds. A missing membership table or column is absent evidence, not a
/// membership.
pub(crate) fn task_repo_membership_exists_on(
    connection: &rusqlite::Connection,
    repo_key: &str,
    task_id: i64,
) -> Result<bool, String> {
    validate_repo_key(repo_key)?;
    if task_id <= 0 {
        return Err("task id must be greater than zero".to_owned());
    }
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM task_repo_memberships m \
               JOIN task_bindings b ON b.task_id = m.task_id \
              WHERE m.repo_key = ?1 AND m.task_id = ?2",
            rusqlite::params![repo_key, task_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("could not read task repo membership: {error}"))?;
    Ok(count == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    struct TempDb(PathBuf);

    impl TempDb {
        fn new(label: &str) -> Self {
            let dir = crate::test_scratch::root().join(format!(
                "phasegent-task-identity-{label}-{}-{}",
                std::process::id(),
                crate::worktree::now_unix_secs()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir.join("task_identity.sqlite3"))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
            for suffix in ["-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
            }
            if let Some(parent) = self.0.parent() {
                let _ = std::fs::remove_dir(parent);
            }
        }
    }

    fn storage(db: &TempDb) -> Storage {
        Storage::open_at(db.path()).unwrap()
    }

    fn forgejo_key(number: &str) -> TaskKey {
        TaskKey::new("forgejo", "https://forgejo.test/", "owner/repo", number).unwrap()
    }

    fn binding_count(storage: &Storage) -> i64 {
        storage
            .connection
            .query_row("SELECT COUNT(*) FROM task_bindings", [], |row| row.get(0))
            .unwrap()
    }

    fn membership_count(storage: &Storage) -> i64 {
        storage
            .connection
            .query_row("SELECT COUNT(*) FROM task_repo_memberships", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    #[test]
    fn canonical_key_dedupes_and_two_instances_stay_distinct() {
        let db = TempDb::new("dedupe");
        let storage = storage(&db);
        let key = forgejo_key("7");
        let first = ensure_binding(&storage, &key, 10).unwrap();
        let second = ensure_binding(&storage, &forgejo_key("7"), 11).unwrap();
        assert_eq!(first.task_id(), second.task_id());
        assert_eq!(binding_count(&storage), 1);

        let other_instance = TaskKey::new(
            "forgejo",
            "https://forgejo.test:8443/root",
            "owner/repo",
            "7",
        )
        .unwrap();
        let third = ensure_binding(&storage, &other_instance, 12).unwrap();
        assert_ne!(first.task_id(), third.task_id());
        assert_eq!(binding_count(&storage), 2);
    }

    #[test]
    fn concurrent_connections_return_one_task_id() {
        let db = TempDb::new("concurrent");
        let first_storage = storage(&db);
        let second_storage = Storage::open_at(db.path()).unwrap();
        let key = forgejo_key("9");
        let expected = ensure_binding(&first_storage, &key, 10).unwrap().task_id();
        let handle = std::thread::spawn(move || {
            let binding = ensure_binding(&second_storage, &key, 11).unwrap();
            binding.task_id()
        });
        let observed = handle.join().unwrap();
        assert_eq!(observed, expected);
        assert_eq!(binding_count(&first_storage), 1);
    }

    #[test]
    fn instance_normalization_boundaries() {
        let trailing = TaskKey::new("forgejo", "https://forgejo.test/", "owner/repo", "7").unwrap();
        let plain = TaskKey::new("forgejo", "https://forgejo.test", "owner/repo", "7").unwrap();
        assert_eq!(trailing, plain);
        assert_eq!(trailing.external_number(), Some(7));

        let ported =
            TaskKey::new("forgejo", "https://forgejo.test:8443", "owner/repo", "7").unwrap();
        let subpath =
            TaskKey::new("forgejo", "https://forgejo.test/prefix", "owner/repo", "7").unwrap();
        assert_ne!(trailing, ported);
        assert_ne!(trailing, subpath);

        let redmine_a =
            TaskKey::new("redmine", "https://redmine.test/", "project-a", "12").unwrap();
        let redmine_b = TaskKey::new("redmine", "https://redmine.test", "project-a", "12").unwrap();
        assert_eq!(redmine_a, redmine_b);
        assert_eq!(redmine_a.instance(), "https://redmine.test");

        let local = TaskKey::new("local", "local", "default", "3").unwrap();
        assert_eq!(local.instance(), "local");
        assert_eq!(local.provider(), "local");
    }

    #[test]
    fn malformed_keys_are_rejected_without_partial_writes() {
        let db = TempDb::new("reject");
        let storage = storage(&db);
        let cases = [
            TaskKey::new("gitlab", "https://forgejo.test", "owner/repo", "7").err(),
            TaskKey::new("forgejo", "ftp://forgejo.test", "owner/repo", "7").err(),
            TaskKey::new(
                "forgejo",
                "https://user:pass@forgejo.test",
                "owner/repo",
                "7",
            )
            .err(),
            TaskKey::new("forgejo", "https://forgejo.test?x=1", "owner/repo", "7").err(),
            TaskKey::new("forgejo", "https://forgejo.test#frag", "owner/repo", "7").err(),
            TaskKey::new("forgejo", "https://forgejo.test", "not-a-repo", "7").err(),
            TaskKey::new("forgejo", "https://forgejo.test", "owner/repo", "0").err(),
            TaskKey::new("forgejo", "https://forgejo.test", "owner/repo", "007").err(),
            TaskKey::new("forgejo", "https://forgejo.test", "owner/repo", "-3").err(),
            TaskKey::new("local", "elsewhere", "default", "1").err(),
            TaskKey::new("local", "local", "other", "1").err(),
        ];
        for case in cases {
            assert!(case.is_some(), "malformed task key must be rejected");
        }
        assert_eq!(binding_count(&storage), 0);

        let key = forgejo_key("7");
        let binding = ensure_binding(&storage, &key, 10).unwrap();
        add_repo_membership(&storage, "repo-a", binding.task_id(), 10).unwrap();
        storage
            .connection
            .execute(
                "UPDATE task_bindings SET instance = 'https://forgejo.test/' WHERE task_id = ?1",
                [binding.task_id()],
            )
            .unwrap();
        assert!(
            bindings_for_repo_number(&storage, "repo-a", 7).is_err(),
            "a non-canonical persisted instance must be rejected"
        );
    }

    #[test]
    fn task_binding_memberships_are_conflict_safe_and_repo_scoped() {
        let db = TempDb::new("memberships");
        let storage = storage(&db);
        let key = forgejo_key("7");
        let binding = ensure_binding(&storage, &key, 10).unwrap();
        let missing = add_repo_membership(&storage, "repo-a", 999, 10);
        assert!(missing.is_err());
        assert_eq!(membership_count(&storage), 0);

        add_repo_membership(&storage, "repo-a", binding.task_id(), 10).unwrap();
        add_repo_membership(&storage, "repo-a", binding.task_id(), 11).unwrap();
        add_repo_membership(&storage, "repo-b", binding.task_id(), 12).unwrap();
        assert_eq!(membership_count(&storage), 2);

        let same_repo = bindings_for_repo_number(&storage, "repo-a", 7).unwrap();
        assert_eq!(same_repo.len(), 1);
        assert_eq!(same_repo[0].task_id(), binding.task_id());
        assert!(
            bindings_for_repo_number(&storage, "repo-c", 7)
                .unwrap()
                .is_empty()
        );
        assert!(
            bindings_for_repo_number(&storage, "repo-a", 8)
                .unwrap()
                .is_empty()
        );

        let second_key = TaskKey::new("forgejo", "https://other.test", "owner/repo", "7").unwrap();
        let second = ensure_binding(&storage, &second_key, 13).unwrap();
        add_repo_membership(&storage, "repo-a", second.task_id(), 13).unwrap();
        let ambiguous = bindings_for_repo_number(&storage, "repo-a", 7).unwrap();
        assert_eq!(ambiguous.len(), 2);
        assert!(ambiguous[0].task_id() < ambiguous[1].task_id());
    }

    #[test]
    fn legacy_link_and_scope_rows_gain_nullable_task_ids_without_backfill() {
        let db = TempDb::new("legacy");
        let connection = rusqlite::Connection::open(db.path()).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE branch_issue_links (\
                     repo_key TEXT NOT NULL, branch TEXT NOT NULL, provider TEXT NOT NULL, \
                     project TEXT NOT NULL, external_id TEXT NOT NULL, \
                     issue_number INTEGER NOT NULL, status TEXT NOT NULL, source TEXT NOT NULL, \
                     created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, \
                     detached_at INTEGER, detached_reason TEXT, \
                     PRIMARY KEY (repo_key, branch, provider, project, external_id));\
                 CREATE TABLE worktree_lease_scopes (\
                     lease_id TEXT PRIMARY KEY, provider TEXT NOT NULL, project TEXT NOT NULL, \
                     external_id INTEGER NOT NULL, created_at INTEGER NOT NULL);\
                 CREATE TABLE worktree_leases (\
                     lease_id TEXT PRIMARY KEY, repo_identity TEXT NOT NULL, issue INTEGER NOT NULL, \
                     session TEXT NOT NULL, checkout_path TEXT NOT NULL, worktree_path TEXT NOT NULL, \
                     branch TEXT NOT NULL, created_at INTEGER NOT NULL);\
                 INSERT INTO branch_issue_links VALUES \
                     ('repo', 'branch', 'forgejo', 'owner/repo', '7', 7, 'linked', 'fixture', 1, 1, NULL, NULL);\
                 INSERT INTO worktree_lease_scopes VALUES \
                     ('lease', 'forgejo', 'owner/repo', 7, 1);\
                 INSERT INTO worktree_leases VALUES \
                     ('lease', '/repo/.git', 7, 'ses_legacy', '/repo', '/repo/wt-7', 'feat/7', 1);",
            )
            .unwrap();
        drop(connection);

        let storage = Storage::open_at(db.path()).unwrap();
        crate::branch_links::store::ensure_schema(&storage.connection).unwrap();
        crate::branch_links::store::ensure_schema(&storage.connection).unwrap();
        crate::worktree::ensure_schema(&storage).unwrap();
        crate::worktree::ensure_schema(&storage).unwrap();

        let link: (String, String, Option<i64>) = storage
            .connection
            .query_row(
                "SELECT provider, project, task_id FROM branch_issue_links \
                  WHERE repo_key = 'repo' AND branch = 'branch'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(link, ("forgejo".to_owned(), "owner/repo".to_owned(), None));
        let records = crate::worktree::registry::load_records(&storage, None).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].legacy_issues.len(), 1);
        assert_eq!(records[0].legacy_issues[0].provider, "forgejo");
        assert_eq!(records[0].legacy_issues[0].project, "owner/repo");
        assert_eq!(records[0].legacy_issues[0].external_id, 7);
        assert!(records[0].tasks.is_empty(), "no task id is backfilled");
        let scope_table: bool = storage
            .connection
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'worktree_lease_scopes')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!scope_table, "the upgrade retires the legacy scope table");
        assert_eq!(binding_count(&storage), 0);
        assert_eq!(membership_count(&storage), 0);
    }
}
