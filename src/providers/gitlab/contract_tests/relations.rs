#![allow(unused_imports)]
use super::support::*;
use crate::providers::config::GitlabConfig;
use crate::providers::gitlab::GitlabProvider;
use crate::providers::{CiProvider, IssueProvider, ProviderDispatcher, RepoProvider};

#[test]
fn list_issue_links_gets_links_endpoint_and_maps_types() {
    let (base, requests, server) = sequence(vec![MockResponse::ok(
        r#"[
            {"issue_link_id":1,"link_type":"relates_to","issue":{"id":101,"iid":11,"project_id":42}},
            {"issue_link_id":2,"link_type":"blocks","issue":{"id":102,"iid":12,"project_id":42}},
            {"issue_link_id":3,"link_type":"is_blocked_by","issue":{"id":103,"iid":13,"project_id":42}}
        ]"#,
    )
    .with_header("x-next-page", "")]);
    let provider = provider(base);
    let relations = provider.list_issue_links(7).unwrap();
    assert_eq!(relations.len(), 3);
    assert_eq!(relations[0].relation_type, "relates");
    assert_eq!(relations[0].issue_id, 7);
    assert_eq!(relations[0].issue_to_id, 11);
    assert_eq!(relations[0].id, 1);
    assert!(relations[0].delay.is_none());
    // `blocks` keeps the canonical name when the source issue owns
    // the link. `is_blocked_by` maps to the inverse `blocked` so the
    // output reads correctly from the queried issue's perspective.
    assert_eq!(relations[1].relation_type, "blocks");
    assert_eq!(relations[1].issue_to_id, 12);
    assert_eq!(relations[2].relation_type, "blocked");
    assert_eq!(relations[2].issue_to_id, 13);
    let requests = requests.recv().unwrap();
    assert!(requests[0].starts_with("GET /api/v4/projects/42/issues/7/links?"));
    server.join().unwrap();
}

#[test]
fn list_issue_links_rejects_zero_iid_before_request() {
    let result = zero_request(|provider| provider.list_issue_links(0));
    let error = result.unwrap_err();
    assert_eq!(error.json()["kind"], "config");
}

#[test]
fn list_issue_links_decodes_live_get_response_target_issue_shape() {
    // The live `GET /projects/:id/issues/:iid/links` response is a
    // JSON array where every element is the **target issue object**
    // plus the link id (`issue_link_id`) and `link_type` attached
    // at the top level. The decoder must read the link id from
    // `issue_link_id` and the linked iid from the top-level `iid`
    // so the rendered `RelationSummary` is non-empty and points at
    // a real target issue. The `relation list` command was
    // returning zeroed relations before this contract was wired
    // through.
    let (base, requests, server) = sequence(vec![MockResponse::ok(
        r#"[
            {"id":101,"iid":11,"project_id":42,"issue_link_id":1,"link_type":"relates_to","title":"Alpha","state":"opened"},
            {"id":102,"iid":12,"project_id":42,"issue_link_id":2,"link_type":"blocks","title":"Beta","state":"opened"},
            {"id":103,"iid":13,"project_id":42,"issue_link_id":3,"link_type":"is_blocked_by","title":"Gamma","state":"opened"}
        ]"#,
    )
    .with_header("x-next-page", "")]);
    let provider = provider(base);
    let relations = provider.list_issue_links(7).unwrap();
    assert_eq!(relations.len(), 3);
    // Every entry must carry a non-zero link id, a non-zero
    // target iid, and the queried issue as `issue_id`. Without
    // this contract the live server response would silently
    // surface an empty relation.
    for (index, expected) in [1_u64, 2, 3].iter().enumerate() {
        assert_eq!(relations[index].id, *expected, "link id at {index}");
        assert_eq!(relations[index].issue_id, 7, "issue_id at {index}");
        assert!(
            relations[index].issue_to_id > 0,
            "issue_to_id must come from the live GET top-level iid; got zero at {index}",
        );
        assert!(relations[index].delay.is_none());
    }
    // Live GET shape must surface the link type strings exactly
    // as the server returned them; the inverse mapping still
    // converts `is_blocked_by` into the canonical `blocked` name
    // so the output reads correctly from the queried issue's
    // perspective.
    assert_eq!(relations[0].relation_type, "relates");
    assert_eq!(relations[0].issue_to_id, 11);
    assert_eq!(relations[1].relation_type, "blocks");
    assert_eq!(relations[1].issue_to_id, 12);
    assert_eq!(relations[2].relation_type, "blocked");
    assert_eq!(relations[2].issue_to_id, 13);
    let requests = requests.recv().unwrap();
    assert!(requests[0].starts_with("GET /api/v4/projects/42/issues/7/links?"));
    server.join().unwrap();
}

#[test]
fn create_issue_link_posts_query_parameters_with_target_project_id() {
    // The live `https://gitlab.example.com/19.2` instance expects
    // `target_project_id`, `target_issue_iid`, and the optional
    // `link_type` to arrive as URL query parameters; the body is
    // rejected. The provider must build the query string exactly
    // like the live server expects it and never put credentials in
    // the URL.
    let (result, request) = one(
        MockResponse::ok(
            r#"{"issue_link_id":8,"link_type":"relates_to","issue":{"id":13,"iid":13,"project_id":42}}"#,
        ),
        |provider| {
            use crate::providers::redmine::model::RedmineRelationType;
            provider.create_issue_link(7, 13, RedmineRelationType::Relates)
        },
    );
    let summary = result.unwrap();
    assert_eq!(summary.id, 8);
    assert_eq!(summary.relation_type, "relates");
    assert_eq!(summary.issue_id, 7);
    assert_eq!(summary.issue_to_id, 13);
    assert!(summary.delay.is_none());
    assert_request(&request, "POST", "/api/v4/projects/42/issues/7/links", None);
    let query_string = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|path| path.split_once('?'))
        .map(|(_, query)| query.to_owned())
        .unwrap_or_default();
    assert!(
        query_string.contains("target_project_id=42"),
        "missing target_project_id query parameter: {request}",
    );
    assert!(
        query_string.contains("target_issue_iid=13"),
        "missing target_issue_iid query parameter: {request}",
    );
    assert!(
        query_string.contains("link_type=relates_to"),
        "missing link_type=relates_to query parameter: {request}",
    );
    // The body must be empty: the live instance rejects body-shape
    // payloads with HTTP 400, so the helper sends no body at all.
    let header_end = request.find("\r\n\r\n").unwrap_or(request.len());
    let body = request.get(header_end + 4..).unwrap_or("");
    assert!(
        body.trim().is_empty(),
        "create must send no body when the query parameters carry the payload; got: {body:?}",
    );
    // The PRIVATE-TOKEN header must remain present and the token
    // must NEVER appear as a URL parameter. The header check is
    // already enforced by `assert_request`; this second check
    // guards against a future contributor accidentally switching
    // to a query-parameter token leak.
    assert!(
        !query_string.to_ascii_lowercase().contains("private-token"),
        "PRIVATE-TOKEN must not be sent as a URL parameter: {request}",
    );
}

#[test]
fn create_issue_link_decodes_live_post_response_with_source_and_target_issues() {
    // The live POST response shape is `{id, source_issue,
    // target_issue, link_type}` rather than the legacy
    // `{issue_link_id, issue: {...}}` shape. The decoder must
    // resolve the link id from `id` and the linked iid from
    // `target_issue.iid` so the rendered `RelationSummary`
    // matches the live payload.
    let (result, _request) = one(
        MockResponse::ok(
            r#"{"id":42,"link_type":"relates_to","source_issue":{"id":7,"iid":7,"project_id":42},"target_issue":{"id":13,"iid":13,"project_id":42}}"#,
        ),
        |provider| {
            use crate::providers::redmine::model::RedmineRelationType;
            provider.create_issue_link(7, 13, RedmineRelationType::Relates)
        },
    );
    let summary = result.unwrap();
    assert_eq!(summary.id, 42, "link id must come from top-level id");
    assert_eq!(summary.relation_type, "relates");
    assert_eq!(summary.issue_id, 7);
    assert_eq!(
        summary.issue_to_id, 13,
        "linked iid must come from target_issue.iid in the live POST response",
    );
    assert!(summary.delay.is_none());
}

#[test]
fn create_issue_link_rejects_blocks_with_structured_not_supported_before_request() {
    // The live instance rejects `blocks` / `is_blocked_by` for
    // create with `link_type does not have a valid value` even
    // when the request is sent with the documented query
    // parameters. We must therefore gate the create path locally
    // so the unsupported direction fails with a structured
    // `not_supported` error BEFORE any network traffic. The test
    // uses `zero_request` so no HTTP listener is started.
    let result = zero_request(|provider| {
        use crate::providers::redmine::model::RedmineRelationType;
        provider.create_issue_link(7, 12, RedmineRelationType::Blocks)
    });
    let error = result.unwrap_err();
    let rendered = error.json();
    assert_eq!(rendered["kind"], "not_supported");
    assert_eq!(rendered["provider"], "gitlab");
    assert!(
        rendered["message"]
            .as_str()
            .unwrap_or_default()
            .contains("does not support relation create"),
        "not_supported error must mention the unsupported direction: {rendered}",
    );
}

#[test]
fn create_issue_link_rejects_precedes_before_request() {
    // Phase 5 adds a local capability gate on top of the existing
    // CLI-level precedes rejection. At the provider level, a
    // direct `create_issue_link(..., Precedes)` call must fail
    // before any HTTP traffic; the gate returns a structured
    // `not_supported` error because the live instance does not
    // accept `relates_to` as the link type for `precedes`. The
    // CLI dispatch layer in `redmine_relations_cli.rs` keeps its
    // earlier structured `config` error for `precedes`, which is
    // asserted separately by
    // `relation_cli_gitlab_create_rejects_precedes_as_config_error`.
    let result = zero_request(|provider| {
        use crate::providers::redmine::model::RedmineRelationType;
        provider.create_issue_link(7, 8, RedmineRelationType::Precedes)
    });
    let error = result.unwrap_err();
    let rendered = error.json();
    assert_eq!(rendered["kind"], "not_supported");
    assert!(
        rendered["message"]
            .as_str()
            .unwrap_or_default()
            .contains("does not support relation create"),
        "provider must gate precedes with a not_supported error: {rendered}",
    );
}

#[test]
fn create_issue_link_rejects_zero_or_self_target_before_request() {
    let result = zero_request(|provider| {
        use crate::providers::redmine::model::RedmineRelationType;
        provider.create_issue_link(7, 0, RedmineRelationType::Relates)
    });
    assert_eq!(result.unwrap_err().json()["kind"], "config");
    let result = zero_request(|provider| {
        use crate::providers::redmine::model::RedmineRelationType;
        provider.create_issue_link(7, 7, RedmineRelationType::Relates)
    });
    assert_eq!(result.unwrap_err().json()["kind"], "config");
}

#[test]
fn delete_issue_link_uses_delete_with_source_and_target_iids() {
    // GitLab REST v4 requires the source issue iid in the URL
    // because the endpoint is scoped per source issue. The path
    // /projects/:id/issues/links/:link_id (without source iid)
    // does not exist; the contract test asserts the correct shape
    // so a future contributor cannot regress to the broken path.
    let (result, request) = one(MockResponse::status(204, ""), |provider| {
        provider.delete_issue_link(Some(7), 11)
    });
    assert_eq!(result.unwrap(), 11);
    assert_request(
        &request,
        "DELETE",
        "/api/v4/projects/42/issues/7/links/11",
        None,
    );
}

#[test]
fn delete_issue_link_without_source_issue_returns_config_error() {
    // The orchestrator CLI does not yet forward the source issue
    // iid (the parser does not accept it in this allowlist scope).
    // The provider surfaces a structured config error instead of
    // silently guessing the source, so a future caller can wire a
    // --issue flag through the parser.
    let result = zero_request(|provider| provider.delete_issue_link(None, 7));
    let error = result.unwrap_err();
    let rendered = error.json();
    assert_eq!(rendered["kind"], "config");
    assert!(
        rendered["message"]
            .as_str()
            .unwrap_or_default()
            .contains("source"),
        "error must name the missing source field: {rendered}",
    );
}

#[test]
fn delete_issue_link_rejects_zero_source_or_zero_link_id_before_request() {
    let error = zero_request(|provider| provider.delete_issue_link(Some(0), 7)).unwrap_err();
    assert_eq!(error.json()["kind"], "config");
    assert!(
        error.json()["message"]
            .as_str()
            .unwrap_or_default()
            .contains("source")
    );
    let error = zero_request(|provider| provider.delete_issue_link(Some(7), 0)).unwrap_err();
    assert_eq!(error.json()["kind"], "config");
    assert!(
        error.json()["message"]
            .as_str()
            .unwrap_or_default()
            .contains("link")
    );
}
