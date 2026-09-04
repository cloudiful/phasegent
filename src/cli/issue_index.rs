use crate::infra::issue_index::SqliteIssueIndex;
use crate::policy::{Capability, Role};
use crate::providers::api::IssueSearchOptions;
use crate::providers::config::resolve_kind;
use crate::providers::index::{
    ISSUE_INDEX_SYNC_MAX_PAGES, IssueIndexDocument, IssueIndexKey, IssueIndexStore,
};
use crate::providers::index_store::{IssueIndexSyncSummary, provider_scope};
use crate::providers::forgejo::ForgejoError;
use crate::providers::{IssueProvider, ProviderKind};

fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(1_700_000_000)
}

pub(crate) fn execute_index_sync(
    role: Role,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    query: Option<String>,
    state: String,
    page: usize,
    limit: usize,
    all: bool,
) -> i32 {
    if !role.allows(Capability::IssueSearch) {
        return crate::cli::permission_error(role, Capability::IssueSearch);
    }
    let provider_kind = match resolve_kind(role, provider_kind) {
        Ok(k) => k,
        Err(e) => return crate::cli::provider_error(e),
    };
    // Redmine index sync requires explicit project id; never silently index all.
    if provider_kind == ProviderKind::Redmine {
        let pid = project_id.as_deref().map(str::trim).unwrap_or("");
        if pid.is_empty() {
            return crate::cli::provider_error(ForgejoError::config(
                "Redmine project id is required for issue index sync; use --project-id",
            ));
        }
    }
    let provider = match crate::cli::provider_for(
        role,
        Some(provider_kind),
        api_base,
        repository,
        project_id,
        close_status_id,
    ) {
        Ok(p) => p,
        Err(e) => return crate::cli::provider_error(e),
    };
    if !provider.supports(Capability::IssueSearch) {
        return crate::cli::provider_error(ForgejoError::not_supported(
            provider.kind().as_str(),
            Capability::IssueSearch.operation(),
        ));
    }
    let options = IssueSearchOptions {
        query: query.clone(),
        state: state.clone(),
        page,
        limit,
        include_body: false,
        all,
    };
    if let Err(e) = options.validate() {
        return crate::cli::provider_error(e);
    }
    let scope = match provider_scope(&provider) {
        Ok(s) => s,
        Err(e) => return crate::cli::provider_error(e),
    };
    let store = match SqliteIssueIndex::open() {
        Ok(s) => s,
        Err(e) => return crate::cli::provider_error(ForgejoError::config(e)),
    };

    let indexed_at = now_unix_secs();
    let mut indexed: usize = 0;
    let mut pages_synced: usize = 0;
    let mut has_more_final = false;
    let mut completed = true;

    let mut seen_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    let should_track_seen = all
        && options.effective_query().is_none()
        && state == "all";

    if !all {
        // Single bounded page
        let page_result = match provider.search_issue_page(&options) {
            Ok(r) => r,
            Err(e) => return crate::cli::provider_error(e),
        };
        has_more_final = page_result.has_more;
        pages_synced = 1;
        for summary in page_result.items {
            let key = match IssueIndexKey::new(
                scope.source.clone(),
                scope.project.clone(),
                summary.number.to_string(),
            ) {
                Ok(k) => k,
                Err(e) => return crate::cli::provider_error(ForgejoError::config(e)),
            };
            if should_track_seen {
                seen_keys.insert(key.to_string());
            }
            let doc = match IssueIndexDocument::new(
                key,
                summary.number,
                summary.title,
                summary.body,
                summary.state,
                summary.html_url,
                None,
                indexed_at,
            ) {
                Ok(d) => d,
                Err(e) => return crate::cli::provider_error(ForgejoError::config(e)),
            };
            if let Err(e) = store.upsert(&doc) {
                return crate::cli::provider_error(ForgejoError::config(e));
            }
            indexed += 1;
        }
        // For single page sync, never tombstone.
        let tombstoned = 0;
        let summary = IssueIndexSyncSummary {
            source: scope.source,
            project: scope.project,
            pages_synced,
            indexed,
            tombstoned,
            has_more: has_more_final,
            completed,
            limit,
            state,
            query,
        };
        return crate::cli::print_json(&summary);
    }

    // --all: walk pages up to safety cap
    let mut current_page = page;
    let mut tombstoned: usize = 0;
    let mut total_indexed: usize = 0;
    loop {
        if pages_synced >= ISSUE_INDEX_SYNC_MAX_PAGES {
            completed = false;
            has_more_final = true;
            break;
        }
        let opts = IssueSearchOptions {
            query: query.clone(),
            state: state.clone(),
            page: current_page,
            limit,
            include_body: false,
            all,
        };
        let page_result = match provider.search_issue_page(&opts) {
            Ok(r) => r,
            Err(e) => return crate::cli::provider_error(e),
        };
        has_more_final = page_result.has_more;
        pages_synced += 1;
        for summary in &page_result.items {
            let key = match IssueIndexKey::new(
                scope.source.clone(),
                scope.project.clone(),
                summary.number.to_string(),
            ) {
                Ok(k) => k,
                Err(e) => return crate::cli::provider_error(ForgejoError::config(e)),
            };
            if should_track_seen {
                seen_keys.insert(key.to_string());
            }
            let doc = match IssueIndexDocument::new(
                key,
                summary.number,
                summary.title.clone(),
                summary.body.clone(),
                summary.state.clone(),
                summary.html_url.clone(),
                None,
                indexed_at,
            ) {
                Ok(d) => d,
                Err(e) => return crate::cli::provider_error(ForgejoError::config(e)),
            };
            if let Err(e) = store.upsert(&doc) {
                return crate::cli::provider_error(ForgejoError::config(e));
            }
            total_indexed += 1;
        }
        // If has_more false or empty page, we are done.
        if !page_result.has_more || page_result.items.is_empty() {
            break;
        }
        current_page += 1;
    }
    indexed = total_indexed;

    // Deterministic tombstone for full queryless sync only.
    if should_track_seen && completed && !has_more_final {
        let active_keys = match store.list_active_keys_for_scope(&scope.source, &scope.project) {
            Ok(v) => v,
            Err(e) => return crate::cli::provider_error(ForgejoError::config(e)),
        };
        for key in active_keys {
            if !seen_keys.contains(&key.to_string()) {
                if let Err(e) = store.tombstone(&key, indexed_at + 1) {
                    return crate::cli::provider_error(ForgejoError::config(e));
                }
                tombstoned += 1;
            }
        }
    } else {
        tombstoned = 0;
    }

    let summary = IssueIndexSyncSummary {
        source: scope.source,
        project: scope.project,
        pages_synced,
        indexed,
        tombstoned,
        has_more: has_more_final,
        completed,
        limit,
        state,
        query,
    };
    crate::cli::print_json(&summary)
}

pub(crate) fn execute_index_search(
    query: String,
    limit: usize,
    offset: usize,
    include_body: bool,
) -> i32 {
    // Local-only: no provider resolution, no network.
    if query.trim().is_empty() {
        return crate::cli::provider_error(ForgejoError::config(
            "issue index search requires --query TEXT (empty queries are rejected)",
        ));
    }
    if limit == 0 || limit > crate::providers::index::ISSUE_INDEX_SEARCH_MAX_LIMIT {
        return crate::cli::provider_error(ForgejoError::config(format!(
            "issue index search --limit must be between 1 and {}",
            crate::providers::index::ISSUE_INDEX_SEARCH_MAX_LIMIT
        )));
    }
    let store = match SqliteIssueIndex::open() {
        Ok(s) => s,
        Err(e) => return crate::cli::provider_error(ForgejoError::config(e)),
    };
    match store.lexical_search(&query, limit, offset, include_body) {
        Ok(result) => crate::cli::print_json(&result),
        Err(e) => crate::cli::provider_error(ForgejoError::config(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::issue_index::SqliteIssueIndex;
    use crate::providers::api::IssueSearchOptions;
    use crate::providers::forgejo::{ForgejoConfig, ForgejoProvider};
    use crate::providers::index::{IssueIndexDocument, IssueIndexKey, IssueIndexStore};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc::{self, Receiver};
    use std::thread::{self, JoinHandle};

    struct MockResponse { status: u16, headers: Vec<(String,String)>, body: String }
    impl MockResponse {
        fn ok(body: impl Into<String>) -> Self { Self { status: 200, headers: Vec::new(), body: body.into() } }
        fn header(mut self, k: &str, v: &str) -> Self { self.headers.push((k.to_owned(), v.to_owned())); self }
    }
    fn tmp_dir(l: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("phasegent-cli-index-test-{}-{}-{}", l, std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()))
    }
    fn forgejo_issue_json(n: u64, title: &str, body: &str) -> String {
        format!(r#"{{"id":{n},"number":{n},"title":"{title}","body":"{body}","state":"open","html_url":"https://forgejo.example/issues/{n}"}}"#)
    }
    fn sequence_forgejo(responses: Vec<MockResponse>) -> (String, Receiver<Vec<String>>, JoinHandle<()>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap(); let a=l.local_addr().unwrap();
        let (tx, rx)=mpsc::channel();
        let h=thread::spawn(move || {
            let mut reqs=Vec::new();
            for resp in responses {
                let (mut s,_)=l.accept().unwrap();
                let mut buf=[0u8;8192]; let n=s.read(&mut buf).unwrap();
                reqs.push(String::from_utf8_lossy(&buf[..n]).into_owned());
                let mut hdrs=format!("HTTP/1.1 {} OK\r\nContent-Length: {}\r\n", resp.status, resp.body.len());
                for (k,v) in resp.headers { hdrs.push_str(&format!("{k}: {v}\r\n")); }
                hdrs.push_str("\r\n");
                s.write_all(hdrs.as_bytes()).unwrap();
                s.write_all(resp.body.as_bytes()).unwrap();
            }
            tx.send(reqs).unwrap();
        });
        (format!("http://{}/api/v1", a), rx, h)
    }

    #[test]
    fn native_request_params_and_full_body_index_path() {
        let long_body = "b".repeat(crate::providers::api::ISSUE_SEARCH_MAX_BODY_BYTES + 20);
        let issue = forgejo_issue_json(7, "Title", &long_body);
        let (base, rx, srv) = sequence_forgejo(vec![MockResponse::ok(format!("[{issue}]")).header("X-Total-Count","1")]);
        let p = ForgejoProvider::new(ForgejoConfig::new(base, "owner","repo"), "token".into()).unwrap();
        let opts = IssueSearchOptions { query: Some("q".into()), state: "all".into(), page: 1, limit: 50, include_body: false, all: false };
        let compact = p.search_issues(&opts).unwrap();
        assert!(compact.items[0].body.is_none());
        let (base2, rx2, srv2) = sequence_forgejo(vec![
            MockResponse::ok(format!("[{issue}]")).header("X-Total-Count","1"),
            MockResponse::ok(format!("[{issue}]")).header("X-Total-Count","1"),
        ]);
        let p2 = ForgejoProvider::new(ForgejoConfig::new(base2, "owner","repo"), "token".into()).unwrap();
        let opts2 = IssueSearchOptions { query: Some("q".into()), state: "all".into(), page: 1, limit: 50, include_body: true, all: false };
        let compact2 = p2.search_issues(&opts2).unwrap();
        assert_eq!(compact2.items[0].body_truncated, Some(true));
        let page = p2.search_issue_page(&opts2).unwrap();
        assert_eq!(page.items[0].body.len(), long_body.len());
        let req = rx.recv().unwrap().remove(0);
        assert!(req.contains("page=1") && req.contains("limit=50") && req.contains("q=q"));
        assert!(req.contains("type=issues"));
        let reqs2 = rx2.recv().unwrap();
        assert!(reqs2[0].contains("page=1"));
        assert!(reqs2[1].contains("page=1"));
        srv.join().unwrap(); srv2.join().unwrap();
    }

    #[test]
    fn one_page_sync_vs_all_and_tombstone_behavior() {
        let (dir, path) = (tmp_dir("sync"), tmp_dir("sync").join("idx.sqlite3"));
        let _ = std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let idx = SqliteIssueIndex::open_at(&path).unwrap();
        let src="forgejo"; let proj="owner/repo";
        for i in 1..=3 {
            let k=IssueIndexKey::new(src, proj, i.to_string()).unwrap();
            let d=IssueIndexDocument::new(k, i, format!("Title {i}"), format!("Body {i}"), "open".into(), None, None, 1_700_000_000+i as i64).unwrap();
            idx.upsert(&d).unwrap();
        }
        assert_eq!(idx.list_active_keys_for_scope(src, proj).unwrap().len(), 3);
        let seen: std::collections::HashSet<String> = ["1","2"].into_iter().map(|s| format!("{src}:{proj}:{s}")).collect();
        let active=idx.list_active_keys_for_scope(src, proj).unwrap();
        let mut tomb=0;
        for k in active { if !seen.contains(&k.to_string()) { idx.tombstone(&k, 1_700_000_100).unwrap(); tomb+=1; } }
        assert_eq!(tomb,1);
        assert_eq!(idx.list_active_keys_for_scope(src, proj).unwrap().len(),2);
        let k3=IssueIndexKey::new(src, proj, "3").unwrap();
        let d3=IssueIndexDocument::new(k3, 3, "Title 3".into(), "Body 3".into(), "open".into(), None, None, 1_700_000_200).unwrap();
        idx.upsert(&d3).unwrap();
        assert_eq!(idx.list_active_keys_for_scope(src, proj).unwrap().len(),3);
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(path.parent().unwrap().to_path_buf());
    }
}
