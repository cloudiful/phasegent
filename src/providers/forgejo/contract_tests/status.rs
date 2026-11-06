use super::super::status::{
    CLOSED, CLOSED_ID, NATIVE_POLICY_SOURCE, OPEN, OPEN_ID, canonical_target, native_states,
    select_native_target,
};
use super::super::{ForgejoProvider, config::ForgejoConfig};
use super::support::server;

const REPO: &str = r#"{"id":17,"name":"repo","full_name":"owner/repo"}"#;

#[test]
fn native_catalogue_is_the_two_states_without_a_server_read() {
    let states: Vec<_> = native_states()
        .into_iter()
        .map(|status| (status.id, status.name, status.is_closed))
        .collect();
    assert_eq!(
        states,
        vec![
            (OPEN_ID, OPEN.to_owned(), false),
            (CLOSED_ID, CLOSED.to_owned(), true)
        ]
    );
}

#[test]
fn native_targets_accept_only_open_and_closed() {
    assert_eq!(canonical_target("Open").unwrap(), OPEN);
    assert_eq!(canonical_target(" open ").unwrap(), OPEN);
    assert_eq!(canonical_target("CLOSED").unwrap(), CLOSED);
    assert_eq!(select_native_target("0").unwrap(), CLOSED);
    assert_eq!(select_native_target("1").unwrap(), OPEN);
    assert_eq!(select_native_target("Closed").unwrap(), CLOSED);
    for invalid in ["In Progress", "Resolved", "New", "2", "", "  "] {
        let error = canonical_target(invalid).unwrap_err().to_string();
        assert!(
            error.contains(OPEN) && error.contains(CLOSED),
            "{invalid}: {error}"
        );
    }
    assert!(select_native_target("2").is_err());
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
fn status_next_reads_the_native_state_without_reading_labels() {
    // Duplicate and structurally odd label arrays are ordinary metadata the
    // native contract never parses: only `state` decides the answer.
    const ISSUE: &str = r#"{"id":501,"number":7,"title":"example","state":"open","labels":[{"id":4,"name":"status/Resolved"},{"id":4,"name":"status/Resolved"},{"id":"bad","name":null},42]}"#;
    let (base, server) = server::sequence(vec![REPO.into(), ISSUE.into()]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    let next = provider.status_next(7).unwrap();
    assert_eq!(next.current.name, "Open");
    assert_eq!(next.current.id, Some(OPEN_ID));
    assert_eq!(next.current.is_closed, Some(false));
    assert_eq!(next.allowed_next.len(), 1);
    assert_eq!(next.allowed_next[0].name, "Closed");
    assert_eq!(next.allowed_next[0].id, Some(CLOSED_ID));
    assert!(next.allowed_next_missing_on_server.is_empty());
    assert_eq!(next.policy_source, NATIVE_POLICY_SOURCE);
    assert!(!next.advisory);
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(!requests.iter().any(|request| request.contains("/labels")));
}

#[test]
fn status_next_of_a_closed_issue_offers_the_reopen_state() {
    const ISSUE: &str = r#"{"id":501,"number":7,"title":"example","state":"closed"}"#;
    let (base, server) = server::sequence(vec![REPO.into(), ISSUE.into()]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    let next = provider.status_next(7).unwrap();
    assert_eq!(next.current.name, "Closed");
    assert_eq!(next.current.id, Some(CLOSED_ID));
    assert_eq!(next.current.is_closed, Some(true));
    assert_eq!(next.allowed_next.len(), 1);
    assert_eq!(next.allowed_next[0].name, "Open");
    assert_eq!(next.allowed_next[0].id, Some(OPEN_ID));
    server.join().unwrap();
}

#[test]
fn status_transition_patches_the_native_state_and_never_touches_labels() {
    const BEFORE: &str = r#"{"id":501,"number":7,"title":"example","state":"open","labels":[{"id":42,"name":"bug"}]}"#;
    const PATCHED: &str = r#"{"id":501,"number":7,"title":"example","state":"closed","labels":[{"id":42,"name":"bug"}]}"#;
    let (base, server) = server::sequence(vec![
        REPO.into(),
        BEFORE.into(),
        "[]".into(),
        BEFORE.into(),
        PATCHED.into(),
    ]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    assert_eq!(
        provider.transition_status(7, "Closed").unwrap().state,
        "closed"
    );
    let requests = server.join().unwrap();
    assert!(requests[2].contains("/issues/7/dependencies?"));
    assert!(requests[4].starts_with("PATCH /api/v1/repos/owner/repo/issues/7 "));
    assert!(requests[4].ends_with(r#"{"state":"closed"}"#));
    assert!(
        !requests.iter().any(|request| request.starts_with("PUT ")),
        "the native contract never writes labels: {requests:?}"
    );
    assert!(
        !requests.iter().any(|request| request.contains("/labels")),
        "the native contract never reads labels: {requests:?}"
    );
}

#[test]
fn reopening_patches_the_native_state_without_a_dependency_gate() {
    const BEFORE: &str = r#"{"id":501,"number":7,"title":"example","state":"closed"}"#;
    const PATCHED: &str = r#"{"id":501,"number":7,"title":"example","state":"open"}"#;
    let (base, server) = server::sequence(vec![
        REPO.into(),
        BEFORE.into(),
        BEFORE.into(),
        PATCHED.into(),
    ]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    assert_eq!(provider.transition_status(7, "Open").unwrap().state, "open");
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(
        !requests
            .iter()
            .any(|request| request.contains("/dependencies")),
        "reopening has no dependency gate: {requests:?}"
    );
    assert!(requests[3].ends_with(r#"{"state":"open"}"#));
}

#[test]
fn the_same_native_state_is_a_read_only_no_op() {
    const ISSUE: &str = r#"{"id":501,"number":7,"title":"example","state":"open"}"#;
    let (base, server) = server::sequence(vec![REPO.into(), ISSUE.into()]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    assert_eq!(provider.transition_status(7, "Open").unwrap().state, "open");
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| request.starts_with("GET ")));
}

#[test]
fn an_unsupported_target_refuses_before_any_request() {
    let (base, server) = server::sequence(vec![]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    let error = provider.transition_status(7, "In Progress").unwrap_err();
    assert!(error.to_string().contains("Open"));
    assert!(server.join().unwrap().is_empty());
}

#[test]
fn a_state_race_and_an_unconfirmed_native_write_refuse() {
    const BEFORE: &str = r#"{"id":501,"number":7,"title":"example","state":"open"}"#;
    const CHANGED: &str = r#"{"id":501,"number":7,"title":"example","state":"closed"}"#;
    // The issue changed state during the preflight: no native write is sent.
    let (base, server) = server::sequence(vec![
        REPO.into(),
        BEFORE.into(),
        "[]".into(),
        CHANGED.into(),
    ]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    assert!(provider.transition_status(7, "Closed").is_err());
    let requests = server.join().unwrap();
    assert!(requests.iter().all(|request| request.starts_with("GET ")));

    // The native write did not confirm the target: the caller is told to
    // inspect the issue instead of retrying blindly.
    let (base, server) = server::sequence(vec![
        REPO.into(),
        BEFORE.into(),
        "[]".into(),
        BEFORE.into(),
        BEFORE.into(),
    ]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    let error = provider
        .transition_status(7, "Closed")
        .unwrap_err()
        .to_string();
    assert!(error.contains("partial transition"), "{error}");
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 5);
    assert!(!requests.iter().any(|request| request.starts_with("PUT ")));
}
