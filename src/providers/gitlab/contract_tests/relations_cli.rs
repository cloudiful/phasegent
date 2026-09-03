#![allow(unused_imports)]
use super::support::*;
use crate::providers::config::GitlabConfig;
use crate::providers::gitlab::GitlabProvider;
use crate::providers::{IssueProvider, ProviderDispatcher, RepoProvider};

#[test]
fn relation_cli_routes_gitlab_list_create_and_delete_fails_without_source() {
    use crate::command::RelationCommand;
    use crate::providers::ProviderDispatcher;
    use crate::providers::redmine::model::RedmineRelationType;

    let (base, requests, server) = sequence(vec![
        // list
        MockResponse::ok(
            r#"[{"issue_link_id":1,"link_type":"relates_to","issue":{"id":12,"iid":12,"project_id":42}}]"#,
        )
        .with_header("x-next-page", ""),
        // create (Phase 5: only `relates` is supported for create, so
        // the mock returns the matching `relates_to` link type)
        MockResponse::ok(
            r#"{"issue_link_id":2,"link_type":"relates_to","issue":{"id":13,"iid":13,"project_id":42}}"#,
        ),
    ]);
    let dispatcher = ProviderDispatcher::Gitlab(provider(base));
    let listed = crate::providers::redmine::relations::execute(
        &dispatcher,
        &RelationCommand::List { issue: 7 },
    )
    .unwrap();
    match listed {
        crate::providers::redmine::relations::RelationResult::List(relations) => {
            assert_eq!(relations.len(), 1);
            assert_eq!(relations[0].relation_type, "relates");
        }
        other => panic!("expected list result, got {other:?}"),
    }
    // Phase 5: the live instance only accepts `relates` for create;
    // `blocks` is gated with a structured not-supported error. Use
    // `Relates` here so the create path succeeds against the mock.
    let created = crate::providers::redmine::relations::execute(
        &dispatcher,
        &RelationCommand::Create {
            issue: 7,
            to: 13,
            relation_type: RedmineRelationType::Relates,
            delay: None,
        },
    )
    .unwrap();
    match created {
        crate::providers::redmine::relations::RelationResult::Created(summary) => {
            assert_eq!(summary.id, 2);
            assert_eq!(summary.relation_type, "relates");
        }
        other => panic!("expected created result, got {other:?}"),
    }
    // Delete without --issue must fail with a structured config
    // error: GitLab requires the source issue iid in the DELETE URL
    // and the orchestrator CLI surfaces the missing field explicitly
    // rather than silently guessing.
    let error = crate::providers::redmine::relations::execute(
        &dispatcher,
        &RelationCommand::Delete {
            relation_id: 2,
            issue: None,
        },
    )
    .unwrap_err();
    let rendered = error.json();
    assert_eq!(rendered["kind"], "config");
    assert!(
        rendered["message"]
            .as_str()
            .unwrap_or_default()
            .contains("source"),
        "delete error must name the missing source field: {rendered}",
    );
    // List and create consumed two requests; the rejected delete
    // hit zero endpoints.
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /api/v4/projects/42/issues/7/links?"));
    assert!(requests[1].starts_with("POST /api/v4/projects/42/issues/7/links"));
    server.join().unwrap();
}

#[test]
fn relation_cli_routes_gitlab_delete_with_source_issue_iid() {
    // A normal CLI invocation that supplies the source issue iid
    // must reach the network with the correct URL. The dispatcher
    // is no longer allowed to silently fail for default GitLab
    // delete calls once the caller passes the flag.
    use crate::command::RelationCommand;
    use crate::providers::ProviderDispatcher;
    let (base, requests, server) = sequence(vec![MockResponse::status(204, "")]);
    let dispatcher = ProviderDispatcher::Gitlab(provider(base));
    let deleted = crate::providers::redmine::relations::execute(
        &dispatcher,
        &RelationCommand::Delete {
            relation_id: 11,
            issue: Some(7),
        },
    )
    .unwrap();
    match deleted {
        crate::providers::redmine::relations::RelationResult::Deleted(relation_id) => {
            assert_eq!(relation_id, 11);
        }
        other => panic!("expected deleted result, got {other:?}"),
    }
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].starts_with("DELETE /api/v4/projects/42/issues/7/links/11"),
        "delete must use the per-source-issue path: {}",
        requests[0],
    );
    assert!(
        !requests[0].contains("/issues/links/11"),
        "delete must NOT use the broken no-source path: {}",
        requests[0],
    );
    server.join().unwrap();
}

#[test]
fn relation_cli_routes_redmine_delete_with_ignored_source_issue() {
    // Redmine deletes ignore the optional source issue field; the
    // shared enum carries the field only to make the GitLab dispatch
    // explicit, not to alter Redmine behaviour.
    use crate::command::RelationCommand;
    use crate::providers::ProviderDispatcher;
    let dispatcher = ProviderDispatcher::Redmine(
        crate::providers::config::RedmineProvider::new(
            crate::providers::config::RedmineConfig::new(
                "https://redmine.example".to_owned(),
                "42".to_owned(),
                5,
            ),
            "test-redmine-key".to_owned(),
        )
        .unwrap(),
    );
    let error = crate::providers::redmine::relations::execute(
        &dispatcher,
        &RelationCommand::Delete {
            relation_id: 99,
            issue: Some(7),
        },
    )
    .unwrap_err();
    let rendered = error.json();
    // The orchestrator has no real Redmine server, so the request
    // fails with a network-level error rather than a not-supported
    // error. What matters is that the optional `issue` flag did
    // NOT short-circuit the dispatch to a not-supported error.
    assert_ne!(rendered["kind"], "not_supported");
    assert_ne!(rendered["operation"], "issue relations");
}

#[test]
fn relation_cli_gitlab_create_rejects_precedes_as_config_error() {
    use crate::command::RelationCommand;
    use crate::providers::ProviderDispatcher;
    use crate::providers::redmine::model::RedmineRelationType;
    let provider = provider("http://127.0.0.1:1".to_owned());
    let dispatcher = ProviderDispatcher::Gitlab(provider);
    let error = crate::providers::redmine::relations::execute(
        &dispatcher,
        &RelationCommand::Create {
            issue: 7,
            to: 8,
            relation_type: RedmineRelationType::Precedes,
            delay: None,
        },
    )
    .unwrap_err();
    let rendered = error.json();
    assert_eq!(rendered["kind"], "config");
    assert!(
        rendered["message"]
            .as_str()
            .unwrap_or_default()
            .contains("precedes")
    );
}

#[test]
fn relation_cli_gitlab_create_rejects_delay_as_config_error() {
    use crate::command::RelationCommand;
    use crate::providers::ProviderDispatcher;
    use crate::providers::redmine::model::RedmineRelationType;
    let provider = provider("http://127.0.0.1:1".to_owned());
    let dispatcher = ProviderDispatcher::Gitlab(provider);
    let error = crate::providers::redmine::relations::execute(
        &dispatcher,
        &RelationCommand::Create {
            issue: 7,
            to: 8,
            relation_type: RedmineRelationType::Blocks,
            delay: Some(2),
        },
    )
    .unwrap_err();
    let rendered = error.json();
    assert_eq!(rendered["kind"], "config");
    assert!(
        rendered["message"]
            .as_str()
            .unwrap_or_default()
            .contains("delay")
    );
}

#[test]
fn relation_cli_gitlab_create_rejects_blocks_as_not_supported_before_request() {
    // End-to-end CLI dispatch must surface the Phase 5 capability
    // gate: the live instance only accepts `relates` for create,
    // so `relation create --type blocks` against a GitLab
    // provider must fail with a structured `not_supported` error
    // BEFORE any HTTP traffic. The CLI binds to a deliberately
    // unreachable address; a real network call would surface as a
    // `request` kind rather than a `not_supported` kind.
    use crate::command::RelationCommand;
    use crate::providers::ProviderDispatcher;
    use crate::providers::redmine::model::RedmineRelationType;
    let provider = provider("http://127.0.0.1:1".to_owned());
    let dispatcher = ProviderDispatcher::Gitlab(provider);
    let error = crate::providers::redmine::relations::execute(
        &dispatcher,
        &RelationCommand::Create {
            issue: 7,
            to: 8,
            relation_type: RedmineRelationType::Blocks,
            delay: None,
        },
    )
    .unwrap_err();
    let rendered = error.json();
    assert_eq!(
        rendered["kind"], "not_supported",
        "blocks create must surface as not_supported, not request / http: {rendered}",
    );
    assert_eq!(rendered["provider"], "gitlab");
    assert!(
        rendered["message"]
            .as_str()
            .unwrap_or_default()
            .contains("does not support relation create"),
        "error must reference the unsupported direction: {rendered}",
    );
}
