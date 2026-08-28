#![allow(unused_imports)]
use super::support::*;
use crate::providers::config::GitlabConfig;
use crate::providers::gitlab::GitlabProvider;
use crate::providers::{CiProvider, IssueProvider, ProviderDispatcher, RepoProvider};

#[test]
fn tracker_label_creates_type_bug_label_when_missing() {
    let (base, requests, server) = sequence(vec![
        // Label list returns empty so the provider must create it.
        MockResponse::ok("[]").with_header("x-next-page", ""),
        MockResponse::ok(label_payload(99, "type::bug")),
    ]);
    let provider = provider(base);
    let label = provider.tracker_label("Bug").unwrap();
    assert_eq!(label, "type::bug");
    let requests = requests.recv().unwrap();
    assert!(requests[1].starts_with("POST /api/v4/projects/42/labels"));
    assert!(requests[1].contains(r#""name":"type::bug""#));
    server.join().unwrap();
}

#[test]
fn tracker_label_creates_type_feature_label_when_missing() {
    let (base, requests, server) = sequence(vec![
        MockResponse::ok("[]").with_header("x-next-page", ""),
        MockResponse::ok(label_payload(100, "type::feature")),
    ]);
    let provider = provider(base);
    let label = provider.tracker_label("feature").unwrap();
    assert_eq!(label, "type::feature");
    let requests = requests.recv().unwrap();
    assert!(requests[1].contains(r#""name":"type::feature""#));
    server.join().unwrap();
}

#[test]
fn tracker_label_skips_creation_when_label_already_exists() {
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(format!("[{}]", label_payload(7, "type::bug")))
            .with_header("x-next-page", ""),
    ]);
    let provider = provider(base);
    let label = provider.tracker_label("Bug").unwrap();
    assert_eq!(label, "type::bug");
    // Only one HTTP call: the GET that found the existing label.
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].starts_with("GET /api/v4/projects/42/labels?"));
    server.join().unwrap();
}

#[test]
fn tracker_label_rejects_unknown_tracker_name() {
    let error = zero_request(|provider| provider.tracker_label("Task"));
    let error = error.unwrap_err();
    assert_eq!(error.json()["kind"], "config");
}

#[test]
fn workflow_label_resolves_every_canonical_status_via_helper() {
    use crate::providers::gitlab::model::workflow_label_from_status;
    let cases = [
        ("New", "workflow::new"),
        ("InProgress", "workflow::in-progress"),
        ("InReview", "workflow::in-review"),
        ("ChangesRequested", "workflow::changes-requested"),
        ("Blocked", "workflow::blocked"),
        ("Resolved", "workflow::resolved"),
        ("Closed", "workflow::closed"),
        ("Cancelled", "workflow::cancelled"),
    ];
    for (input, expected) in cases {
        assert_eq!(workflow_label_from_status(input).unwrap(), expected);
    }
}

#[test]
fn workflow_label_rejects_unknown_status() {
    use crate::providers::gitlab::model::workflow_label_from_status;
    let error = workflow_label_from_status("Reviewing").unwrap_err();
    assert_eq!(error.json()["kind"], "config");
}

#[test]
fn gitlab_provider_rejects_planning_field_shapes_via_planning_cli() {
    // Redmine planning flags must surface as a structured
    // config error for GitLab, not as a successful write. Phase 4
    // distinguishes per-flag: `--parent-issue` (and every other
    // Redmine-only planning field) is rejected with a config error
    // that names the specific flag.
    use crate::command::PlanningOptions;
    use crate::providers::ProviderDispatcher;
    let dispatcher = ProviderDispatcher::Gitlab(
        GitlabProvider::new(
            GitlabConfig::new("https://gitlab.example/api/v4", 42),
            TEST_TOKEN.to_owned(),
        )
        .unwrap(),
    );
    let planning = PlanningOptions {
        parent_issue: Some("1".to_owned()),
        ..PlanningOptions::default()
    };
    let error =
        crate::providers::redmine::planning::resolve_planning(&dispatcher, &planning).unwrap_err();
    assert_eq!(error.json()["kind"], "config");
    let message = error.json()["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        message.contains("parent-issue"),
        "error must name the rejected flag: {message}",
    );
}

#[test]
fn gitlab_tracker_only_create_succeeds_against_planning_cli() {
    // A tracker-only GitLab invocation must not require any Redmine
    // planning field; the planning CLI should fall through to the
    // provider's create path with a `type::bug` label.
    use crate::command::PlanningOptions;
    use crate::providers::ProviderDispatcher;
    let dispatcher = ProviderDispatcher::Gitlab(
        GitlabProvider::new(
            GitlabConfig::new("https://gitlab.example/api/v4", 42),
            TEST_TOKEN.to_owned(),
        )
        .unwrap(),
    );
    let planning = PlanningOptions::default();
    let resolved =
        crate::providers::redmine::planning::resolve_planning(&dispatcher, &planning).unwrap();
    // Empty planning fields round-trip cleanly.
    assert!(resolved.is_empty());
}
