use super::*;
use crate::branch_links::{self, IssueKey, StateSnapshot};
use std::collections::HashMap;

fn key(number: u64) -> IssueKey {
    IssueKey::from_number("redmine", "tools/phasegent", number).expect("issue key")
}

#[test]
fn forward_and_reverse_reads_project_cached_and_unknown_states() {
    let connection = open_memory_db();
    let repo = "forge.example.com/owner/repo";
    for (branch, number) in [("feat/628", 628), ("feat/616", 616)] {
        let issue = key(number);
        branch_links::store::link(
            &connection,
            &branch_links::LinkParams {
                repo_key: repo,
                branch,
                issue: &issue,
                issue_number: number,
                source: "manual",
                now: 1_700_000_001,
            },
        )
        .expect("link");
    }

    let mut states = HashMap::new();
    states.insert(
        key(628).to_string(),
        StateSnapshot {
            state: "open".to_owned(),
            source: "index".to_owned(),
            indexed_at: 1_700_000_100,
        },
    );
    let lookup = |issue: &IssueKey| states.get(&issue.to_string()).cloned();

    let forward = branch_links::issues_for_branch(&connection, repo, "feat/628", false, &lookup)
        .expect("forward read");
    assert_eq!(forward.len(), 1);
    let cached = forward[0].state.as_ref().expect("cached state");
    assert_eq!(cached.state, "open");
    assert_eq!(cached.source, "index");
    assert_eq!(cached.indexed_at, 1_700_000_100);

    let unknown = branch_links::issues_for_branch(&connection, repo, "feat/616", false, &lookup)
        .expect("unknown read");
    assert_eq!(unknown.len(), 1);
    assert!(unknown[0].state.is_none(), "missing snapshot is unknown");

    let reverse = branch_links::branches_for_issue(&connection, repo, &key(628), false, &lookup)
        .expect("reverse read");
    assert_eq!(reverse.len(), 1);
    assert_eq!(reverse[0].branch, "feat/628");
    assert_eq!(
        reverse[0].state.as_ref().expect("reverse state").state,
        "open"
    );
}

#[test]
fn issue_key_validation_rejects_bad_identifiers() {
    assert!(IssueKey::new("", "project", "1").is_err());
    assert!(IssueKey::new("redmine", "", "1").is_err());
    assert!(IssueKey::new("redmine", "project", "").is_err());
    assert!(IssueKey::from_number("redmine", "project", 0).is_err());
    assert!(IssueKey::new("red\nmine", "project", "1").is_err());
}

#[test]
fn closing_an_issue_preserves_its_branch_association() {
    let connection = open_memory_db();
    let repo = "forge.example.com/owner/repo";
    let issue = key(628);
    branch_links::store::link(
        &connection,
        &branch_links::LinkParams {
            repo_key: repo,
            branch: "feat/628",
            issue: &issue,
            issue_number: 628,
            source: "manual",
            now: 1_700_000_001,
        },
    )
    .expect("link");

    let closed = |_: &IssueKey| {
        Some(StateSnapshot {
            state: "closed".to_owned(),
            source: "index".to_owned(),
            indexed_at: 1_700_000_050,
        })
    };
    let links = branch_links::issues_for_branch(&connection, repo, "feat/628", false, &closed)
        .expect("read with closed state");
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].state.as_ref().expect("state").state, "closed");

    let reverse = branch_links::branches_for_issue(&connection, repo, &issue, false, &closed)
        .expect("reverse with closed state");
    assert_eq!(reverse.len(), 1);
    assert_eq!(reverse[0].branch, "feat/628");
}

#[test]
fn reverse_read_exposes_detach_history_like_forward() {
    let connection = open_memory_db();
    let repo = "forge.example.com/owner/repo";
    let issue = key(616);
    branch_links::store::link(
        &connection,
        &branch_links::LinkParams {
            repo_key: repo,
            branch: "feat/616",
            issue: &issue,
            issue_number: 616,
            source: "manual",
            now: 1_700_000_001,
        },
    )
    .expect("link");
    branch_links::store::detach(
        &connection,
        repo,
        "feat/616",
        &issue,
        "manual",
        1_700_000_010,
    )
    .expect("detach");

    let active = branch_links::branches_for_issue(
        &connection,
        repo,
        &issue,
        false,
        &branch_links::UnknownState,
    )
    .expect("active reverse read");
    assert!(active.is_empty());

    let history = branch_links::branches_for_issue(
        &connection,
        repo,
        &issue,
        true,
        &branch_links::UnknownState,
    )
    .expect("history reverse read");
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].status, "detached");
    assert_eq!(history[0].detached_at, Some(1_700_000_010));
    assert_eq!(history[0].detached_reason.as_deref(), Some("manual"));

    let forward = branch_links::issues_for_branch(
        &connection,
        repo,
        "feat/616",
        true,
        &branch_links::UnknownState,
    )
    .expect("history forward read");
    assert_eq!(forward.len(), 1);
    assert_eq!(forward[0].detached_at, history[0].detached_at);
    assert_eq!(forward[0].detached_reason, history[0].detached_reason);
}

fn seed_index(dir: &std::path::Path, rows: &[(&str, &str, &str, &str, i64)]) {
    let path = dir.join("phasegent-index.sqlite3");
    let connection = rusqlite::Connection::open(&path).expect("index db must open");
    connection
        .execute_batch(
            "CREATE TABLE issue_documents (
                source TEXT NOT NULL, project TEXT NOT NULL, external_id TEXT NOT NULL,
                issue_number INTEGER NOT NULL, title TEXT NOT NULL, body TEXT NOT NULL,
                state TEXT NOT NULL, url TEXT, provider_updated_at INTEGER,
                indexed_at INTEGER NOT NULL, content_hash TEXT NOT NULL,
                deleted INTEGER NOT NULL DEFAULT 0, deleted_at INTEGER,
                PRIMARY KEY (source, project, external_id)
            );",
        )
        .expect("schema must apply");
    for (source, project, external_id, state, indexed_at) in rows {
        connection
            .execute(
                "INSERT INTO issue_documents \
                 (source, project, external_id, issue_number, title, body, state, indexed_at, content_hash, deleted) \
                 VALUES (?1, ?2, ?3, 628, 't', 'b', ?4, ?5, 'h', 0)",
                rusqlite::params![source, project, external_id, state, indexed_at],
            )
            .expect("row must insert");
    }
}

#[test]
fn snapshot_reads_hit_and_miss_locally() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
    let _lock = lock_workflow_tests();
    let dir = unique_scratch("snapshot");
    seed_index(
        &dir,
        &[("redmine", "tools-phasegent", "628", "open", 1_700_000_100)],
    );
    let _guard = EnvGuard::set(
        "PHASEGENT_INDEX_DB_PATH",
        dir.join("phasegent-index.sqlite3")
            .as_os_str()
            .to_string_lossy()
            .as_ref(),
    );
    let found = branch_links::read_snapshot("redmine", "tools-phasegent", "628")
        .expect("indexed issue must project");
    assert_eq!(found.state, "open");
    assert_eq!(found.indexed_at, 1_700_000_100);
    assert!(
        branch_links::read_snapshot("redmine", "tools-phasegent", "999").is_none(),
        "missing row is unknown"
    );
    assert!(
        branch_links::read_snapshot("", "tools-phasegent", "628").is_none(),
        "empty scope is unknown"
    );
}

#[test]
fn snapshot_stays_unknown_without_a_local_index() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
    let _lock = lock_workflow_tests();
    let dir = unique_scratch("snapshot-missing");
    let _guard = EnvGuard::set(
        "PHASEGENT_INDEX_DB_PATH",
        dir.join("absent.sqlite3")
            .as_os_str()
            .to_string_lossy()
            .as_ref(),
    );
    assert!(
        branch_links::read_snapshot("redmine", "tools-phasegent", "628").is_none(),
        "missing file is unknown, never an error"
    );
    let _pg = EnvGuard::set("PHASEGENT_INDEX_PG_URL", "postgres://example/x");
    assert!(
        branch_links::read_snapshot("redmine", "tools-phasegent", "628").is_none(),
        "postgres backend has no local snapshot to read"
    );
}
