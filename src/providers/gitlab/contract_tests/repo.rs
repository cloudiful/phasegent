#![allow(unused_imports)]
use super::support::*;
use crate::providers::config::GitlabConfig;
use crate::providers::gitlab::GitlabProvider;
use crate::providers::{CiProvider, IssueProvider, ProviderDispatcher, RepoProvider};

#[test]
fn list_projects_returns_not_supported_error() {
    let error = zero_request(|provider| provider.list_projects());
    let error = error.unwrap_err();
    assert_eq!(error.json()["kind"], "not_supported");
    assert_eq!(error.json()["operation"], "project list");
}

#[test]
fn repo_create_posts_to_projects_with_namespace_id_and_private_visibility() {
    let (base, requests, server) = sequence(vec![
        // GitLabProvider::create_repo first fetches /user so it can
        // resolve the personal namespace id when the operator did not
        // supply an explicit namespace.
        MockResponse::ok(user_payload(7)),
        MockResponse::ok(project_payload(99, "widgets", "owner", "private")),
    ]);
    let provider = provider(base);
    let summary = provider
        .create_repo("widgets", true, "phase3", true)
        .unwrap();
    assert_eq!(summary.full_name, "owner/widgets");
    assert_eq!(summary.owner, "owner");
    assert_eq!(summary.name, "widgets");
    assert!(summary.private);
    assert_eq!(
        summary.html_url.as_deref(),
        Some("https://gitlab.example/owner/widgets"),
    );
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0].starts_with("GET /api/v4/user"),
        "first request must resolve the personal namespace: {}",
        requests[0],
    );
    assert_request(
        &requests[1],
        "POST",
        "/api/v4/projects",
        Some("\"name\":\"widgets\""),
    );
    assert!(
        requests[1].contains("\"visibility\":\"private\""),
        "private-only policy must serialise as visibility=private: {}",
        requests[1],
    );
    assert!(
        requests[1].contains("\"namespace_id\":7"),
        "personal namespace id must be forwarded when the target carries no OWNER prefix: {}",
        requests[1],
    );
    assert!(requests[1].contains("\"initialize_with_readme\":true"));
    assert!(requests[1].contains("\"description\":\"phase3\""));
    server.join().unwrap();
}

#[test]
fn repo_create_rejects_public_repository_without_request() {
    let provider = zero_request_provider();
    let error = provider
        .create_repo("acme/widgets", false, "", false)
        .unwrap_err();
    let rendered = error.json();
    assert_eq!(rendered["kind"], "config");
    assert!(
        rendered["message"]
            .as_str()
            .unwrap_or_default()
            .contains("private"),
        "{rendered}",
    );
}

#[test]
fn repo_create_bare_target_lands_in_personal_namespace() {
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(user_payload(11)),
        MockResponse::ok(project_payload(101, "fresh", "owner", "private")),
    ]);
    let provider = provider(base);
    let summary = provider.create_repo("fresh", true, "", false).unwrap();
    assert_eq!(summary.full_name, "owner/fresh");
    let requests = requests.recv().unwrap();
    let body = &requests[1];
    assert!(body.starts_with("POST /api/v4/projects"));
    assert!(body.contains("\"name\":\"fresh\""));
    assert!(
        body.contains("\"namespace_id\":11"),
        "bare target without OWNER prefix must use the personal namespace id: {body}",
    );
    assert!(body.contains("\"visibility\":\"private\""));
    // When `--auto-init` is not supplied, the field is intentionally
    // omitted so a "don't touch my repo" caller gets a clean payload.
    assert!(
        !body.contains("initialize_with_readme"),
        "auto_init=false must omit initialize_with_readme so the server keeps its default: {body}",
    );
    server.join().unwrap();
}

#[test]
fn repo_create_owner_target_resolves_namespace_via_api() {
    // When the caller passes OWNER/REPO with no explicit namespace id,
    // the provider must resolve OWNER to a numeric namespace id via
    // `GET /namespaces?search=OWNER` and POST that id; it must never
    // silently fall back to the authenticated user's personal namespace.
    let (base, requests, server) = sequence(vec![
        // 1. resolve the personal user id.
        MockResponse::ok(user_payload(7)),
        // 2. search for the OWNER namespace; GitLab returns a single
        // group match.
        MockResponse::ok(
            r#"[{"id":42,"path":"acme","full_path":"acme","kind":"group","name":"Acme"}]"#,
        )
        .with_header("x-next-page", ""),
        // 3. POST /projects with the resolved namespace_id.
        MockResponse::ok(project_payload(33, "widgets", "acme", "private")),
    ]);
    let provider = provider(base);
    let summary = provider
        .create_repo("acme/widgets", true, "", false)
        .unwrap();
    assert_eq!(summary.full_name, "acme/widgets");
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].starts_with("GET /api/v4/user"));
    let lookup = &requests[1];
    assert!(
        lookup.starts_with("GET /api/v4/namespaces?"),
        "OWNER must be resolved via /namespaces: {lookup}",
    );
    assert!(lookup.contains("search=acme"));
    let body = &requests[2];
    assert!(body.starts_with("POST /api/v4/projects"), "{body}",);
    assert!(
        body.contains("\"namespace_id\":42"),
        "resolved namespace id must be forwarded: {body}",
    );
    assert!(body.contains("\"path\":\"widgets\""));
    assert!(body.contains("\"visibility\":\"private\""));
    server.join().unwrap();
}

#[test]
fn repo_create_owner_target_without_namespace_id_errors_when_owner_missing() {
    // If /namespaces?search=OWNER returns no exact match, the
    // provider must surface a structured config error before POST
    // /projects so the operator can correct the OWNER.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(user_payload(7)),
        MockResponse::ok("[]").with_header("x-next-page", ""),
    ]);
    let provider = provider(base);
    let error = provider
        .create_repo("missing/widgets", true, "", false)
        .unwrap_err();
    let rendered = error.json();
    assert_eq!(rendered["kind"], "config");
    assert!(
        rendered["message"]
            .as_str()
            .unwrap_or_default()
            .contains("missing"),
        "{rendered}",
    );
    // The personal user resolution must still happen so a future
    // retry with a different OWNER doesn't re-resolve the namespace.
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /api/v4/user"));
    assert!(requests[1].starts_with("GET /api/v4/namespaces?"));
    server.join().unwrap();
}

#[test]
fn repo_create_owner_target_errors_when_namespace_is_ambiguous() {
    // If /namespaces returns multiple exact matches for the OWNER
    // (for example a user namespace and a group namespace that share
    // a path) the provider must surface a structured config error
    // instead of picking one arbitrarily.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(user_payload(7)),
        MockResponse::ok(
            r#"[
                {"id":11,"path":"acme","full_path":"acme","kind":"group","name":"Acme Group"},
                {"id":12,"path":"acme","full_path":"acme","kind":"user","name":"Acme User"}
            ]"#,
        )
        .with_header("x-next-page", ""),
    ]);
    let provider = provider(base);
    let error = provider
        .create_repo("acme/widgets", true, "", false)
        .unwrap_err();
    let rendered = error.json();
    assert_eq!(rendered["kind"], "config");
    assert!(
        rendered["message"]
            .as_str()
            .unwrap_or_default()
            .contains("ambiguous"),
        "{rendered}",
    );
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /api/v4/user"));
    assert!(requests[1].starts_with("GET /api/v4/namespaces?"));
    server.join().unwrap();
}

#[test]
fn repo_create_owner_target_resolves_user_namespace_when_no_group_match() {
    // A bare user namespace (kind=user) with no group sharing the
    // path must still resolve to the user id so cross-account
    // OWNER/REPO works without an explicit --namespace.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(user_payload(7)),
        MockResponse::ok(
            r#"[{"id":99,"path":"someone","full_path":"someone","kind":"user","name":"Someone"}]"#,
        )
        .with_header("x-next-page", ""),
        MockResponse::ok(project_payload(33, "widgets", "someone", "private")),
    ]);
    let provider = provider(base);
    let summary = provider
        .create_repo("someone/widgets", true, "", false)
        .unwrap();
    assert_eq!(summary.full_name, "someone/widgets");
    let requests = requests.recv().unwrap();
    let body = &requests[2];
    assert!(
        body.contains("\"namespace_id\":99"),
        "user namespace id must be forwarded when no group match exists: {body}",
    );
    server.join().unwrap();
}

#[test]
fn repo_create_bare_target_does_not_call_namespaces_endpoint() {
    // A bare REPOSITORY target must skip the OWNER lookup so the
    // personal namespace is used without an extra round trip.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(user_payload(11)),
        MockResponse::ok(project_payload(101, "fresh", "owner", "private")),
    ]);
    let provider = provider(base);
    let _ = provider.create_repo("fresh", true, "", false).unwrap();
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        !requests[0].contains("/namespaces"),
        "bare target must not call /namespaces: {}",
        requests[0],
    );
    server.join().unwrap();
}

#[test]
fn repo_create_handles_403_forbidden_as_structured_error() {
    // The first authenticated call is always GET /user. For a bare
    // target the call chain is /user -> POST /projects; for an
    // OWNER/REPO target /namespaces?search=OWNER sits in between.
    // This test exercises the bare-target path so the 403 surfaces
    // before any other request is sent.
    let (result, request) = one(
        MockResponse::status(403, r#"{"message":"insufficient scope"}"#),
        |provider| provider.create_repo("widgets", true, "desc", false),
    );
    assert!(request.starts_with("GET /api/v4/user"));
    let error = result.unwrap_err();
    let rendered = error.json();
    assert_eq!(rendered["kind"], "http");
    assert_eq!(rendered["status"], 403);
    assert_eq!(rendered["operation"], "repo create");
}
