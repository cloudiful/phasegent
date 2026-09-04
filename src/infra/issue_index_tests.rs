//! Focused tests for the independent issue index via temp files.
//! override so the real user database is never touched.

use crate::infra::issue_index::SqliteIssueIndex;
use crate::infra::issue_index_backend::block_on;
use crate::infra::issue_index_schema::DB_FILENAME_INDEX;
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::providers::index::{
    ISSUE_INDEX_MAX_CHUNK_BYTES, ISSUE_INDEX_MAX_CHUNKS, ISSUE_INDEX_MAX_LIST_LIMIT,
    IssueIndexDocument, IssueIndexKey, IssueIndexListOptions, IssueIndexStore, build_chunks,
    content_hash, hash_text,
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
    let k = IssueIndexKey::new("forgejo", "owner/repo", "1").unwrap();
    assert!(block_on(idx2.get(&k)).unwrap().is_none());
    drop(idx);
    drop(idx2);
    let reopened = SqliteIssueIndex::open_at(&path).unwrap();
    let k2 = IssueIndexKey::new("forgejo", "owner/repo", "1").unwrap();
    assert!(block_on(reopened.get(&k2)).unwrap().is_none());
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
    block_on(idx.upsert(&doc1)).unwrap();
    let l1 = block_on(idx.get(&key)).unwrap().unwrap();
    assert_eq!(l1.title, "Title v1");
    assert_eq!(l1.content_hash, h1);
    assert_eq!(l1.chunks.len(), doc1.chunks.len());
    let doc2 = IssueIndexDocument::new(key.clone(),42,"Title v2".into(),"Body v2 longer 😀😀😀😀".into(),"closed".into(),Some("https://example.com/42".into()),Some(1_700_000_200),1_700_000_300).unwrap();
    block_on(idx.upsert(&doc2)).unwrap();
    let l2 = block_on(idx.get(&key)).unwrap().unwrap();
    assert_eq!(l2.title, "Title v2");
    assert_eq!(l2.state, "closed");
    assert_eq!(l2.chunks.len(), doc2.chunks.len());
    assert_eq!(l2.body, "Body v2 longer 😀😀😀😀");
    assert_eq!(
        l2.chunks.iter().map(|c| c.text.len()).sum::<usize>(),
        l2.body.len()
    );
    assert!(
        block_on(idx.get(&IssueIndexKey::new("forgejo", "owner/repo", "999").unwrap()))
            .unwrap()
            .is_none()
    );
    let _ = fs::remove_dir_all(dir);
}

#[rustfmt::skip]
#[test]
fn tombstone_missing_existing_and_resurrection() {
    let (dir, path) = tmp_index("tombstone");
    let idx = SqliteIssueIndex::open_at(&path).unwrap();
    let missing = IssueIndexKey::new("forgejo", "owner/repo", "missing-1").unwrap();
    block_on(idx.tombstone(&missing, 1_700_010_000)).unwrap();
    let t = block_on(idx.get(&missing)).unwrap().unwrap();
    assert!(t.deleted);
    assert_eq!(t.deleted_at, Some(1_700_010_000));
    assert!(t.chunks.is_empty());
    t.validate().unwrap();
    block_on(idx.tombstone(&missing, 1_700_010_010)).unwrap();
    assert!(block_on(idx.get(&missing)).unwrap().unwrap().deleted);
    let res = IssueIndexDocument::new(missing.clone(),99,"Resurrected".into(),"I am back".into(),"open".into(),None,None,1_700_020_000).unwrap();
    block_on(idx.upsert(&res)).unwrap();
    let back = block_on(idx.get(&missing)).unwrap().unwrap();
    assert!(!back.deleted);
    assert_eq!(back.deleted_at, None);
    assert_eq!(back.title, "Resurrected");
    back.validate().unwrap();
    let key = IssueIndexKey::new("redmine", "owner/repo", "55").unwrap();
    let doc = IssueIndexDocument::new(key.clone(),55,"To be deleted".into(),"Body to tombstone".into(),"open".into(),None,None,1_700_030_000).unwrap();
    block_on(idx.upsert(&doc)).unwrap();
    assert!(!block_on(idx.get(&key)).unwrap().unwrap().chunks.is_empty());
    block_on(idx.tombstone(&key, 1_700_040_000)).unwrap();
    let tomb = block_on(idx.get(&key)).unwrap().unwrap();
    assert!(tomb.deleted);
    assert_eq!(tomb.deleted_at, Some(1_700_040_000));
    assert!(tomb.chunks.is_empty());
    tomb.validate().unwrap();
    block_on(idx.tombstone(&key, 1_700_040_010)).unwrap();
    assert!(block_on(idx.get(&key)).unwrap().unwrap().deleted);
    let doc2 = IssueIndexDocument::new(key.clone(),55,"To be deleted".into(),"New body".into(),"open".into(),None,None,1_700_050_000).unwrap();
    block_on(idx.upsert(&doc2)).unwrap();
    let res2 = block_on(idx.get(&key)).unwrap().unwrap();
    assert!(!res2.deleted);
    assert_eq!(res2.deleted_at, None);
    assert_eq!(res2.body, "New body");
    res2.validate().unwrap();
    let _ = fs::remove_dir_all(dir);
}

#[rustfmt::skip]
#[test]
fn bounded_list_pagination() {
    let (dir, path) = tmp_index("list");
    let idx = SqliteIssueIndex::open_at(&path).unwrap();
    for i in 0..5 {
        let k = IssueIndexKey::new("forgejo", "owner/repo", format!("{i:03}")).unwrap();
        let d = IssueIndexDocument::new(k,i as u64+1,format!("Title {i}"),format!("Body {i}"),"open".into(),None,None,1_700_000_000+i as i64).unwrap();
        block_on(idx.upsert(&d)).unwrap();
    }
    let p1 = block_on(idx
        .list(&IssueIndexListOptions::new(2, 0).unwrap()))
        .unwrap();
    assert_eq!(p1.len(), 2);
    assert_eq!(p1[0].key.external_id, "000");
    assert_eq!(p1[1].key.external_id, "001");
    for d in &p1 {
        assert_eq!(
            d.chunks.iter().map(|c| c.text.len()).sum::<usize>(),
            d.body.len()
        );
    }
    let p2 = block_on(idx
        .list(&IssueIndexListOptions::new(2, 2).unwrap()))
        .unwrap();
    assert_eq!(p2[0].key.external_id, "002");
    assert_eq!(p2[1].key.external_id, "003");
    assert_eq!(
        block_on(idx.list(&IssueIndexListOptions::new(2, 4).unwrap()))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        block_on(idx.list(&IssueIndexListOptions::new(2, 10).unwrap()))
            .unwrap()
            .len(),
        0
    );
    assert!(IssueIndexListOptions::new(0, 0).is_err());
    assert!(IssueIndexListOptions::new(ISSUE_INDEX_MAX_LIST_LIMIT + 1, 0).is_err());
    let tk = IssueIndexKey::new("forgejo", "owner/repo", "002").unwrap();
    block_on(idx.tombstone(&tk, 1_700_010_000)).unwrap();
    assert_eq!(
        block_on(idx.list(&IssueIndexListOptions::new(10, 0).unwrap()))
            .unwrap()
            .len(),
        4
    );
    let res = IssueIndexDocument::new(tk.clone(),3,"Title 2".into(),"Body 2".into(),"open".into(),None,None,1_700_020_000).unwrap();
    block_on(idx.upsert(&res)).unwrap();
    assert_eq!(
        block_on(idx.list(&IssueIndexListOptions::new(10, 0).unwrap()))
            .unwrap()
            .len(),
        5
    );
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
    assert!(IssueIndexListOptions::new(0, 0).is_err());
    assert!(IssueIndexListOptions::new(ISSUE_INDEX_MAX_LIST_LIMIT + 1, 0).is_err());
    let (dir, path) = tmp_index("val-tomb");
    let idx = SqliteIssueIndex::open_at(&path).unwrap();
    let valid = IssueIndexKey::new("forgejo", "owner/repo", "1").unwrap();
    assert!(block_on(idx.tombstone(&valid, 0)).is_err());
    assert!(block_on(idx.tombstone(&valid, -5)).is_err());
    let invalid = IssueIndexKey {
        source: "".into(),
        project: "owner/repo".into(),
        external_id: "1".into(),
    };
    assert!(block_on(idx.get(&invalid)).is_err());
    let _ = fs::remove_dir_all(dir);
    let (dir2, path2) = tmp_index("val-get2");
    let idx2 = SqliteIssueIndex::open_at(&path2).unwrap();
    let inv2 = IssueIndexKey {
        source: "".into(),
        project: "owner/repo".into(),
        external_id: "1".into(),
    };
    assert!(block_on(idx2.get(&inv2)).is_err());
    let _ = fs::remove_dir_all(dir2);
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
    fn postgres_upsert_search_and_tombstone_round_trip() {
        let Some(url) = test_pg_url() else {
            eprintln!("SKIP postgres_upsert_search: PHASEGENT_TEST_PG_URL not set");
            return;
        };
        // Never print the URL.
        assert!(!url.is_empty());
        let idx = pg_block(PostgresIssueIndex::open(&url)).expect("postgres open must succeed with valid URL");
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
        assert!(!res.items.iter().any(|i| i.body.is_some()), "body omitted by default");
        let res_body = pg_block(idx.lexical_search("postgres", 10, 0, true)).unwrap();
        assert!(res_body.items.iter().any(|i| i.body.is_some()));
        // Tombstone must remove from search.
        pg_block(idx.tombstone(&key, 1_700_000_100)).unwrap();
        let after = pg_block(idx.lexical_search("hello", 10, 0, false)).unwrap();
        assert!(!after.items.iter().any(|i| i.external_id == "1" && i.source == source));
        // Resurrect.
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
        let doc_long = IssueIndexDocument::new(k2.clone(), 2, "long pg".into(), long_body.clone(), "open".into(), None, None, 1_700_000_300).unwrap();
        pg_block(idx.upsert(&doc_long)).unwrap();
        let capped = pg_block(idx.lexical_search("long", 10, 0, true)).unwrap();
        let item = capped.items.iter().find(|i| i.external_id == "2").unwrap();
        assert_eq!(item.body_truncated, Some(true));
        assert_eq!(item.body.as_ref().unwrap().len(), crate::providers::api::ISSUE_SEARCH_MAX_BODY_BYTES);
        // Clean up keys.
        pg_block(idx.tombstone(&k2, 1_700_000_400)).unwrap();
        let _ = pg_block(idx.tombstone(&key, 1_700_000_401));
    }

    #[test]
    fn postgres_backend_selection_requires_url() {
        let _lock = lock_workflow_tests();
        let db_path = super::tmp_index("pg-select").0.join("dummy.sqlite3");
        let storage = crate::infra::storage::Storage::open_at(&db_path).unwrap();
        // Persist postgres without URL -> backend open must fail, not fallback.
        storage
            .save_global_setting("PHASEGENT_INDEX_BACKEND", "postgres")
            .unwrap();
        storage.delete_global_setting("PHASEGENT_INDEX_PG_URL").unwrap();
        let err = match pg_block(
            crate::infra::issue_index_backend::IssueIndexBackend::open_with_storage(&storage),
        ) {
            Ok(_) => panic!("postgres without URL must fail, not fallback to sqlite"),
            Err(err) => err,
        };
        assert!(err.contains("PHASEGENT_INDEX_PG_URL"), "got: {err}");
        // No URL leak.
        assert!(!err.to_ascii_lowercase().contains("postgres://"), "leak: {err}");
        let _ = std::fs::remove_dir_all(db_path.parent().unwrap());
    }
}
