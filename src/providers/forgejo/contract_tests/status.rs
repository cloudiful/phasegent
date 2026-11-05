use super::super::{ForgejoProvider, config::ForgejoConfig};
use super::super::{
    model::Label,
    status::{current_status, require_labels, statuses, validate_transition},
};
use super::support::server;

fn label(id: u64, name: &str) -> Label {
    Label {
        id,
        name: format!("status/{name}"),
    }
}

#[test]
fn canonical_status_labels_are_read_without_fabricating_missing_ones() {
    let labels = vec![label(1, "New"), label(2, "Resolved")];
    let names: Vec<_> = statuses(&labels)
        .into_iter()
        .map(|s| (s.id, s.name, s.is_closed))
        .collect();
    assert_eq!(
        names,
        vec![
            (1, "New".into(), false),
            (2, "Resolved".into(), false),
            (0, "Closed".into(), true)
        ]
    );
    assert!(require_labels(&labels).is_err());
    let mut complete: Vec<Label> = [
        "New",
        "In Progress",
        "In Review",
        "Resolved",
        "Blocked",
        "Changes Requested",
        "Cancelled",
    ]
    .into_iter()
    .enumerate()
    .map(|(i, name)| label(i as u64 + 1, name))
    .collect();
    assert!(require_labels(&complete).is_ok());
    complete.push(label(9, "New"));
    assert!(require_labels(&complete).is_err());
}

#[test]
fn dependency_preflight_checks_every_page_and_fails_closed() {
    let (base, server) = server::sequence(vec![
        r#"[{"state":"closed"}]"#.into(),
        r#"[{"state":"open"}]"#.into(),
        "[]".into(),
    ]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    assert!(provider.require_closed_dependencies(7).is_err());
    let requests = server.join().unwrap();
    assert!(requests[0].starts_with("GET /api/v1/repos/owner/repo/issues/7/dependencies?"));
    assert!(requests[1].contains("page=2"));
    let (base, server) = server::respond("404 Not Found", "missing");
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    assert!(provider.require_closed_dependencies(7).is_err());
    server.join().unwrap();
}

#[test]
fn status_next_reads_open_resolved_and_reports_missing_labels() {
    let repo = r#"{"id":17,"name":"repo","full_name":"owner/repo"}"#;
    let issue = r#"{"id":501,"number":7,"title":"example","state":"open","labels":[{"id":4,"name":"status/Resolved"}]}"#;
    let (base, server) = server::sequence(vec![repo.into(), issue.into(), "[]".into()]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    let next = provider.status_next(7).unwrap();
    assert_eq!(next.current.name, "Resolved");
    assert_eq!(next.current.is_closed, Some(false));
    assert_eq!(next.allowed_next[0].name, "Closed");
    assert_eq!(next.allowed_next[0].id, Some(0));
    assert_eq!(next.allowed_next_missing_on_server, vec!["In Progress"]);
    assert!(
        server
            .join()
            .unwrap()
            .iter()
            .all(|request| request.starts_with("GET "))
    );
}

#[test]
fn status_transition_checks_races_and_preserves_business_labels() {
    const REPO: &str = r#"{"id":17,"name":"repo","full_name":"owner/repo"}"#;
    const CATALOG: &str = r#"[{"id":1,"name":"status/New"},{"id":2,"name":"status/In Progress"},{"id":3,"name":"status/In Review"},{"id":4,"name":"status/Resolved"},{"id":5,"name":"status/Blocked"},{"id":6,"name":"status/Changes Requested"},{"id":7,"name":"status/Cancelled"}]"#;
    const BEFORE: &str = r#"{"id":501,"number":7,"title":"example","state":"open","labels":[{"id":4,"name":"status/Resolved"},{"id":42,"name":"bug"}]}"#;
    const PATCHED: &str = r#"{"id":501,"number":7,"title":"example","state":"closed","labels":[{"id":4,"name":"status/Resolved"},{"id":42,"name":"bug"}]}"#;
    let (base, server) = server::sequence(vec![
        REPO.into(),
        BEFORE.into(),
        CATALOG.into(),
        "[]".into(),
        "[]".into(),
        BEFORE.into(),
        PATCHED.into(),
        r#"[{"id":42,"name":"bug"}]"#.into(),
    ]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    assert_eq!(
        provider.transition_status(7, "Closed").unwrap().state,
        "closed"
    );
    let requests = server.join().unwrap();
    assert!(requests[4].contains("/dependencies?"));
    assert!(requests[6].starts_with("PATCH /api/v1/repos/owner/repo/issues/7 "));
    assert!(requests[7].starts_with("PUT /api/v1/repos/owner/repo/issues/7/labels "));
    assert!(requests[7].ends_with(r#"{"labels":[42]}"#));

    let changed = BEFORE.replace("\"bug\"", "\"different\"");
    let (base, server) = server::sequence(vec![
        REPO.into(),
        BEFORE.into(),
        CATALOG.into(),
        "[]".into(),
        "[]".into(),
        changed,
    ]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    assert!(provider.transition_status(7, "Closed").is_err());
    assert!(
        server
            .join()
            .unwrap()
            .iter()
            .all(|request| request.starts_with("GET "))
    );

    let changed_after_patch = PATCHED.replace("\"bug\"", "\"changed\"");
    let (base, server) = server::sequence(vec![
        REPO.into(),
        BEFORE.into(),
        CATALOG.into(),
        "[]".into(),
        "[]".into(),
        BEFORE.into(),
        changed_after_patch,
    ]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    assert!(
        provider
            .transition_status(7, "Closed")
            .unwrap_err()
            .to_string()
            .contains("partial transition")
    );
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 7);
    assert!(requests.iter().all(|request| !request.starts_with("PUT ")));

    let (base, server) = server::sequence(vec![
        REPO.into(),
        BEFORE.into(),
        CATALOG.into(),
        "[]".into(),
        "[]".into(),
        BEFORE.into(),
        PATCHED.into(),
        "invalid-json".into(),
    ]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    let error = provider
        .transition_status(7, "Closed")
        .unwrap_err()
        .to_string();
    assert!(error.contains("partial transition"));
    assert_eq!(server.join().unwrap().len(), 8);
}

#[test]
fn canonical_open_and_closed_states_and_transition_graph() {
    assert_eq!(
        current_status("open", &[label(4, "Resolved")]).unwrap(),
        "Resolved"
    );
    assert_eq!(
        current_status("closed", &[label(7, "Cancelled")]).unwrap(),
        "Cancelled"
    );
    assert_eq!(current_status("closed", &[]).unwrap(), "Closed");
    assert!(current_status("open", &[]).is_err());
    assert!(current_status("open", &[label(1, "New"), label(2, "Resolved")]).is_err());
    assert!(current_status("open", &[label(7, "Cancelled")]).is_err());
    assert!(validate_transition("Resolved", "Closed").is_ok());
    assert!(validate_transition("New", "Closed").is_err());
    assert!(validate_transition("New", "New").is_ok());
}
