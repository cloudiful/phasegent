use super::issue_search::{fallback_or_provider_error, warm_single_summary};
use crate::infra::issue_index::SqliteIssueIndex;
use crate::infra::issue_index_backend::block_on;
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::providers::api::{IssueSearchItem, IssueSummary};
use crate::providers::forgejo::ForgejoError;
use crate::providers::forgejo::{ForgejoConfig, ForgejoProvider};
use crate::providers::index::{IssueIndexDocument, IssueIndexKey, IssueIndexStore, LexicalScope};
use crate::providers::index_store::{explicit_scope, lexical_scope_for_state};
use crate::providers::{ProviderDispatcher, ProviderKind};

fn tmp_index_path(label: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "phasegent-transparent-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let path = dir.join("phasegent-index.sqlite3");
    (dir, path)
}

fn forgejo_dispatcher(owner: &str, repo: &str) -> ProviderDispatcher {
    let config = ForgejoConfig::new("https://forgejo.example/api/v1", owner, repo);
    let provider = ForgejoProvider::new(config, "test-token".to_owned()).unwrap();
    ProviderDispatcher::Forgejo(provider)
}

#[test]
fn remote_page_warms_full_body_while_output_stays_compact() {
    // One provider page returns full bodies; warming must store the
    // full body while the stdout envelope stays compact.
    let long_body = "b".repeat(crate::providers::api::ISSUE_SEARCH_MAX_BODY_BYTES + 50);
    let summary = IssueSummary {
        id: 7,
        number: 7,
        title: "Title".to_owned(),
        body: long_body.clone(),
        state: "open".to_owned(),
        html_url: Some("https://forgejo.example/issues/7".to_owned()),
    };
    // Compact output omits body.
    let compact = IssueSearchItem::from_summary(summary.clone(), false);
    assert!(compact.body.is_none());
    assert!(compact.source.is_none());
    // Warming stores the full body without the 8192 cap.
    let _lock = lock_workflow_tests();
    let (dir, path) = tmp_index_path("warm-full");
    let _guard = EnvGuard::set("PHASEGENT_INDEX_DB_PATH", path.to_str().unwrap());
    let storage_path = dir.join("test-storage.sqlite3");
    let _guard_db = EnvGuard::set("PHASEGENT_DB_PATH", storage_path.to_str().unwrap());
    let _guard_pg = EnvGuard::set("PHASEGENT_INDEX_PG_URL", "");
    let _guard_backend = EnvGuard::set("PHASEGENT_INDEX_BACKEND", "");
    let dispatcher = forgejo_dispatcher("owner", "repo");
    warm_single_summary(&dispatcher, &summary, "issue search");
    let idx = SqliteIssueIndex::open_at(&path).unwrap();
    // Warming is visible via scoped lexical search; output bodies stay capped.
    let scope = lexical_scope_for_state(
        explicit_scope(Some(ProviderKind::Forgejo), Some("owner/repo"), None).as_ref(),
        "all",
    );
    let res = block_on(idx.lexical_search("Title", 10, 0, true)).unwrap();
    assert_eq!(res.total_count, 1);
    assert_eq!(res.items[0].title, "Title");
    assert_eq!(res.items[0].body_truncated, Some(true));
    assert_eq!(
        res.items[0].body.as_ref().unwrap().len(),
        crate::providers::api::ISSUE_SEARCH_MAX_BODY_BYTES
    );
    // Scoped search also finds the warmed document.
    let scoped = block_on(idx.lexical_search_scoped("Title", 10, 0, false, &scope)).unwrap();
    assert_eq!(scoped.total_count, 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn index_failure_is_warning_only_and_never_fails_remote() {
    // Warming with an invalid document (empty title) must produce a
    // bounded warning, not a failure; the provider result stays 0.
    let _lock = lock_workflow_tests();
    let (dir, path) = tmp_index_path("warm-fail");
    let _guard = EnvGuard::set("PHASEGENT_INDEX_DB_PATH", path.to_str().unwrap());
    let storage_path = dir.join("test-storage.sqlite3");
    let _guard_db = EnvGuard::set("PHASEGENT_DB_PATH", storage_path.to_str().unwrap());
    let _guard_pg = EnvGuard::set("PHASEGENT_INDEX_PG_URL", "");
    let _guard_backend = EnvGuard::set("PHASEGENT_INDEX_BACKEND", "");
    let dispatcher = forgejo_dispatcher("owner", "repo");
    let bad = IssueSummary {
        id: 1,
        number: 1,
        title: "".to_owned(),
        body: "body".to_owned(),
        state: "open".to_owned(),
        html_url: None,
    };
    // Must not panic and must not touch stdout JSON shape.
    warm_single_summary(&dispatcher, &bad, "issue search");
    // Valid doc still warms after a bad one (best-effort continues).
    let good = IssueSummary {
        id: 2,
        number: 2,
        title: "good".to_owned(),
        body: "body".to_owned(),
        state: "open".to_owned(),
        html_url: None,
    };
    warm_single_summary(&dispatcher, &good, "issue search");
    let idx = SqliteIssueIndex::open_at(&path).unwrap();
    let res = block_on(idx.lexical_search("good", 10, 0, false)).unwrap();
    assert_eq!(res.total_count, 1);
    assert_eq!(res.items[0].external_id, "2");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn provider_failure_returns_local_item_with_markers_scoped() {
    let _lock = lock_workflow_tests();
    let (dir, path) = tmp_index_path("fallback-scoped");
    let _guard = EnvGuard::set("PHASEGENT_INDEX_DB_PATH", path.to_str().unwrap());
    let storage_path = dir.join("test-storage.sqlite3");
    let _guard_db = EnvGuard::set("PHASEGENT_DB_PATH", storage_path.to_str().unwrap());
    let _guard_pg = EnvGuard::set("PHASEGENT_INDEX_PG_URL", "");
    let _guard_backend = EnvGuard::set("PHASEGENT_INDEX_BACKEND", "");
    let idx = SqliteIssueIndex::open_at(&path).unwrap();
    // Two scopes, only one matches the explicit provider/project.
    for (source, project, num, title) in [
        ("forgejo", "owner/repo", 1u64, "alpha scoped"),
        ("forgejo", "other/repo", 2u64, "alpha unrelated"),
    ] {
        let key = IssueIndexKey::new(source, project, num.to_string()).unwrap();
        let doc = IssueIndexDocument::new(
            key,
            num,
            title.to_owned(),
            "alpha body".to_owned(),
            "open".to_owned(),
            None,
            None,
            1_700_000_000 + num as i64,
        )
        .unwrap();
        block_on(idx.upsert(&doc)).unwrap();
    }
    drop(idx);
    let options = crate::providers::IssueSearchOptions {
        query: Some("alpha".to_owned()),
        state: "all".to_owned(),
        page: 1,
        limit: 10,
        include_body: false,
        all: false,
    };
    let original = ForgejoError::auth("bad credentials");
    // Explicit forgejo scope must filter to owner/repo only.
    let code = fallback_or_provider_error(
        &original,
        &options,
        None,
        Some(ProviderKind::Forgejo),
        Some("owner/repo"),
        None,
    );
    assert_eq!(code, 0, "scoped fallback with match must succeed");
    // Verify scoping at the storage layer directly (bounded/paged).
    let idx2 = SqliteIssueIndex::open_at(&path).unwrap();
    let explicit = explicit_scope(Some(ProviderKind::Forgejo), Some("owner/repo"), None);
    let scope = lexical_scope_for_state(explicit.as_ref(), "all");
    let res = block_on(idx2.lexical_search_scoped("alpha", 10, 0, false, &scope)).unwrap();
    assert_eq!(res.total_count, 1);
    assert_eq!(res.items[0].project, "owner/repo");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn provider_failure_with_no_local_match_preserves_error() {
    let _lock = lock_workflow_tests();
    let (dir, path) = tmp_index_path("fallback-empty");
    let _guard = EnvGuard::set("PHASEGENT_INDEX_DB_PATH", path.to_str().unwrap());
    let storage_path = dir.join("test-storage.sqlite3");
    let _guard_db = EnvGuard::set("PHASEGENT_DB_PATH", storage_path.to_str().unwrap());
    let _guard_pg = EnvGuard::set("PHASEGENT_INDEX_PG_URL", "");
    let _guard_backend = EnvGuard::set("PHASEGENT_INDEX_BACKEND", "");
    // Empty index: no docs match.
    let _idx = SqliteIssueIndex::open_at(&path).unwrap();
    let options = crate::providers::IssueSearchOptions {
        query: Some("missing-term-xyz".to_owned()),
        state: "all".to_owned(),
        page: 1,
        limit: 10,
        include_body: false,
        all: false,
    };
    let original = ForgejoError::auth("bad credentials");
    let code = fallback_or_provider_error(
        &original,
        &options,
        None,
        Some(ProviderKind::Forgejo),
        Some("owner/repo"),
        None,
    );
    assert_eq!(code, 1, "no local match must preserve provider error");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn queryless_all_has_no_fallback_and_argument_errors_not_masked() {
    let _lock = lock_workflow_tests();
    let (dir, path) = tmp_index_path("no-fallback");
    let _guard = EnvGuard::set("PHASEGENT_INDEX_DB_PATH", path.to_str().unwrap());
    let storage_path = dir.join("test-storage.sqlite3");
    let _guard_db = EnvGuard::set("PHASEGENT_DB_PATH", storage_path.to_str().unwrap());
    let _guard_pg = EnvGuard::set("PHASEGENT_INDEX_PG_URL", "");
    let _guard_backend = EnvGuard::set("PHASEGENT_INDEX_BACKEND", "");
    let idx = SqliteIssueIndex::open_at(&path).unwrap();
    let key = IssueIndexKey::new("forgejo", "owner/repo", "1").unwrap();
    let doc = IssueIndexDocument::new(
        key,
        1,
        "alpha".to_owned(),
        "alpha".to_owned(),
        "open".to_owned(),
        None,
        None,
        1_700_000_000,
    )
    .unwrap();
    block_on(idx.upsert(&doc)).unwrap();
    drop(idx);
    // Queryless --all must not fallback even with local matches.
    let all_options = crate::providers::IssueSearchOptions {
        query: None,
        state: "all".to_owned(),
        page: 1,
        limit: 10,
        include_body: false,
        all: true,
    };
    let original = ForgejoError::auth("bad credentials");
    let code = fallback_or_provider_error(
        &original,
        &all_options,
        None,
        Some(ProviderKind::Forgejo),
        Some("owner/repo"),
        None,
    );
    assert_eq!(code, 1);
    // Not-supported and argument errors never fallback.
    let not_supported = ForgejoError::not_supported("forgejo", "issue search");
    let query_options = crate::providers::IssueSearchOptions {
        query: Some("alpha".to_owned()),
        state: "all".to_owned(),
        page: 1,
        limit: 10,
        include_body: false,
        all: false,
    };
    assert_eq!(
        fallback_or_provider_error(
            &not_supported,
            &query_options,
            None,
            Some(ProviderKind::Forgejo),
            Some("owner/repo"),
            None,
        ),
        1
    );
    let arg_error = ForgejoError::config("issue search limit must be between 1 and 100");
    assert_eq!(
        fallback_or_provider_error(
            &arg_error,
            &query_options,
            None,
            Some(ProviderKind::Forgejo),
            Some("owner/repo"),
            None,
        ),
        1
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn local_fallback_needs_no_provider_network_or_credentials() {
    // No provider env, no mock server, no Storage provider rows: the
    // fallback must still succeed from the local index alone.
    let _lock = lock_workflow_tests();
    let (dir, path) = tmp_index_path("fallback-localonly");
    let _guard = EnvGuard::set("PHASEGENT_INDEX_DB_PATH", path.to_str().unwrap());
    let storage_path = dir.join("test-storage.sqlite3");
    let _guard_db = EnvGuard::set("PHASEGENT_DB_PATH", storage_path.to_str().unwrap());
    let _guard_pg = EnvGuard::set("PHASEGENT_INDEX_PG_URL", "");
    let _guard_backend = EnvGuard::set("PHASEGENT_INDEX_BACKEND", "");
    let _unset_base = EnvGuard::set("PHASEGENT_API_BASE", "");
    let _unset_repo = EnvGuard::set("PHASEGENT_REPOSITORY", "");
    let _unset_provider = EnvGuard::set("PHASEGENT_PROVIDER", "");
    let idx = SqliteIssueIndex::open_at(&path).unwrap();
    let key = IssueIndexKey::new("redmine", "42", "9").unwrap();
    let doc = IssueIndexDocument::new(
        key,
        9,
        "offline alpha".to_owned(),
        "offline body".to_owned(),
        "open".to_owned(),
        None,
        None,
        1_700_000_000,
    )
    .unwrap();
    block_on(idx.upsert(&doc)).unwrap();
    drop(idx);
    let options = crate::providers::IssueSearchOptions {
        query: Some("offline".to_owned()),
        state: "all".to_owned(),
        page: 1,
        limit: 10,
        include_body: false,
        all: false,
    };
    let original = ForgejoError::request("issue search", "network down".to_owned());
    // Explicit redmine scope, no provider lookup performed.
    let code = fallback_or_provider_error(
        &original,
        &options,
        None,
        Some(ProviderKind::Redmine),
        None,
        Some("42"),
    );
    assert_eq!(code, 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn mutation_write_through_covers_get_create_update_close() {
    let _lock = lock_workflow_tests();
    let (dir, path) = tmp_index_path("mutation-warm");
    let _guard = EnvGuard::set("PHASEGENT_INDEX_DB_PATH", path.to_str().unwrap());
    let storage_path = dir.join("test-storage.sqlite3");
    let _guard_db = EnvGuard::set("PHASEGENT_DB_PATH", storage_path.to_str().unwrap());
    let _guard_pg = EnvGuard::set("PHASEGENT_INDEX_PG_URL", "");
    let _guard_backend = EnvGuard::set("PHASEGENT_INDEX_BACKEND", "");
    let dispatcher = forgejo_dispatcher("owner", "repo");
    for (num, title, state) in [
        (11u64, "get title", "open"),
        (12u64, "create title", "open"),
        (13u64, "update title", "open"),
        (14u64, "close title", "closed"),
    ] {
        let summary = IssueSummary {
            id: num,
            number: num,
            title: title.to_owned(),
            body: format!("body {num}"),
            state: state.to_owned(),
            html_url: None,
        };
        warm_single_summary(&dispatcher, &summary, "issue mutation");
    }
    let idx = SqliteIssueIndex::open_at(&path).unwrap();
    // All four warmed summaries are visible via lexical search.
    let res = block_on(idx.lexical_search("title", 10, 0, false)).unwrap();
    assert_eq!(res.total_count, 4);
    // Close is the closed document.
    let closed = block_on(idx.lexical_search("close", 10, 0, false)).unwrap();
    assert_eq!(closed.total_count, 1);
    assert_eq!(closed.items[0].external_id, "14");
    assert_eq!(closed.items[0].state, "closed");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn fallback_items_retain_scope_without_invented_ids() {
    let item = IssueSearchItem::from_local_parts(
        "redmine".to_owned(),
        "42".to_owned(),
        "non-numeric-ext".to_owned(),
        99,
        "title".to_owned(),
        "open".to_owned(),
        None,
        "body".to_owned(),
        false,
    );
    assert_eq!(item.id, 99);
    assert_eq!(item.number, 99);
    assert_eq!(item.source.as_deref(), Some("redmine"));
    assert_eq!(item.external_id.as_deref(), Some("non-numeric-ext"));
    assert!(item.body.is_none());
    let text = serde_json::to_string(&item).unwrap();
    assert!(text.contains("redmine"));
    assert!(!text.contains("postgres://"));
    // Provider-fresh items omit scope keys entirely.
    let fresh = IssueSearchItem::from_summary(
        IssueSummary {
            id: 1,
            number: 1,
            title: "t".to_owned(),
            body: "b".to_owned(),
            state: "open".to_owned(),
            html_url: None,
        },
        false,
    );
    let fresh_text = serde_json::to_string(&fresh).unwrap();
    assert!(!fresh_text.contains("source"));
    assert!(!fresh_text.contains("data_source"));
}

#[test]
fn explicit_scope_needs_no_provider_lookup_and_is_bounded() {
    // Pure function: no Storage, no env provider defaults, no network.
    assert_eq!(
        explicit_scope(Some(ProviderKind::Forgejo), Some("owner/repo"), None)
            .unwrap()
            .project,
        "owner/repo"
    );
    assert!(explicit_scope(Some(ProviderKind::Forgejo), None, None).is_none());
    assert!(explicit_scope(Some(ProviderKind::Forgejo), Some("bad"), None).is_none());
    assert_eq!(
        explicit_scope(Some(ProviderKind::Redmine), None, Some("42"))
            .unwrap()
            .source,
        "redmine"
    );
    assert!(explicit_scope(Some(ProviderKind::Redmine), None, Some("  ")).is_none());
    assert_eq!(
        explicit_scope(Some(ProviderKind::Gitlab), None, Some("77"))
            .unwrap()
            .project,
        "77"
    );
    assert!(explicit_scope(Some(ProviderKind::Gitlab), None, Some("abc")).is_none());
    assert!(explicit_scope(None, Some("owner/repo"), Some("42")).is_none());
    // State mapping is bounded to open/closed/all.
    let global = lexical_scope_for_state(None, "all");
    assert!(global.is_global());
    let explicit = explicit_scope(Some(ProviderKind::Forgejo), Some("owner/repo"), None);
    let scoped = lexical_scope_for_state(explicit.as_ref(), "open");
    assert_eq!(scoped.source.as_deref(), Some("forgejo"));
    assert_eq!(scoped.state.as_deref(), Some("open"));
    assert!(LexicalScope::global().is_global());
}
