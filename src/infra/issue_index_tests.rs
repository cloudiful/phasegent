//! Focused tests for the independent issue index via temp files.
//! override so the real user database is never touched.

use crate::infra::issue_index::SqliteIssueIndex;
use crate::infra::issue_index_backend::block_on;
use crate::infra::issue_index_schema::DB_FILENAME_INDEX;
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::providers::index::{
    ISSUE_INDEX_MAX_CHUNK_BYTES, ISSUE_INDEX_MAX_CHUNKS, IssueIndexDocument, IssueIndexKey,
    IssueIndexStore, build_chunks, content_hash, hash_text,
};
use std::fs;
use std::path::PathBuf;

#[rustfmt::skip]
fn tmp_dir(l: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "phasegent-index-test-{l}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}
#[rustfmt::skip]
fn tmp_index(l: &str) -> (PathBuf, PathBuf) {
    let d = tmp_dir(l);
    let p = d.join(DB_FILENAME_INDEX);
    (d, p)
}

#[rustfmt::skip]
#[test]
fn schema_open_is_idempotent_and_private() {
    let (dir, path) = tmp_index("schema");
    let idx = SqliteIssueIndex::open_at(&path).unwrap();
    assert!(path.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let dm = fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
        & 0o777;
        assert_eq!(dm, 0o700);
        let fm = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(fm, 0o600);
    }
    let idx2 = SqliteIssueIndex::open_at(&path).unwrap();
    // Empty index has no lexical matches.
    assert_eq!(
        block_on(idx2.lexical_search("anything", 10, 0, false))
            .unwrap()
            .total_count,
        0
    );
    drop(idx);
    drop(idx2);
    let reopened = SqliteIssueIndex::open_at(&path).unwrap();
    assert_eq!(
        block_on(reopened.lexical_search("anything", 10, 0, false))
            .unwrap()
            .total_count,
        0
    );
    let _ = fs::remove_dir_all(dir);
}

#[rustfmt::skip]
#[test]
fn private_path_is_separate_and_env_override_works() {
    assert_ne!(DB_FILENAME_INDEX, "phasegent.sqlite3");
    assert!(DB_FILENAME_INDEX.contains("index"));
    let _lock = lock_workflow_tests();
    let (dir, path) = tmp_index("env");
    let _g = EnvGuard::set("PHASEGENT_INDEX_DB_PATH", path.to_str().unwrap());
    let idx = SqliteIssueIndex::open().unwrap();
    assert_eq!(idx.db_path(), path.as_path());
    assert!(path.exists());
    let _ = fs::remove_dir_all(dir);
}

#[rustfmt::skip]
#[test]
fn existing_parent_permissions_not_chmodded() {
    let base = tmp_dir("parent-perm");
    let existing = base.join("existing");
    fs::create_dir_all(&existing).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&existing, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let db_path = existing.join("index.sqlite3");
    let idx = SqliteIssueIndex::open_at(&db_path).unwrap();
    assert!(db_path.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = fs::metadata(&existing).unwrap().permissions().mode() & 0o777;
        assert_eq!(m, 0o755, "existing parent must not be chmodded");
        let fm = fs::metadata(&db_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(fm, 0o600);
    }
    let new_sub = existing.join("new_sub");
    let new_path = new_sub.join("index2.sqlite3");
    let idx2 = SqliteIssueIndex::open_at(&new_path).unwrap();
    assert!(new_path.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = fs::metadata(&new_sub).unwrap().permissions().mode() & 0o777;
        assert_eq!(m, 0o700);
    }
    let _ = fs::remove_dir_all(base);
    drop(idx);
    drop(idx2);
}

#[rustfmt::skip]
#[test]
fn relative_env_path_is_rejected() {
    let _lock = lock_workflow_tests();
    let _g = EnvGuard::set("PHASEGENT_INDEX_DB_PATH", "relative/path.db");
    let err = SqliteIssueIndex::open().err().expect("should fail");
    assert!(err.contains("absolute"));
}

#[rustfmt::skip]
#[test]
fn stable_identity_hash_and_chunk_utf8() {
    let k = IssueIndexKey::new("redmine", "owner/repo", "123").unwrap();
    assert_eq!(k.to_string(), "redmine:owner/repo:123");
    assert!(IssueIndexKey::new("", "owner/repo", "1").is_err());
    assert!(IssueIndexKey::new("forgejo", "owner/repo", "a\x01b").is_err());
    assert!(IssueIndexKey::new("x".repeat(300), "owner/repo", "1").is_err());
    let h1 = content_hash("Title", "Body", "open");
    assert_eq!(h1, content_hash("Title", "Body", "open"));
    assert_ne!(h1, content_hash("Title", "Body2", "open"));
    assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(h1.len(), 16);
    assert_eq!(hash_text("hello"), hash_text("hello"));
    let emoji = "😀";
    let text = emoji.repeat(1500);
    let chunks = build_chunks(&text, ISSUE_INDEX_MAX_CHUNK_BYTES, ISSUE_INDEX_MAX_CHUNKS).unwrap();
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].byte_start, 0);
    assert_eq!(chunks[0].byte_end, 4000);
    assert_eq!(chunks[1].byte_start, 4000);
    assert_eq!(chunks[1].byte_end, 6000);
    for (i, c) in chunks.iter().enumerate() {
        assert_eq!(c.ordinal, i);
        assert_eq!(c.text.len(), c.byte_end - c.byte_start);
        assert_eq!(c.hash, hash_text(&c.text));
    }
    let cjk = "汉";
    let text = cjk.repeat(2000);
    let chunks = build_chunks(&text, 4000, 64).unwrap();
    assert_eq!(chunks[0].text.len(), 3999);
    assert_eq!(chunks[0].byte_end, 3999);
    assert_eq!(chunks[1].byte_start, 3999);
    let big = "a".repeat(ISSUE_INDEX_MAX_CHUNK_BYTES * ISSUE_INDEX_MAX_CHUNKS + 1);
    assert!(build_chunks(&big, ISSUE_INDEX_MAX_CHUNK_BYTES, ISSUE_INDEX_MAX_CHUNKS).is_err());
    let exact = "a".repeat(ISSUE_INDEX_MAX_CHUNK_BYTES * ISSUE_INDEX_MAX_CHUNKS);
    assert_eq!(
        build_chunks(&exact, ISSUE_INDEX_MAX_CHUNK_BYTES, ISSUE_INDEX_MAX_CHUNKS)
            .unwrap()
            .len(),
        ISSUE_INDEX_MAX_CHUNKS
    );
    let key2 = IssueIndexKey::new("forgejo", "owner/repo", "trim-1").unwrap();
    let doc_trim = IssueIndexDocument::new(
        key2, 1, "  hello  ".into(), "body".into(), "  open  ".into(), None, None, 1_700_000_000,
    )
    .unwrap();
    assert_eq!(doc_trim.title, "hello");
    assert_eq!(doc_trim.state, "open");
    assert_eq!(doc_trim.content_hash, content_hash("hello", "body", "open"));
    doc_trim.validate().unwrap();
}

#[rustfmt::skip]
#[test]
fn upsert_replacement_and_round_trip() {
    let (dir, path) = tmp_index("upsert");
    let idx = SqliteIssueIndex::open_at(&path).unwrap();
    let key = IssueIndexKey::new("forgejo", "owner/repo", "42").unwrap();
    let doc1 = IssueIndexDocument::new(key.clone(),42,"Title v1".into(),"Body v1".into(),"open".into(),Some("https://example.com/42".into()),Some(1_700_000_000),1_700_000_100).unwrap();
    let h1 = doc1.content_hash.clone();
    assert_eq!(h1, doc1.content_hash);
    assert_eq!(doc1.chunks.len(), doc1.chunks.len());
    block_on(idx.upsert(&doc1)).unwrap();
    // Upsert is atomically visible via lexical search.
    let r1 = block_on(idx.lexical_search("Title", 10, 0, false)).unwrap();
    assert_eq!(r1.total_count, 1);
    assert_eq!(r1.items[0].title, "Title v1");
    assert_eq!(r1.items[0].external_id, "42");
    let doc2 = IssueIndexDocument::new(key.clone(),42,"Title v2".into(),"Body v2 longer 😀😀😀😀".into(),"closed".into(),Some("https://example.com/42".into()),Some(1_700_000_200),1_700_000_300).unwrap();
    block_on(idx.upsert(&doc2)).unwrap();
    let r2 = block_on(idx.lexical_search("Title", 10, 0, false)).unwrap();
    assert_eq!(r2.total_count, 1);
    assert_eq!(r2.items[0].title, "Title v2");
    assert_eq!(r2.items[0].state, "closed");
    // Replacement is atomic: old-only term no longer matches, new term does.
    assert_eq!(
        block_on(idx.lexical_search("v1", 10, 0, false)).unwrap().total_count,
        0
    );
    assert_eq!(
        block_on(idx.lexical_search("v2", 10, 0, false)).unwrap().total_count,
        1
    );
    // Unknown term has no matches.
    assert_eq!(
        block_on(idx.lexical_search("missing-term-xyz-999", 10, 0, false))
            .unwrap()
            .total_count,
        0
    );
    let _ = fs::remove_dir_all(dir);
}

#[rustfmt::skip]
#[test]
fn scoped_lexical_search_filters_by_source_project_and_state() {
    use crate::providers::index::LexicalScope;
    let (dir, path) = tmp_index("scoped");
    let idx = SqliteIssueIndex::open_at(&path).unwrap();
    // Same term in three scopes plus a closed variant.
    for (source, project, ext, num, title, state) in [
        ("forgejo", "owner/repo", "1", 1u64, "alpha scoped open", "open"),
        ("forgejo", "other/repo", "2", 2u64, "alpha other open", "open"),
        ("redmine", "42", "3", 3u64, "alpha redmine open", "open"),
        ("forgejo", "owner/repo", "4", 4u64, "alpha scoped closed", "closed"),
    ] {
        let key = IssueIndexKey::new(source, project, ext).unwrap();
        let doc = IssueIndexDocument::new(key, num, title.into(), "alpha body".into(), state.into(), None, None, 1_700_000_000 + num as i64).unwrap();
        block_on(idx.upsert(&doc)).unwrap();
    }
    // Global finds all four.
    let global = block_on(idx.lexical_search_scoped("alpha", 10, 0, false, &LexicalScope::global())).unwrap();
    assert_eq!(global.total_count, 4);
    // Scoped to forgejo/owner/repo finds two (open+closed).
    let scoped = LexicalScope::scoped("forgejo", "owner/repo", "all").unwrap();
    let res = block_on(idx.lexical_search_scoped("alpha", 10, 0, false, &scoped)).unwrap();
    assert_eq!(res.total_count, 2);
    assert!(res.items.iter().all(|item| item.project == "owner/repo"));
    // Scoped + state open finds one.
    let open_only = LexicalScope::scoped("forgejo", "owner/repo", "open").unwrap();
    let res = block_on(idx.lexical_search_scoped("alpha", 10, 0, false, &open_only)).unwrap();
    assert_eq!(res.total_count, 1);
    assert_eq!(res.items[0].external_id, "1");
    // Scoped + state closed finds the other.
    let closed_only = LexicalScope::scoped("forgejo", "owner/repo", "closed").unwrap();
    let res = block_on(idx.lexical_search_scoped("alpha", 10, 0, false, &closed_only)).unwrap();
    assert_eq!(res.total_count, 1);
    assert_eq!(res.items[0].external_id, "4");
    // Other scope is isolated.
    let other = LexicalScope::scoped("forgejo", "other/repo", "all").unwrap();
    let res = block_on(idx.lexical_search_scoped("alpha", 10, 0, false, &other)).unwrap();
    assert_eq!(res.total_count, 1);
    assert_eq!(res.items[0].external_id, "2");
    // Bounded pagination stays correct for scoped queries.
    let page = block_on(idx.lexical_search_scoped("alpha", 1, 0, false, &scoped)).unwrap();
    assert_eq!(page.items.len(), 1);
    assert!(page.has_more);
    assert_eq!(page.total_count, 2);
    let page2 = block_on(idx.lexical_search_scoped("alpha", 1, 1, false, &scoped)).unwrap();
    assert_eq!(page2.items.len(), 1);
    assert!(!page2.has_more);
    // Global lexical_search stays compatible (no scope filter).
    let compat = block_on(idx.lexical_search("alpha", 10, 0, false)).unwrap();
    assert_eq!(compat.total_count, 4);
    let _ = fs::remove_dir_all(dir);
}

#[rustfmt::skip]
#[test]
fn validation_failures() {
    assert!(IssueIndexKey::new("", "owner/repo", "1").is_err());
    assert!(IssueIndexKey::new("forgejo", "", "1").is_err());
    assert!(IssueIndexKey::new("forgejo", "owner/repo", "").is_err());
    assert!(IssueIndexKey::new("forgejo", "owner/repo", "a\x01").is_err());
    let k = IssueIndexKey::new("forgejo", "owner/repo", "1").unwrap();
    let r = IssueIndexDocument::new(k.clone(), 0, "T".into(), "B".into(), "open".into(), None, None, 1_700_000_000);
    assert!(r.is_err());
    let r = IssueIndexDocument::new(k.clone(), 1, "".into(), "B".into(), "open".into(), None, None, 1_700_000_000);
    assert!(r.is_err());
    let r = IssueIndexDocument::new(k.clone(), 1, "T".into(), "B".into(), "".into(), None, None, 1_700_000_000);
    assert!(r.is_err());
    let r = IssueIndexDocument::new(k.clone(), 1, "T".into(), "B".into(), "open".into(), None, None, 0);
    assert!(r.is_err());
    let r = IssueIndexDocument::new(k.clone(), 1, "T".into(), "B".into(), "open".into(), None, Some(0), 1_700_000_000);
    assert!(r.is_err());
    let big = "a".repeat(ISSUE_INDEX_MAX_CHUNK_BYTES * ISSUE_INDEX_MAX_CHUNKS + 1);
    let r = IssueIndexDocument::new(k.clone(), 1, "T".into(), big, "open".into(), None, None, 1_700_000_000);
    assert!(r.is_err());
    // Lexical search validates bounds and rejects empty queries without crashing.
    let (vdir, vpath) = tmp_index("val-lex");
    let vidx = SqliteIssueIndex::open_at(&vpath).unwrap();
    assert!(block_on(vidx.lexical_search("x", 0, 0, false)).is_err());
    assert!(block_on(vidx.lexical_search("", 10, 0, false)).is_err());
    assert!(block_on(vidx.lexical_search("   ", 10, 0, false)).is_err());
    drop(vidx);
    let _ = fs::remove_dir_all(vdir);
    // Invalid stored keys are rejected at document construction time.
    let invalid = IssueIndexKey {
        source: "".into(),
        project: "owner/repo".into(),
        external_id: "1".into(),
    };
    assert!(invalid.validate().is_err());
}

#[cfg(feature = "postgres")]
mod postgres_tests {
    use super::*;
    use crate::infra::issue_index_backend::block_on as pg_block;
    use crate::infra::issue_index_postgres::PostgresIssueIndex;
    use crate::providers::index::{IssueIndexDocument, IssueIndexKey, IssueIndexStore};

    fn test_pg_url() -> Option<String> {
        for name in ["PHASEGENT_TEST_PG_URL", "PHASEGENT_INDEX_PG_URL"] {
            if let Ok(v) = std::env::var(name) {
                let t = v.trim().to_owned();
                if !t.is_empty() {
                    return Some(t);
                }
            }
        }
        None
    }

    #[test]
    fn postgres_upsert_search_round_trip() {
        let Some(url) = test_pg_url() else {
            eprintln!("SKIP postgres_upsert_search: PHASEGENT_TEST_PG_URL not set");
            return;
        };
        // Never print the URL.
        assert!(!url.is_empty());
        let idx = pg_block(PostgresIssueIndex::open(&url))
            .expect("postgres open must succeed with valid URL");
        // Ensure clean slate for our unique keys (use unique source/project prefix).
        let suffix = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let source = format!("pgtest-{suffix}");
        let project = format!("proj/{suffix}");
        let key = IssueIndexKey::new(source.clone(), project.clone(), "1").unwrap();
        let doc = IssueIndexDocument::new(
            key.clone(),
            1,
            "hello postgres".into(),
            "body with tsvector test".into(),
            "open".into(),
            None,
            None,
            1_700_000_000,
        )
        .unwrap();
        pg_block(idx.upsert(&doc)).unwrap();
        let res = pg_block(idx.lexical_search("hello", 10, 0, false)).unwrap();
        // Bounded envelope must respect limit/offset and not leak URL.
        assert!(res.total_count >= 1);
        assert!(res.items.iter().any(|i| i.title == "hello postgres"));
        assert!(
            !res.items.iter().any(|i| i.body.is_some()),
            "body omitted by default"
        );
        let res_body = pg_block(idx.lexical_search("postgres", 10, 0, true)).unwrap();
        assert!(res_body.items.iter().any(|i| i.body.is_some()));
        // Replacement is atomically visible.
        let doc2 = IssueIndexDocument::new(
            key.clone(),
            1,
            "hello postgres revived".into(),
            "new body".into(),
            "open".into(),
            None,
            None,
            1_700_000_200,
        )
        .unwrap();
        pg_block(idx.upsert(&doc2)).unwrap();
        let revived = pg_block(idx.lexical_search("revived", 10, 0, false)).unwrap();
        assert!(revived.items.iter().any(|i| i.external_id == "1"));
        // Body cap on search output.
        let long_body = "a".repeat(crate::providers::api::ISSUE_SEARCH_MAX_BODY_BYTES + 50);
        let k2 = IssueIndexKey::new(source.clone(), project.clone(), "2").unwrap();
        let doc_long = IssueIndexDocument::new(
            k2.clone(),
            2,
            "long pg".into(),
            long_body.clone(),
            "open".into(),
            None,
            None,
            1_700_000_300,
        )
        .unwrap();
        pg_block(idx.upsert(&doc_long)).unwrap();
        let capped = pg_block(idx.lexical_search("long", 10, 0, true)).unwrap();
        let item = capped.items.iter().find(|i| i.external_id == "2").unwrap();
        assert_eq!(item.body_truncated, Some(true));
        assert_eq!(
            item.body.as_ref().unwrap().len(),
            crate::providers::api::ISSUE_SEARCH_MAX_BODY_BYTES
        );
        // No explicit cleanup; unique keys isolate this test.
    }

    #[test]
    fn postgres_backend_selection_requires_url() {
        // Phase 1 URL-driven selection: absent/blank PG URL selects SQLite
        // even when the legacy backend says postgres; only a non-empty URL
        // selects PostgreSQL. A legacy value can never force a different
        // backend and never fails open.
        let _lock = lock_workflow_tests();
        let db_path = super::tmp_index("pg-select").0.join("dummy.sqlite3");
        let storage = crate::infra::storage::Storage::open_at(&db_path).unwrap();
        storage
            .save_global_setting("PHASEGENT_INDEX_BACKEND", "postgres")
            .unwrap();
        storage
            .delete_global_setting("PHASEGENT_INDEX_PG_URL")
            .unwrap();
        let _unset_url =
            crate::infra::storage::test_support::EnvGuard::set("PHASEGENT_INDEX_PG_URL", "");
        let _unset_backend =
            crate::infra::storage::test_support::EnvGuard::set("PHASEGENT_INDEX_BACKEND", "");
        let kind = crate::infra::issue_index_backend::resolve_index_backend(&storage).unwrap();
        assert_eq!(
            kind,
            crate::infra::issue_index_backend::IndexBackendKind::Sqlite,
            "legacy postgres without URL must select SQLite"
        );
        let opened =
            crate::infra::issue_index_backend::IssueIndexBackend::open_blocking_with_storage(
                &storage,
            );
        assert!(
            opened.is_ok(),
            "legacy postgres without URL must open SQLite, not fail"
        );
        let _ = std::fs::remove_dir_all(db_path.parent().unwrap());
    }
}
