//! Local provider cross-backend parity.
//!
//! The local provider is SQLite-first; PostgreSQL is selected by the
//! same non-empty `PHASEGENT_INDEX_PG_URL` as the index (single-active,
//! never dual-written). This module runs a shared scenario on SQLite so
//! the baseline is always exercised, and — when the `postgres` feature
//! is enabled and a live PG URL is present — verifies the mirrored
//! schema and seeds so a future PostgreSQL CRUD arm cannot drift from
//! the SQLite one. Without a live PG database the PostgreSQL arm skips;
//! the SQLite baseline always runs.

use crate::providers::api::IssueSearchOptions;
use crate::providers::local::LocalProvider;
use std::path::PathBuf;

fn tmp_provider(label: &str) -> (LocalProvider, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "phasegent-local-cross-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("phasegent-local.sqlite3");
    let provider = LocalProvider::open_at(&path).unwrap();
    (provider, dir)
}

fn search_open() -> IssueSearchOptions {
    IssueSearchOptions {
        query: None,
        state: "open".to_owned(),
        page: 1,
        limit: 50,
        include_body: false,
        all: true,
    }
}

#[test]
fn sqlite_cross_backend_issue_comment_round_trip() {
    // Shared baseline: the provider CRUD a PostgreSQL backend must
    // reproduce identically. Kept aligned with
    // `contract_tests::crud_round_trip_keeps_redmine_envelope`.
    let (provider, dir) = tmp_provider("sqlite");
    let created = provider.create_issue("Cross backend", "body").unwrap();
    assert!(created.id > 0);
    assert_eq!(created.number, created.id);
    assert_eq!(created.state, "open");

    let fetched = provider.get_issue(created.number).unwrap();
    assert_eq!(fetched.title, "Cross backend");

    let search = provider.search_issues(&search_open()).unwrap();
    assert!(
        search
            .items
            .iter()
            .any(|item| item.number == created.number),
        "created issue must be searchable"
    );

    let updated = provider.update_body(created.number, "New body").unwrap();
    assert_eq!(updated.body, "New body");

    let marker = "<!-- cross-marker -->";
    let comment = provider
        .create_comment(created.number, "comment body", marker)
        .unwrap();
    assert_eq!(comment.marker.as_deref(), Some(marker));
    let found = provider.find_marker(created.number, marker).unwrap();
    assert_eq!(found.id, comment.id);

    let _ = std::fs::remove_dir_all(dir);
}

/// PostgreSQL local backend parity. Gated behind the `postgres` feature
/// (and a live `PHASEGENT_TEST_PG_URL` / `PHASEGENT_INDEX_PG_URL`) so
/// the default `cargo test` needs no database and the PG arm compiles to
/// a graceful skip when no server is available.
#[cfg(feature = "postgres")]
mod postgres_cross_backend {
    use crate::infra::issue_index_backend::block_on;
    use crate::infra::local_store::PostgresLocalStore;

    fn test_pg_url() -> Option<String> {
        for name in ["PHASEGENT_TEST_PG_URL", "PHASEGENT_INDEX_PG_URL"] {
            if let Ok(value) = std::env::var(name) {
                let trimmed = value.trim().to_owned();
                if !trimmed.is_empty() {
                    return Some(trimmed);
                }
            }
        }
        None
    }

    #[test]
    fn postgres_schema_and_seed_parity() {
        let Some(url) = test_pg_url() else {
            eprintln!("SKIP postgres cross_backend: PHASEGENT_TEST_PG_URL not set");
            return;
        };
        // Never print the URL.
        assert!(!url.is_empty());

        // Opening applies the embedded mirrored migration
        // (migrations/pg/0002_local.sql): a valid, additive SQL file with
        // matching column names is required for this to succeed.
        block_on(PostgresLocalStore::open(&url))
            .expect("postgres local store must apply the embedded migration");

        // The store's pool is private, so inspect through an independent
        // pool over the same URL to assert schema/seed parity.
        let pool = block_on(
            sqlx::postgres::PgPoolOptions::new()
                .max_connections(2)
                .connect(&url),
        )
        .expect("raw postgres pool must connect");

        for table in [
            "local_issues",
            "local_comments",
            "local_projects",
            "status_transitions",
        ] {
            let count: i64 = block_on(
                sqlx::query_scalar(
                    "SELECT COUNT(*) FROM information_schema.tables \
                     WHERE table_schema = 'public' AND table_name = $1",
                )
                .bind(table)
                .fetch_one(&pool),
            )
            .expect("table existence query must run");
            assert_eq!(count, 1, "postgres must expose table {table}");
        }

        // Seed parity mirrors local_sql/seed.sql.
        let default_project: i64 = block_on(
            sqlx::query_scalar("SELECT COUNT(*) FROM local_projects WHERE project = 'default'")
                .fetch_one(&pool),
        )
        .expect("project seed query must run");
        assert_eq!(default_project, 1);

        let edges: i64 = block_on(
            sqlx::query_scalar("SELECT COUNT(*) FROM status_transitions").fetch_one(&pool),
        )
        .expect("status seed query must run");
        assert_eq!(edges, 16);

        // Constraint parity: the SQLite schema enforces the same
        // `length(project) <= 200` CHECK as PG, so a too-long identifier
        // must be rejected on PG as well.
        let long = "x".repeat(201);
        let oversized = block_on(
            sqlx::query(
                "INSERT INTO local_projects (project, description, created_at) \
                         VALUES ($1, '', 1)",
            )
            .bind(&long)
            .execute(&pool),
        );
        assert!(
            oversized.is_err(),
            "postgres must reject a project identifier longer than 200 bytes"
        );

        // Marker uniqueness parity (local_comments.marker is UNIQUE).
        let issue_id: i64 = block_on(
            sqlx::query_scalar(
                "INSERT INTO local_issues (title, body, status, project, tracker, \
                 author_role, created_at, updated_at) \
                 VALUES ($1, '', 'New', 'default', 'Task', 'executor', 1, 1) \
                 RETURNING id",
            )
            .bind("cross-pg-issue")
            .fetch_one(&pool),
        )
        .expect("seed issue must insert");
        let insert_comment = |marker: &str| {
            block_on(
                sqlx::query(
                    "INSERT INTO local_comments (issue_id, role, phase, attempt, marker, body, created_at) \
                     VALUES ($1, 'executor', '', 1, $2, '', 1)",
                )
                .bind(issue_id)
                .bind(marker)
                .execute(&pool),
            )
        };
        insert_comment("pg-marker").expect("first marker must insert");
        assert!(
            insert_comment("pg-marker").is_err(),
            "postgres must reject a duplicate marker"
        );
    }
}
