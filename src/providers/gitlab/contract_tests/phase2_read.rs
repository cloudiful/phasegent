//! Phase 2 (issue 257) read-side parity contract tests.
//!
//! These tests pin the new GitLab equivalent reads that Phase 2 wired
//! onto the shared CLI surface:
//!   * `list_issue_statuses` → static `WORKFLOW_LABELS` catalogue
//!     (no HTTP traffic).
//!   * `list_milestones` → `GET /projects/:id/milestones` mapped onto
//!     `RedmineVersion`.
//!   * `list_projects` (additional coverage beyond repo.rs) →
//!     `GET /projects` mapped onto `RedmineProject` with public
//!     visibility surfaced via `is_public`.
//!
//! Every test asserts that the equivalent read is reachable through
//! both the `GitlabProvider` and the `ProviderDispatcher::Gitlab` arm
//! so the CLI/MCP layer can rely on the parity row.

#![allow(unused_imports)]
use super::support::*;
use crate::providers::config::GitlabConfig;
use crate::providers::gitlab::GitlabProvider;
use crate::providers::{IssueProvider, ProviderDispatcher, RedmineMetadataProvider, RepoProvider};
#[test]
fn list_workflow_statuses_returns_static_catalogue_via_provider() {
    let provider = provider("http://127.0.0.1:1".to_owned());
    let statuses = provider.list_issue_statuses().unwrap();
    assert_eq!(statuses.len(), 8);
    assert_eq!(statuses[0].name, "New");
    assert_eq!(statuses[6].name, "Closed");
    assert!(statuses[6].is_closed);
    assert!(!statuses[0].is_closed);
    // Stable ids: 1..=8.
    let ids: Vec<u64> = statuses.iter().map(|status| status.id).collect();
    assert_eq!(ids, vec![1, 2, 3, 4, 5, 6, 7, 8]);
}

#[test]
fn list_workflow_statuses_returns_static_catalogue_via_dispatcher() {
    // The shared CLI command reaches the GitLab arm through
    // `ProviderDispatcher::Gitlab`, not the inherent provider. The
    // dispatcher arm must therefore surface the same catalogue.
    let dispatcher = dispatcher("http://127.0.0.1:1".to_owned());
    let statuses = dispatcher.list_issue_statuses().unwrap();
    assert_eq!(statuses.len(), 8);
    assert_eq!(statuses[2].name, "In Review");
    assert!(!statuses[5].is_closed);
    assert!(statuses[7].is_closed);
}

#[test]
fn list_milestones_hits_project_milestones_and_maps_to_redmine_version() {
    let response = serde_json::json!([
        {
            "id": 12,
            "iid": 1,
            "title": "Sprint 1",
            "description": "first sprint",
            "state": "active",
            "due_date": "2026-09-30",
            "start_date": "2026-09-01",
            "web_url": "https://gitlab.example/group/project/-/milestones/1"
        },
        {
            "id": 14,
            "iid": 2,
            "title": "Backlog",
            "description": null,
            "state": "closed",
            "due_date": null,
            "start_date": null,
            "web_url": null
        }
    ])
    .to_string();
    let (result, request) = one(MockResponse::ok(response), |provider| {
        provider.list_milestones()
    });
    let versions = result.unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].id, 12);
    assert_eq!(versions[0].name, "Sprint 1");
    assert_eq!(versions[0].status, "active");
    assert_eq!(versions[0].due_date.as_deref(), Some("2026-09-30"));
    assert_eq!(versions[1].id, 14);
    assert_eq!(versions[1].name, "Backlog");
    assert_eq!(versions[1].status, "closed");
    assert!(versions[1].due_date.is_none());
    assert_request(&request, "GET", "/api/v4/projects/42/milestones?", None);
}

#[test]
fn list_milestones_via_dispatcher_routes_to_gitlab_arm() {
    // The shared CLI command reaches the GitLab arm through
    // `ProviderDispatcher::Gitlab`. The dispatcher must therefore
    // forward to the real `GitlabProvider::list_milestones` so a
    // `version list` against the GitLab provider returns the
    // milestone-backed versions.
    let (result, request) = one_dispatcher(
        MockResponse::ok(
            serde_json::json!([{
                "id": 7,
                "iid": 1,
                "title": "M1",
                "state": "active",
                "due_date": null,
                "start_date": null,
                "web_url": null,
                "description": null
            }])
            .to_string(),
        ),
        |dispatcher| dispatcher.list_project_versions(),
    );
    let versions = result.unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].id, 7);
    assert_eq!(versions[0].name, "M1");
    assert_eq!(versions[0].status, "active");
    assert_request(&request, "GET", "/api/v4/projects/42/milestones?", None);
}

#[test]
fn list_projects_via_dispatcher_routes_to_gitlab_arm() {
    // The shared CLI command reaches the GitLab arm through
    // `ProviderDispatcher::Gitlab`. The dispatcher must therefore
    // forward to the real `GitlabProvider::list_projects`.
    let (result, request) = one_dispatcher(
        MockResponse::ok(format!(
            "[{}]",
            project_payload(41, "alpha", "acme", "public")
        )),
        |dispatcher| dispatcher.list_projects(),
    );
    let projects = result.unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].id, 41);
    assert_eq!(projects[0].identifier, "alpha");
    // `visibility = public` surfaces as `is_public = Some(true)`.
    assert_eq!(projects[0].is_public, Some(true));
    assert_request(&request, "GET", "/api/v4/projects?", None);
}

#[test]
fn phase_2_capability_rows_report_true_through_dispatcher() {
    // Phase 2 parity matrix (issue 257): the dispatcher surface now
    // reports `true` for `ProjectRead`, `IssueStatusRead`, and
    // `VersionRead` on the GitLab arm because equivalent reads are
    // wired. `ProjectCreate` stays `false` (the equivalent lives on
    // the `repo create` path), and `IssueAttachmentUpload` stays
    // `false` (Phase 1 uniform not-supported row).
    let dispatcher = dispatcher("http://127.0.0.1:1".to_owned());
    assert!(dispatcher.supports(crate::policy::Capability::ProjectRead));
    assert!(dispatcher.supports(crate::policy::Capability::IssueStatusRead));
    assert!(dispatcher.supports(crate::policy::Capability::VersionRead));
    assert!(!dispatcher.supports(crate::policy::Capability::ProjectCreate));
    assert!(!dispatcher.supports(crate::policy::Capability::IssueAttachmentUpload));
}

#[test]
fn phase_2_dto_widening_accepts_extended_issue_payload() {
    // Phase 2 widens `ApiIssue` to decode `milestone`, `due_date`,
    // `weight`, `time_stats`, `assignee(s)`, `created_at`, and
    // `updated_at`. The decoder must accept every documented field
    // without losing the original narrow shape.
    use crate::providers::gitlab::model::ApiIssue;
    let payload = serde_json::json!({
        "id": 7,
        "iid": 4,
        "title": "Title",
        "description": "body-of-4",
        "state": "opened",
        "labels": ["workflow::in-progress"],
        "web_url": "https://gitlab.example/group/project/-/issues/4",
        "milestone": {
            "id": 12,
            "iid": 1,
            "title": "Sprint 1",
            "description": "first sprint",
            "state": "active",
            "due_date": "2026-09-30",
            "start_date": "2026-09-01",
            "web_url": "https://gitlab.example/group/project/-/milestones/1"
        },
        "due_date": "2026-09-30",
        "weight": 3,
        "time_stats": {
            "time_estimate": 1800,
            "total_time_spent": 0,
            "human_time_estimate": "30m",
            "human_total_time_spent": null
        },
        "created_at": "2026-09-01T00:00:00.000Z",
        "updated_at": "2026-09-10T00:00:00.000Z",
        "assignee": null,
        "assignees": [
            {
                "id": 9,
                "username": "alice",
                "name": "Alice"
            }
        ]
    })
    .to_string();
    let issue: ApiIssue = serde_json::from_str(&payload).unwrap();
    assert_eq!(issue.iid, 4);
    assert_eq!(issue.title, "Title");
    assert_eq!(issue.state, "opened");
    // Original narrow shape stays decoded.
    assert_eq!(
        issue.web_url.as_deref(),
        Some("https://gitlab.example/group/project/-/issues/4")
    );
    // Phase 2 widening fields are decoded.
    let milestone = issue.milestone.as_ref().expect("milestone decoded");
    assert_eq!(milestone.id, 12);
    assert_eq!(milestone.title.as_deref(), Some("Sprint 1"));
    assert_eq!(milestone.state.as_deref(), Some("active"));
    assert_eq!(milestone.due_date.as_deref(), Some("2026-09-30"));
    assert_eq!(issue.due_date.as_deref(), Some("2026-09-30"));
    assert_eq!(issue.weight, Some(3));
    let stats = issue.time_stats.as_ref().expect("time_stats decoded");
    assert_eq!(stats.time_estimate, Some(1_800));
    assert_eq!(stats.human_time_estimate.as_deref(), Some("30m"));
    assert_eq!(
        issue.created_at.as_deref(),
        Some("2026-09-01T00:00:00.000Z"),
    );
    assert_eq!(
        issue.updated_at.as_deref(),
        Some("2026-09-10T00:00:00.000Z"),
    );
    let assignees = issue.assignees.as_ref().expect("assignees decoded");
    assert_eq!(assignees.len(), 1);
    assert_eq!(assignees[0].username.as_deref(), Some("alice"));
    assert!(issue.assignee.is_none());
}

#[test]
fn phase_2_dto_widening_accepts_legacy_narrow_issue_payload() {
    // Phase 2 widening is backward-compatible: an old narrow payload
    // (no `milestone`, `due_date`, `weight`, `time_stats`,
    // `created_at`, `updated_at`, `assignee`, `assignees`) must still
    // decode so existing fixtures keep working.
    use crate::providers::gitlab::model::ApiIssue;
    let payload = serde_json::json!({
        "id": 7,
        "iid": 4,
        "title": "Title",
        "description": "body-of-4",
        "state": "opened",
        "labels": [],
        "web_url": "https://gitlab.example/group/project/-/issues/4"
    })
    .to_string();
    let issue: ApiIssue = serde_json::from_str(&payload).unwrap();
    assert_eq!(issue.iid, 4);
    assert!(issue.milestone.is_none());
    assert!(issue.due_date.is_none());
    assert!(issue.weight.is_none());
    assert!(issue.time_stats.is_none());
    assert!(issue.created_at.is_none());
    assert!(issue.updated_at.is_none());
    assert!(issue.assignee.is_none());
    assert!(issue.assignees.is_none());
}
