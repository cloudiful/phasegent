//! Local provider contract tests (SQLite).

pub(crate) mod cross_backend;

use super::model::{local_sql, state_for_status};
use super::{LocalProvider, PgLocalProvider};
use crate::providers::api::IssueSearchOptions;
use std::path::PathBuf;

fn tmp_provider(label: &str) -> (LocalProvider, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "phasegent-local-p2-{label}-{}-{}",
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

fn search_all(state: &str) -> IssueSearchOptions {
    IssueSearchOptions {
        query: None,
        state: state.to_owned(),
        page: 1,
        limit: 50,
        include_body: false,
        all: true,
    }
}

#[test]
fn crud_round_trip_keeps_redmine_envelope() {
    let (provider, dir) = tmp_provider("crud");
    let created = provider.create_issue("Hello", "World").unwrap();
    assert!(created.id > 0);
    assert_eq!(created.number, created.id);
    assert_eq!(created.title, "Hello");
    assert_eq!(created.body, "World");
    assert_eq!(created.state, "open");
    assert!(created.html_url.as_deref().unwrap().ends_with(&format!("/issues/{}", created.id)));

    let fetched = provider.get_issue(created.number).unwrap();
    assert_eq!(fetched.title, "Hello");

    let updated = provider.update_body(created.number, "New body").unwrap();
    assert_eq!(updated.body, "New body");

    assert!(provider.get_issue(999_999).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn search_filters_state_and_paginates() {
    let (provider, dir) = tmp_provider("search");
    for title in ["alpha one", "alpha two", "beta one"] {
        provider.create_issue(title, "body").unwrap();
    }
    // Move one issue to Resolved then Closed so `closed` is non-empty.
    let first = provider
        .search_issues(&IssueSearchOptions {
            query: Some("alpha one".to_owned()),
            state: "all".to_owned(),
            page: 1,
            limit: 10,
            include_body: false,
            all: false,
        })
        .unwrap();
    assert_eq!(first.items.len(), 1);
    let target = first.items[0].number;
    provider.with_conn("test", |conn| {
        conn.execute(
            "UPDATE local_issues SET status='Resolved', updated_at=2 WHERE id=?1",
            rusqlite::params![target as i64],
        )?;
        Ok(())
    }).unwrap();
    provider.close_issue(target).unwrap();

    let open = provider.search_issues(&search_all("open")).unwrap();
    assert!(open.items.iter().all(|item| item.state == "open"));
    assert!(open.total_count.is_some());

    let closed = provider.search_issues(&search_all("closed")).unwrap();
    assert_eq!(closed.items.len(), 1);
    assert_eq!(closed.items[0].number, target);

    let all = provider.search_issues(&search_all("all")).unwrap();
    assert_eq!(all.items.len(), 3);

    let page = provider
        .search_issue_page(&IssueSearchOptions {
            query: None,
            state: "all".to_owned(),
            page: 2,
            limit: 2,
            include_body: false,
            all: true,
        })
        .unwrap();
    assert_eq!(page.page, 2);
    assert_eq!(page.items.len(), 1);
    assert!(!page.has_more);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn close_validates_transition_and_writes_closed_at() {
    let (provider, dir) = tmp_provider("close");
    let created = provider.create_issue("Close me", "body").unwrap();
    // New -> Closed has no seeded edge, so close must fail before any write.
    let rejected = provider.close_issue(created.number).unwrap_err();
    assert!(rejected.to_string().contains("not allowed"));

    provider.with_conn("test", |conn| {
        conn.execute(
            "UPDATE local_issues SET status='Resolved', updated_at=2 WHERE id=?1",
            rusqlite::params![created.number as i64],
        )?;
        Ok(())
    }).unwrap();
    let closed = provider.close_issue(created.number).unwrap();
    assert_eq!(closed.state, "closed");
    let closed_at: Option<i64> = provider.with_conn("test", |conn| {
        conn.query_row(
            "SELECT closed_at FROM local_issues WHERE id=?1",
            rusqlite::params![created.number as i64],
            |row| row.get(0),
        )
    }).unwrap();
    assert!(closed_at.unwrap_or(0) > 0);
    let again = provider.close_issue(created.number).unwrap();
    assert_eq!(again.state, "closed");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn comment_marker_conflict_is_friendly() {
    let (provider, dir) = tmp_provider("comment");
    let issue = provider.create_issue("C", "b").unwrap();
    let marker = "<!-- unit-marker -->";
    let created = provider
        .create_comment(issue.number, "<!-- unit-marker --> hi", marker)
        .unwrap();
    assert_eq!(created.marker.as_deref(), Some(marker));
    assert!(created.body.is_none());
    assert!(created.html_url.as_deref().unwrap().contains("#note-"));

    let duplicate = provider
        .create_comment(issue.number, "other", marker)
        .unwrap_err();
    assert!(duplicate.to_string().contains("already exists"));

    let fetched = provider.get_comment(issue.number, created.id).unwrap();
    assert!(fetched.body.is_some());
    assert_eq!(fetched.marker.as_deref(), Some(marker));

    let found = provider.find_marker(issue.number, marker).unwrap();
    assert_eq!(found.id, created.id);
    assert!(provider.find_marker(issue.number, "missing").is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn marker_is_globally_unique_across_issues() {
    // The schema makes local_comments.marker globally UNIQUE
    // (schema.sql), so the same marker cannot be reused on a second
    // issue. Forgejo/Redmine/GitLab allow per-issue reuse; local
    // intentionally diverges and this test pins that behaviour so any
    // future schema change is deliberate.
    let (provider, dir) = tmp_provider("marker-global");
    let first = provider.create_issue("A", "b").unwrap();
    let second = provider.create_issue("B", "b").unwrap();
    let marker = "<!-- global-marker -->";
    provider
        .create_comment(first.number, "hi", marker)
        .unwrap();
    let reused = provider
        .create_comment(second.number, "hi", marker)
        .unwrap_err();
    assert!(reused.to_string().contains("already exists"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn metadata_projects_statuses_versions() {
    let (provider, dir) = tmp_provider("meta");
    let projects = provider.list_projects().unwrap();
    assert!(projects.iter().any(|p| p.identifier == "default"));

    let created = provider.create_project("Team", "team", Some("desc")).unwrap();
    assert_eq!(created.identifier, "team");
    assert!(provider.create_project("Team", "team", None).is_err());

    let statuses = provider.list_issue_statuses().unwrap();
    assert_eq!(statuses.len(), 8);
    assert!(statuses.iter().any(|s| s.name == "Closed" && s.is_closed));

    let versions = provider.list_versions().unwrap();
    assert!(versions.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn queries_sql_is_single_source_and_state_mapping() {
    assert!(local_sql("get_issue_by_id").contains("FROM local_issues"));
    assert!(local_sql("search_issues").contains("LIMIT"));
    assert_eq!(state_for_status("Closed"), "closed");
    assert_eq!(state_for_status("New"), "open");
    let reserved = PgLocalProvider::open("postgres://example/db").unwrap_err();
    assert!(reserved.to_string().contains("postgres backend"));
}

#[test]
fn set_issue_status_writes_status_and_stamps_closed_at() {
    let (provider, dir) = tmp_provider("set-status");
    let issue = provider.create_issue("Status", "body").unwrap();
    // Closed target (id 7) stamps closed_at and flips the envelope state.
    let closed = provider.set_issue_status(issue.number, 7).unwrap();
    assert_eq!(closed.state, "closed");
    let closed_at: Option<i64> = provider.with_conn("test", |conn| {
        conn.query_row(
            "SELECT closed_at FROM local_issues WHERE id=?1",
            rusqlite::params![issue.number as i64],
            |row| row.get(0),
        )
    }).unwrap();
    assert!(closed_at.unwrap_or(0) > 0, "closed target must stamp closed_at");
    // Open target (id 6) clears closed_at and returns the open state.
    let reopened = provider.set_issue_status(issue.number, 6).unwrap();
    assert_eq!(reopened.state, "open");
    let cleared: Option<i64> = provider.with_conn("test", |conn| {
        conn.query_row(
            "SELECT closed_at FROM local_issues WHERE id=?1",
            rusqlite::params![issue.number as i64],
            |row| row.get(0),
        )
    }).unwrap();
    assert!(cleared.is_none(), "open target must clear closed_at");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn set_issue_status_rejects_unknown_id() {
    let (provider, dir) = tmp_provider("set-status-unknown");
    let issue = provider.create_issue("Reject", "body").unwrap();
    let err = provider.set_issue_status(issue.number, 99_999).unwrap_err();
    assert!(
        err.to_string().contains("local status id 99999 was not found"),
        "got: {err}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn advance_issue_status_respects_policy_with_advisory_and_noop_branches() {
    let (provider, dir) = tmp_provider("advance-policy");
    let issue = provider.create_issue("Advance", "body").unwrap();
    // Allowed canonical edge: New -> In Progress.
    let allowed = provider
        .advance_issue_status(issue.number, "In Progress")
        .unwrap();
    assert!(allowed.changed);
    assert!(!allowed.advisory);
    assert_eq!(allowed.from.name, "New");
    assert_eq!(allowed.to.name, "In Progress");
    // No-op: re-applying the current (canonical) status.
    let noop = provider
        .advance_issue_status(issue.number, "In Progress")
        .unwrap();
    assert!(!noop.changed);
    assert!(!noop.advisory);
    assert!(noop.caveat.is_none());
    // Advisory: current status is unknown/custom, so policy defers to the server.
    provider.with_conn("test", |conn| {
        conn.execute(
            "UPDATE local_issues SET status='Custom', updated_at=2 WHERE id=?1",
            rusqlite::params![issue.number as i64],
        )?;
        Ok(())
    }).unwrap();
    let advisory = provider
        .advance_issue_status(issue.number, "In Progress")
        .unwrap();
    assert!(advisory.changed);
    assert!(advisory.advisory);
    assert!(advisory.caveat.is_some());
    let _ = std::fs::remove_dir_all(dir);
}
