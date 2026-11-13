use super::support::*;
use super::*;

use crate::worktree::registry;

fn legacy_store(storage: &Storage) -> &Storage {
    storage
        .connection
        .execute_batch(
            "CREATE TABLE worktree_leases (
                lease_id TEXT PRIMARY KEY,
                repo_identity TEXT NOT NULL,
                issue INTEGER NOT NULL,
                session TEXT NOT NULL,
                checkout_path TEXT NOT NULL,
                worktree_path TEXT NOT NULL,
                branch TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                heartbeat_at INTEGER NOT NULL
            );
            CREATE TABLE worktree_lease_scopes (
                lease_id TEXT PRIMARY KEY,
                provider TEXT NOT NULL,
                project TEXT NOT NULL,
                external_id INTEGER NOT NULL,
                task_id INTEGER
            );",
        )
        .expect("legacy schema");
    storage
}

fn lease(storage: &Storage, id: &str, issue: u64, path: &str, session: &str, created: i64) {
    storage
        .connection
        .execute(
            "INSERT INTO worktree_leases \
                (lease_id, repo_identity, issue, session, checkout_path, worktree_path, branch, status, created_at, heartbeat_at) \
             VALUES (?1, '/tmp/repo/.git', ?2, ?3, '/tmp/repo', ?4, 'phasegent/test', 'retained', ?5, ?5)",
            rusqlite::params![id, issue as i64, session, path, created],
        )
        .expect("legacy lease");
}

fn scope(storage: &Storage, id: &str, provider: &str, project: &str, external: u64) {
    storage
        .connection
        .execute(
            "INSERT INTO worktree_lease_scopes (lease_id, provider, project, external_id, task_id) \
             VALUES (?1, ?2, ?3, ?4, NULL)",
            rusqlite::params![id, provider, project, external as i64],
        )
        .expect("legacy scope");
}

#[test]
fn upgrade_imports_every_legacy_row_and_drops_the_lease_tables_atomically() {
    let (_temp, storage, _env) = open_temp_db("registry-upgrade");
    legacy_store(&storage);
    lease(&storage, "one", 7, "/tmp/repo/wt", "ses_a", 2);
    lease(&storage, "two", 8, "/tmp/repo/wt", "ses_b", 3);
    scope(&storage, "one", "forgejo", "owner/repo", 7);

    registry::ensure_schema(&storage).expect("upgrade");
    let records = registry::load_records(&storage, Some("/tmp/repo/.git")).expect("records");
    assert_eq!(records.len(), 1, "rows sharing one path form one directory");
    let record = &records[0];
    assert_eq!(record.sessions, vec!["ses_a", "ses_b"]);
    assert_eq!(record.issue_numbers(), vec![7, 8]);
    assert_eq!(record.legacy_issues.len(), 2);
    let scoped = record
        .legacy_issues
        .iter()
        .find(|issue| issue.issue == 7)
        .expect("the scoped association keeps its provenance");
    assert_eq!(scoped.provider, "forgejo");
    assert_eq!(scoped.project, "owner/repo");
    assert_eq!(scoped.external_id, 7);
    let unknown = record
        .legacy_issues
        .iter()
        .find(|issue| issue.issue == 8)
        .expect("the unscoped association stays unknown");
    assert_eq!(unknown.provider, "");
    assert_eq!(unknown.project, "");
    assert_eq!(unknown.external_id, 0);

    for table in ["worktree_leases", "worktree_lease_scopes"] {
        let present: bool = storage
            .connection
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!present, "{table} must be dropped in the same upgrade");
    }
}

#[test]
fn upgrade_refuses_an_unrepresentable_legacy_issue_without_dropping_evidence() {
    let (_temp, storage, _env) = open_temp_db("registry-invalid-issue");
    legacy_store(&storage);
    lease(&storage, "invalid", 0, "/tmp/repo/wt", "ses_a", 2);

    assert!(registry::ensure_schema(&storage).is_err());
    let count: i64 = storage
        .connection
        .query_row(
            "SELECT COUNT(*) FROM worktree_leases WHERE lease_id = 'invalid' AND issue = 0",
            [],
            |row| row.get(0),
        )
        .expect("legacy evidence remains");
    assert_eq!(count, 1);
}

#[test]
fn upgrade_is_idempotent_and_never_duplicates_directory_evidence() {
    let (_temp, storage, _env) = open_temp_db("registry-idempotent");
    legacy_store(&storage);
    lease(&storage, "one", 7, "/tmp/repo/wt", "ses_a", 2);
    scope(&storage, "one", "forgejo", "owner/repo", 7);

    registry::ensure_schema(&storage).expect("first upgrade");
    let before = registry::load_records(&storage, None).expect("records");
    registry::ensure_schema(&storage).expect("second upgrade");
    let after = registry::load_records(&storage, None).expect("records");
    assert_eq!(before, after);
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].legacy_issues.len(), 1);
}

#[test]
fn legacy_projection_is_read_only_and_leaves_the_legacy_store_untouched() {
    let (_temp, storage, _env) = open_temp_db("registry-preview");
    legacy_store(&storage);
    lease(&storage, "one", 7, "/tmp/repo/wt", "ses_a", 2);
    scope(&storage, "one", "redmine", "23", 7);

    let records = registry::load_records(&storage, Some("/tmp/repo/.git")).expect("preview");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].sessions, vec!["ses_a"]);
    let legacy = &records[0].legacy_issues[0];
    assert_eq!(
        (legacy.provider.as_str(), legacy.project.as_str()),
        ("redmine", "23")
    );

    let registry_tables: i64 = storage
        .connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'worktree_directories'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(registry_tables, 0, "a preview never creates the registry");
    let leases: i64 = storage
        .connection
        .query_row("SELECT COUNT(*) FROM worktree_leases", [], |row| row.get(0))
        .unwrap();
    assert_eq!(leases, 1, "a preview never drops legacy evidence");
}

#[test]
fn an_absent_store_reads_as_an_empty_registry() {
    let _lock = crate::infra::storage::test_support::lock_workflow_tests();
    let temp = TempDir::new("registry-absent");
    let missing = temp.path().join("missing.sqlite3");
    let _env = EnvGuard::set("PHASEGENT_DB_PATH", missing.to_string_lossy().as_ref());
    assert!(
        Storage::open_existing_read_only()
            .expect("read-only open")
            .is_none(),
        "an absent store must report absence instead of creating one"
    );
    assert!(!missing.exists());
}

#[test]
fn directories_keep_multiple_tasks_and_sessions_after_an_issue_association() {
    let (_temp, storage, _env) = open_temp_db("registry-multi");
    registry::ensure_schema(&storage).expect("schema");
    let path = TempDir::new("registry-multi-dir");
    let canonical = crate::worktree::canonical_or_self(path.path());
    let id = registry::stable_directory_id("/tmp/repo/.git", &canonical.to_string_lossy());
    registry::insert_directory(
        &storage,
        &registry::NewDirectory {
            directory_id: &id,
            repo_identity: "/tmp/repo/.git",
            checkout_path: "/tmp/repo",
            worktree_path: &canonical.to_string_lossy(),
            branch: "phasegent/test",
            created_at: 1,
            session: "ses_a",
            associations: &[
                registry::NewAssociation::Unscoped { issue: 7 },
                registry::NewAssociation::Unscoped { issue: 8 },
            ],
        },
    )
    .expect("insert");
    registry::attach_association(
        &storage,
        &id,
        "/tmp/repo/.git",
        2,
        "ses_b",
        &registry::NewAssociation::Unscoped { issue: 9 },
    )
    .expect("attach");

    let records = registry::load_records(&storage, Some("/tmp/repo/.git")).expect("records");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].issue_numbers(), vec![7, 8, 9]);
    assert_eq!(records[0].sessions, vec!["ses_a", "ses_b"]);
}

#[test]
fn removing_a_directory_deletes_its_associations_and_keeps_others() {
    let (_temp, storage, _env) = open_temp_db("registry-remove");
    registry::ensure_schema(&storage).expect("schema");
    let first = TempDir::new("registry-remove-a");
    let second = TempDir::new("registry-remove-b");
    for (path, id, issue) in [(first.path(), "dir-a", 7), (second.path(), "dir-b", 8)] {
        let canonical = crate::worktree::canonical_or_self(path);
        registry::insert_directory(
            &storage,
            &registry::NewDirectory {
                directory_id: id,
                repo_identity: "/tmp/repo/.git",
                checkout_path: "/tmp/repo",
                worktree_path: &canonical.to_string_lossy(),
                branch: "phasegent/test",
                created_at: 1,
                session: "ses_a",
                associations: &[registry::NewAssociation::Unscoped { issue }],
            },
        )
        .expect("insert");
    }
    let key =
        crate::task_identity::TaskKey::new("forgejo", "https://forgejo.test", "owner/repo", "7")
            .unwrap();
    let binding = crate::task_identity::ensure_binding(&storage, &key, 10).unwrap();
    storage.connection.execute("INSERT INTO worktree_directory_tasks (directory_id, task_id, issue, created_at) VALUES ('dir-a', ?1, 7, 1)", [binding.task_id()]).unwrap();
    storage
        .connection
        .execute_batch("PRAGMA foreign_keys = OFF")
        .unwrap();
    assert_eq!(
        registry::remove_directory(&storage, "/tmp/repo/.git", "dir-a").unwrap(),
        1
    );
    let records = registry::load_records(&storage, Some("/tmp/repo/.git")).expect("records");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].directory_id, "dir-b");
    let orphans: i64 = storage
        .connection
        .query_row(
            "SELECT COUNT(*) FROM worktree_directory_legacy_issues WHERE directory_id = 'dir-a'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        orphans, 0,
        "explicit cleanup removes association rows without FK enforcement"
    );
    let sessions: i64 = storage
        .connection
        .query_row(
            "SELECT COUNT(*) FROM worktree_directory_sessions WHERE directory_id = 'dir-a'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sessions, 0);
    let tasks: i64 = storage
        .connection
        .query_row(
            "SELECT COUNT(*) FROM worktree_directory_tasks WHERE directory_id = 'dir-a'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tasks, 0);
    assert!(
        crate::task_identity::binding_for_task_id_on(&storage.connection, binding.task_id())
            .unwrap()
            .is_some()
    );
}

#[test]
fn a_marker_bearing_legacy_store_imports_its_rows_before_dropping_them() {
    let (_temp, storage, _env) = open_temp_db("registry-marker-legacy");
    legacy_store(&storage);
    lease(&storage, "one", 7, "/tmp/repo/wt", "ses_a", 2);
    scope(&storage, "one", "forgejo", "owner/repo", 7);
    storage
        .connection
        .execute_batch(
            "CREATE TABLE worktree_registry_meta (name TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO worktree_registry_meta (name, value) VALUES ('schema_version', '1');",
        )
        .expect("marker");

    let preview = registry::load_records(&storage, Some("/tmp/repo/.git")).expect("preview");
    assert_eq!(
        preview[0].issue_numbers(),
        vec![7],
        "a marker must never hide unconsumed legacy rows from a read"
    );
    registry::ensure_schema(&storage).expect("converging upgrade");
    let records = registry::load_records(&storage, Some("/tmp/repo/.git")).expect("records");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].issue_numbers(), vec![7]);
    assert_eq!(records[0].sessions, vec!["ses_a"]);
    assert!(
        {
            let exists: i64 = storage
                .connection
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'worktree_leases')",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            exists == 0
        },
        "the imported legacy table is retired"
    );
}

#[test]
fn an_unsupported_legacy_schema_refuses_the_upgrade_and_keeps_every_old_row() {
    let (_temp, storage, _env) = open_temp_db("registry-unsupported-legacy");
    storage
        .connection
        .execute_batch(
            "CREATE TABLE worktree_leases (
                lease_id TEXT PRIMARY KEY,
                repo_identity TEXT NOT NULL,
                issue INTEGER NOT NULL,
                session TEXT NOT NULL,
                worktree_path TEXT NOT NULL,
                branch TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            INSERT INTO worktree_leases
                (lease_id, repo_identity, issue, session, worktree_path, branch, created_at)
            VALUES ('one', '/tmp/repo/.git', 7, 'ses_a', '/tmp/repo/wt', 'phasegent/test', 2);",
        )
        .expect("older schema");

    let error = registry::ensure_schema(&storage).expect_err("unsupported schema refuses");
    assert!(error.contains("checkout_path"), "got: {error}");
    let rows: i64 = storage
        .connection
        .query_row("SELECT COUNT(*) FROM worktree_leases", [], |row| row.get(0))
        .expect("legacy rows survive");
    assert_eq!(rows, 1, "the old store stays intact for inspection");
}

#[test]
fn migration_preserves_every_same_number_scope_and_an_exact_task_identity() {
    let (_temp, storage, _env) = open_temp_db("registry-all-scope");
    legacy_store(&storage);
    lease(&storage, "forgejo", 7, "/tmp/repo/wt", "ses_a", 2);
    lease(&storage, "redmine", 7, "/tmp/repo/wt", "ses_b", 3);
    lease(&storage, "task", 7, "/tmp/repo/wt", "ses_c", 4);
    scope(&storage, "forgejo", "forgejo", "owner/repo", 7);
    scope(&storage, "redmine", "redmine", "23", 7);
    let key =
        crate::task_identity::TaskKey::new("forgejo", "https://forgejo.test", "owner/repo", "7")
            .expect("task key");
    let binding = crate::task_identity::ensure_binding(&storage, &key, 10).expect("binding");
    storage
        .connection
        .execute(
            "INSERT INTO worktree_lease_scopes (lease_id, provider, project, external_id, task_id) \
             VALUES ('task', 'forgejo', 'owner/repo', 7, ?1)",
            [binding.task_id()],
        )
        .expect("task scope");

    registry::ensure_schema(&storage).expect("upgrade");
    let records = registry::load_records(&storage, Some("/tmp/repo/.git")).expect("records");
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.tasks.len(), 1);
    assert_eq!(record.tasks[0].task_id, binding.task_id());
    assert_eq!(record.tasks[0].issue, 7);
    let scopes: Vec<(&str, &str, u64)> = record
        .legacy_issues
        .iter()
        .map(|issue| {
            (
                issue.provider.as_str(),
                issue.project.as_str(),
                issue.external_id,
            )
        })
        .collect();
    assert_eq!(
        scopes,
        vec![("forgejo", "owner/repo", 7), ("redmine", "23", 7)],
        "two same-number legacy scopes survive together"
    );
}

#[test]
fn a_durable_task_attach_is_idempotent_across_repeats() {
    use crate::worktree::registry::{NewAssociation, NewDirectory};

    let (_temp, storage, _env) = open_temp_db("registry-task-repeat");
    let Some(repo) = TempRepo::init("registry-task-repeat-repo") else {
        return;
    };
    repo.set_origin("https://forgejo.test/owner/repo.git");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let repo_key = crate::branch_links::repo_key_for_origin("https://forgejo.test/owner/repo.git")
        .expect("repo key");
    let key =
        crate::task_identity::TaskKey::new("forgejo", "https://forgejo.test", "owner/repo", "7")
            .expect("task key");
    let binding = crate::task_identity::ensure_binding(&storage, &key, 10).expect("binding");
    crate::task_identity::add_repo_membership(&storage, &repo_key, binding.task_id(), 10)
        .expect("membership");
    registry::ensure_schema(&storage).expect("schema");
    let canonical = crate::worktree::canonical_or_self(repo.dir.path());
    let id = registry::stable_directory_id(&identity, &canonical.to_string_lossy());
    let association = NewAssociation::Task {
        task_id: binding.task_id(),
        issue: 7,
        provider: "forgejo".to_owned(),
        project: "owner/repo".to_owned(),
        external_id: 7,
    };
    registry::insert_directory(
        &storage,
        &NewDirectory {
            directory_id: &id,
            repo_identity: &identity,
            checkout_path: &repo.dir.path().to_string_lossy(),
            worktree_path: &canonical.to_string_lossy(),
            branch: "phasegent/test",
            created_at: 10,
            session: "ses_a",
            associations: std::slice::from_ref(&association),
        },
    )
    .expect("first task association");
    registry::attach_association(&storage, &id, &identity, 11, "ses_a", &association)
        .expect("exact repeat is idempotent");
    let count: i64 = storage
        .connection
        .query_row(
            "SELECT COUNT(*) FROM worktree_directory_tasks WHERE directory_id = ?1 AND task_id = ?2",
            rusqlite::params![id, binding.task_id()],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(count, 1);
}

#[test]
fn same_number_and_same_session_never_make_a_foreign_identity_ours() {
    use crate::worktree::occupancy;
    use crate::worktree::registry::{DirectoryTask, NewAssociation};

    let record = crate::worktree::DirectoryRecord {
        directory_id: "dir-1".to_owned(),
        repo_identity: "/tmp/repo/.git".to_owned(),
        checkout_path: "/tmp/repo".to_owned(),
        worktree_path: "/tmp/repo/wt".to_owned(),
        branch: "phasegent/test".to_owned(),
        created_at: 1,
        tasks: vec![DirectoryTask {
            task_id: 5,
            issue: 7,
            provider: Some("forgejo".to_owned()),
            instance: Some("https://forgejo.test".to_owned()),
            project: Some("owner/repo".to_owned()),
            external_id: Some("7".to_owned()),
        }],
        legacy_issues: vec![crate::worktree::DirectoryLegacyIssue {
            issue: 7,
            provider: "redmine".to_owned(),
            project: "23".to_owned(),
            external_id: 7,
        }],
        sessions: vec!["ses_shared".to_owned()],
    };
    let other_task = NewAssociation::Task {
        task_id: 6,
        issue: 7,
        provider: "forgejo".to_owned(),
        project: "owner/repo".to_owned(),
        external_id: 7,
    };
    assert!(
        !occupancy::owns_association(&other_task, &record),
        "same number is not the same task"
    );
    let other_legacy = NewAssociation::Legacy {
        issue: 7,
        provider: "forgejo".to_owned(),
        project: "owner/repo".to_owned(),
        external_id: 7,
    };
    assert!(
        !occupancy::owns_association(&other_legacy, &record),
        "same number is not the same legacy scope"
    );
    let exact = NewAssociation::Task {
        task_id: 5,
        issue: 7,
        provider: "forgejo".to_owned(),
        project: "owner/repo".to_owned(),
        external_id: 7,
    };
    assert!(occupancy::owns_association(&exact, &record));
    assert!(
        occupancy::owns_association(
            &NewAssociation::Legacy {
                issue: 7,
                provider: "redmine".to_owned(),
                project: "23".to_owned(),
                external_id: 7,
            },
            &record,
        ),
        "the exact legacy provider/project/external identity is ours"
    );
}

#[test]
fn a_fresh_foreign_host_occupant_keeps_a_closed_historical_directory() {
    use crate::worktree::occupancy::{self, ForeignIssueState};
    use crate::worktree::opencode::test_support::FakeOpenCodeApi;
    use crate::worktree::registry::NewAssociation;

    struct EverythingClosed;
    impl ForeignIssueState for EverythingClosed {
        fn task_closed(&self, _repo_path: &Path, _task: &crate::worktree::DirectoryTask) -> bool {
            true
        }
        fn legacy_closed(
            &self,
            _repo_path: &Path,
            _issue: &crate::worktree::DirectoryLegacyIssue,
        ) -> bool {
            true
        }
    }

    let record = crate::worktree::DirectoryRecord {
        directory_id: "dir-host".to_owned(),
        repo_identity: "/tmp/repo/.git".to_owned(),
        checkout_path: "/tmp/repo".to_owned(),
        worktree_path: "/tmp/pg-host-checkout".to_owned(),
        branch: "phasegent/test".to_owned(),
        created_at: 1,
        tasks: Vec::new(),
        legacy_issues: Vec::new(),
        sessions: vec!["ses_closed".to_owned()],
    };
    let api = FakeOpenCodeApi::empty().with_session("ses_live", Path::new("/tmp/pg-host-checkout"));
    let reason = occupancy::foreign_protection_reason(
        Path::new("/tmp/repo"),
        &record,
        &NewAssociation::Unscoped { issue: 9 },
        Some(&EverythingClosed),
        Some(&api),
        &[],
    )
    .expect("a live foreign occupant conflicts");
    assert!(reason.contains("currently occupies"), "got: {reason}");
}

#[test]
fn an_orphan_legacy_scope_refuses_the_upgrade_instead_of_being_dropped() {
    let (_temp, storage, _env) = open_temp_db("registry-orphan-scope");
    legacy_store(&storage);
    storage
        .connection
        .execute(
            "INSERT INTO worktree_lease_scopes (lease_id, provider, project, external_id, task_id) \
             VALUES ('missing', 'forgejo', 'owner/repo', 7, NULL)",
            [],
        )
        .expect("orphan scope");
    let error = registry::ensure_schema(&storage).expect_err("orphan scope refuses");
    assert!(error.contains("missing lease"), "got: {error}");
    let rows: i64 = storage
        .connection
        .query_row("SELECT COUNT(*) FROM worktree_lease_scopes", [], |row| {
            row.get(0)
        })
        .expect("scope rows survive");
    assert_eq!(rows, 1);
}

#[test]
fn a_mixed_store_preview_merges_registry_and_legacy_rows_without_writing() {
    use crate::worktree::registry::{NewAssociation, NewDirectory};

    let (_temp, storage, _env) = open_temp_db("registry-mixed-preview");
    registry::ensure_schema(&storage).expect("schema");
    let registry_id = registry::stable_directory_id("/tmp/mixed/.git", "/tmp/mixed/registry");
    registry::insert_directory(
        &storage,
        &NewDirectory {
            directory_id: &registry_id,
            repo_identity: "/tmp/mixed/.git",
            checkout_path: "/tmp/mixed",
            worktree_path: "/tmp/mixed/registry",
            branch: "phasegent/registry",
            created_at: 3,
            session: "ses_registry",
            associations: &[NewAssociation::Unscoped { issue: 10 }],
        },
    )
    .expect("registry row");
    legacy_store(&storage);
    storage
        .connection
        .execute(
            "INSERT INTO worktree_leases \
                (lease_id, repo_identity, issue, session, checkout_path, worktree_path, branch, status, created_at, heartbeat_at) \
             VALUES ('legacy', '/tmp/mixed/.git', 11, 'ses_legacy', '/tmp/mixed', '/tmp/mixed/legacy', 'phasegent/legacy', 'retained', 2, 2)",
            [],
        )
        .expect("legacy row");
    scope(&storage, "legacy", "forgejo", "owner/repo", 11);

    let schema_before: i64 = storage
        .connection
        .query_row("PRAGMA schema_version", [], |row| row.get(0))
        .expect("schema version");
    let records = registry::load_records(&storage, Some("/tmp/mixed/.git")).expect("mixed read");
    assert_eq!(records.len(), 2, "both proven sources stay visible");
    assert!(
        records
            .iter()
            .any(|record| record.worktree_path == "/tmp/mixed/registry"),
        "the newer registry-only directory is never hidden"
    );
    assert!(
        records
            .iter()
            .any(|record| record.worktree_path == "/tmp/mixed/legacy"),
        "the legacy-only directory stays visible"
    );
    let schema_after: i64 = storage
        .connection
        .query_row("PRAGMA schema_version", [], |row| row.get(0))
        .expect("schema version");
    assert_eq!(schema_before, schema_after, "the preview must not write");
    let legacy_rows: i64 = storage
        .connection
        .query_row("SELECT COUNT(*) FROM worktree_leases", [], |row| row.get(0))
        .expect("legacy rows");
    assert_eq!(legacy_rows, 1);
}

#[test]
fn a_legacy_task_reference_without_a_binding_refuses_the_whole_upgrade() {
    let (_temp, storage, _env) = open_temp_db("registry-orphan-task");
    legacy_store(&storage);
    lease(&storage, "one", 7, "/tmp/repo/wt", "ses_a", 2);
    storage
        .connection
        .execute(
            "INSERT INTO worktree_lease_scopes \
                (lease_id, provider, project, external_id, task_id) \
             VALUES ('one', 'forgejo', 'owner/repo', 7, 999)",
            [],
        )
        .expect("orphan task scope");
    let error = registry::ensure_schema(&storage).expect_err("missing binding refuses");
    assert!(error.contains("no binding"), "got: {error}");
    let rows: i64 = storage
        .connection
        .query_row("SELECT COUNT(*) FROM worktree_lease_scopes", [], |row| {
            row.get(0)
        })
        .expect("rows survive");
    assert_eq!(rows, 1);
    let leases: i64 = storage
        .connection
        .query_row("SELECT COUNT(*) FROM worktree_leases", [], |row| row.get(0))
        .expect("lease survives");
    assert_eq!(leases, 1);
}

#[test]
fn a_legacy_task_reference_that_disagrees_with_its_binding_refuses_the_upgrade() {
    let (_temp, storage, _env) = open_temp_db("registry-mismatched-task");
    legacy_store(&storage);
    lease(&storage, "one", 7, "/tmp/repo/wt", "ses_a", 2);
    let key =
        crate::task_identity::TaskKey::new("forgejo", "https://forgejo.test", "owner/repo", "7")
            .expect("task key");
    let binding = crate::task_identity::ensure_binding(&storage, &key, 10).expect("binding");
    storage
        .connection
        .execute(
            "INSERT INTO worktree_lease_scopes \
                (lease_id, provider, project, external_id, task_id) \
             VALUES ('one', 'redmine', '23', 7, ?1)",
            [binding.task_id()],
        )
        .expect("mismatched task scope");
    let error = registry::ensure_schema(&storage).expect_err("mismatch refuses");
    assert!(error.contains("disagrees"), "got: {error}");
    let rows: i64 = storage
        .connection
        .query_row("SELECT COUNT(*) FROM worktree_lease_scopes", [], |row| {
            row.get(0)
        })
        .expect("rows survive");
    assert_eq!(rows, 1);
}

#[test]
fn a_legacy_task_with_a_contradictory_lease_number_refuses_the_upgrade() {
    let (_temp, storage, _env) = open_temp_db("registry-mismatched-lease-number");
    legacy_store(&storage);
    lease(&storage, "one", 8, "/tmp/repo/wt", "ses_a", 2);
    let key =
        crate::task_identity::TaskKey::new("forgejo", "https://forgejo.test", "owner/repo", "7")
            .expect("task key");
    let binding = crate::task_identity::ensure_binding(&storage, &key, 10).expect("binding");
    storage
        .connection
        .execute(
            "INSERT INTO worktree_lease_scopes \
                (lease_id, provider, project, external_id, task_id) \
             VALUES ('one', 'forgejo', 'owner/repo', 7, ?1)",
            [binding.task_id()],
        )
        .expect("scope");
    assert!(registry::ensure_schema(&storage).is_err());
    let count: i64 = storage
        .connection
        .query_row("SELECT COUNT(*) FROM worktree_lease_scopes", [], |row| {
            row.get(0)
        })
        .expect("scope evidence remains");
    assert_eq!(count, 1);
}

#[test]
fn a_read_only_projection_refuses_a_contradictory_legacy_task_reference() {
    let (_temp, storage, _env) = open_temp_db("registry-readonly-task-contradiction");
    legacy_store(&storage);
    lease(&storage, "one", 7, "/tmp/repo/wt", "ses_a", 2);
    let key =
        crate::task_identity::TaskKey::new("forgejo", "https://forgejo.test", "owner/repo", "7")
            .expect("task key");
    let binding = crate::task_identity::ensure_binding(&storage, &key, 10).expect("binding");
    storage
        .connection
        .execute(
            "INSERT INTO worktree_lease_scopes \
                (lease_id, provider, project, external_id, task_id) \
             VALUES ('one', 'redmine', '23', 7, ?1)",
            [binding.task_id()],
        )
        .expect("mismatched scope");
    let error = registry::load_records(&storage, Some("/tmp/repo/.git"))
        .expect_err("a contradictory legacy task reference must not be trusted");
    assert!(
        error.message.contains("disagrees"),
        "got: {}",
        error.message
    );
    let rows: i64 = storage
        .connection
        .query_row("SELECT COUNT(*) FROM worktree_lease_scopes", [], |row| {
            row.get(0)
        })
        .expect("rows survive");
    assert_eq!(rows, 1);
}

#[test]
fn upgrade_refuses_a_negative_legacy_issue_without_dropping_evidence() {
    let (_temp, storage, _env) = open_temp_db("registry-negative-issue");
    legacy_store(&storage);
    storage
        .connection
        .execute(
            "INSERT INTO worktree_leases \
                (lease_id, repo_identity, issue, session, checkout_path, worktree_path, branch, status, created_at, heartbeat_at) \
             VALUES ('negative', '/tmp/repo/.git', -3, 'ses_a', '/tmp/repo', '/tmp/repo/wt', 'phasegent/test', 'retained', 2, 2)",
            [],
        )
        .expect("negative legacy lease");

    assert!(registry::ensure_schema(&storage).is_err());
    let count: i64 = storage
        .connection
        .query_row(
            "SELECT COUNT(*) FROM worktree_leases WHERE lease_id = 'negative' AND issue = -3",
            [],
            |row| row.get(0),
        )
        .expect("negative evidence remains");
    assert_eq!(count, 1);
    let projected = registry::load_records(&storage, Some("/tmp/repo/.git"))
        .expect_err("the read-only projection must refuse the same unrepresentable value");
    assert!(
        projected.message.contains("unrepresentable"),
        "got: {}",
        projected.message
    );
}
