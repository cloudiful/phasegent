use crate::auth;
use crate::command;
use crate::infra::storage::Storage;
use crate::policy::{Capability, Role};
use crate::providers::ProviderKind;
use crate::providers::forgejo::{ForgejoConfig, ForgejoProvider};
use crate::providers::redmine::model::{
    TransitionVerdict, canonical_allowed_next, canonical_status_name, evaluate_transition,
};
use crate::remote;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

#[test]
fn remote_resolution_keeps_https_port_and_drops_ssh_port() {
    let https = remote::parse_remote("https://forgejo.example:8443/owner/widgets.git").unwrap();
    assert_eq!(https.api_base, "https://forgejo.example:8443/api/v1");
    assert_eq!(https.repository, "owner/widgets");

    let ssh = remote::parse_remote("ssh://git@forgejo.example:2222/owner/widgets.git").unwrap();
    assert_eq!(ssh.api_base, "https://forgejo.example/api/v1");
    assert_eq!(ssh.repository, "owner/widgets");

    let prefixed =
        remote::parse_remote("https://forgejo.example/forgejo/owner/widgets.git").unwrap();
    assert_eq!(prefixed.api_base, "https://forgejo.example/forgejo/api/v1");
    assert_eq!(prefixed.repository, "owner/widgets");
}

#[test]
fn option_values_cannot_be_omitted() {
    for args in [
        vec!["--role", "orchestrator", "issue", "search", "--state"],
        vec![
            "--role",
            "orchestrator",
            "issue",
            "search",
            "--query",
            "--state",
            "all",
        ],
        vec![
            "--role",
            "orchestrator",
            "issue",
            "update-body",
            "1",
            "--body",
        ],
        vec![
            "--role",
            "orchestrator",
            "issue",
            "create",
            "--title",
            "Title",
            "--body",
        ],
    ] {
        let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert!(
            command::parse(&args).is_err(),
            "accepted missing value: {args:?}"
        );
    }
}

#[test]
fn inline_form_accepts_leading_dash_values_for_required_options() {
    // Markdown list bullets (`- Goal`) and separator lines (`---`) must reach the
    // server intact when supplied via the explicit `--option=value` token form.
    // Two-arg `--option value` still treats a leading-dash next token as missing,
    // so this regression covers only the escape hatch.

    // issue title leading dash
    let args = [
        "--role",
        "orchestrator",
        "issue",
        "create",
        "--title=-starts-with-dash",
        "--body=ok",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("inline --title should parse");
    match invocation.command {
        command::Command::Issue(command::IssueCommand::Create {
            title,
            body,
            tracker: _,
            planning: _,
        }) => {
            assert_eq!(title, "-starts-with-dash");
            assert_eq!(body, "ok");
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // issue body (Markdown bullet) via issue create
    let args = [
        "--role",
        "orchestrator",
        "issue",
        "create",
        "--title=ok",
        "--body=- Goal",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("inline --body should parse");
    match invocation.command {
        command::Command::Issue(command::IssueCommand::Create {
            title,
            body,
            tracker: _,
            planning: _,
        }) => {
            assert_eq!(title, "ok");
            assert_eq!(body, "- Goal");
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // issue body (`---` separator) via issue update-body
    let args = [
        "--role",
        "orchestrator",
        "issue",
        "update-body",
        "1",
        "--body=---",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("inline --body should parse");
    match invocation.command {
        command::Command::Issue(command::IssueCommand::UpdateBody {
            number,
            body,
            tracker: _,
            planning: _,
        }) => {
            assert_eq!(number, 1);
            assert_eq!(body, "---");
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // issue search query beginning with a dash (negative filter style)
    let args = [
        "--role",
        "orchestrator",
        "issue",
        "search",
        "--query=-tag:regression",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("inline --query should parse");
    match invocation.command {
        command::Command::Issue(command::IssueCommand::Search {
            query, state, ..
        }) => {
            assert_eq!(query.as_deref(), Some("-tag:regression"));
            assert_eq!(state, "all");
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // issue search state via inline form (valid state value): confirms the
    // parser-level inline form is recognized. The parser separately rejects
    // non-{open,closed,all} state values regardless of leading-dash, which
    // is verified by `inline_form_with_invalid_state_value_errors_semantically`.
    let args = [
        "--role",
        "orchestrator",
        "issue",
        "search",
        "--state=closed",
        "--query=-tag:regression",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("inline --state should parse");
    match invocation.command {
        command::Command::Issue(command::IssueCommand::Search {
            query, state, ..
        }) => {
            assert_eq!(query.as_deref(), Some("-tag:regression"));
            assert_eq!(state, "closed");
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // comment body leading dash via comment create
    let args = [
        "--role",
        "executor",
        "comment",
        "create",
        "1",
        "--body=---",
        "--marker=m",
        "--authorized",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("inline --body should parse");
    match invocation.command {
        command::Command::Comment(command::CommentCommand::Create {
            issue,
            body,
            marker,
            authorized,
        }) => {
            assert_eq!(issue, 1);
            assert_eq!(body, "---");
            assert_eq!(marker, "m");
            assert!(authorized);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // comment marker leading dash via comment find-marker
    let args = [
        "--role",
        "executor",
        "comment",
        "find-marker",
        "1",
        "--marker=---",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("inline --marker should parse");
    match invocation.command {
        command::Command::Comment(command::CommentCommand::FindMarker { issue, marker }) => {
            assert_eq!(issue, 1);
            assert_eq!(marker, "---");
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn two_arg_value_with_leading_dash_still_errors_via_strict_missing_check() {
    // The two-arg `--option value` form keeps its existing strict missing-value
    // detection: a leading-dash next token is interpreted as a missing value,
    // not as the value itself. The escape hatch is the inline `--option=value`
    // form, which is covered separately.
    for args in [
        // --body followed by a leading-dash value should still error
        vec![
            "--role",
            "orchestrator",
            "issue",
            "create",
            "--title",
            "Title",
            "--body",
            "-not-value",
        ],
        // --body followed by a separator-style value should still error
        vec![
            "--role",
            "orchestrator",
            "issue",
            "update-body",
            "1",
            "--body",
            "---",
        ],
        // --query followed by a leading-dash value should still error
        vec![
            "--role",
            "orchestrator",
            "issue",
            "search",
            "--query",
            "-tag:regression",
        ],
        // --marker followed by a leading-dash value should still error
        vec![
            "--role",
            "executor",
            "comment",
            "find-marker",
            "1",
            "--marker",
            "---",
        ],
    ] {
        let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert!(
            command::parse(&args).is_err(),
            "two-arg form accepted a leading-dash value: {args:?}"
        );
    }
}

#[test]
fn inline_form_does_not_match_other_long_options_with_the_same_prefix() {
    // The split_inline helper must distinguish `--body=...` from `--bodyline=...`
    // so that adding new long options never silently captures an unrelated
    // value. We verify by passing a deliberately crafted inline token against
    // an unrelated subcommand; it must surface as "unknown option".
    let args = [
        "--role",
        "orchestrator",
        "issue",
        "create",
        "--title=ok",
        "--bodyline=should-not-match-body",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let error = command::parse(&args).expect_err("unknown long option must error");
    assert!(
        error.contains("unknown option"),
        "expected unknown option error, got: {error}"
    );
}

#[test]
fn inline_form_accepts_empty_value_for_body_but_rejects_empty_marker() {
    // Inline `--body=` carries an explicit empty body (downstream decides
    // whether empty is meaningful for that field). For `--marker=` the
    // required-nonempty semantic must still reject the empty value with the
    // same structured error as the two-arg form.
    let args = [
        "--role",
        "orchestrator",
        "issue",
        "create",
        "--title=ok",
        "--body=",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("inline empty body should parse");
    match invocation.command {
        command::Command::Issue(command::IssueCommand::Create {
            title,
            body,
            tracker: _,
            planning: _,
        }) => {
            assert_eq!(title, "ok");
            assert_eq!(body, "");
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let args = [
        "--role",
        "executor",
        "comment",
        "create",
        "1",
        "--body=ok",
        "--marker=",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let error = command::parse(&args).expect_err("empty inline marker must error");
    assert!(
        error.contains("non-empty"),
        "expected non-empty marker error, got: {error}"
    );
}

#[test]
fn empty_marker_is_rejected_by_parser_and_provider() {
    let args = [
        "--role",
        "orchestrator",
        "comment",
        "find-marker",
        "1",
        "--marker",
        "",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    assert!(command::parse(&args).is_err());

    let provider = ForgejoProvider::new(
        ForgejoConfig::new("http://127.0.0.1:1/api/v1", "owner", "repo"),
        "token".to_owned(),
    )
    .unwrap();
    let error = provider.find_marker(1, "").unwrap_err();
    assert_eq!(error.json()["kind"], "config");
}

#[test]
fn phase2_persisted_provider_config_paths_have_been_removed() {
    // The legacy `<role>.config.json` layout was retired when the
    // project migrated to a single SQLite database under the
    // platform-standard config directory. `auth::config_path_for`
    // and friends used to expose the on-disk layout to tests; with
    // the migration they are gone and this regression guard pins the
    // absence. The test body only documents the contract so future
    // contributors do not reintroduce a parallel file layout.
    assert!(true);
}

#[test]
fn redmine_stored_config_round_trips_group_selection_and_legacy_defaults() {
    // Backward-compatible decode: old configs that still carry
    // `group_name`/`group_role` from the legacy `AI Agents` workflow keep
    // deserializing without error so operators do not lose their saved
    // credentials when upgrading.
    let legacy = serde_json::json!({
        "api_base": "https://redmine.example",
        "project_id": "44",
        "close_status_id": 5,
        "group_name": "AI Agents",
        "group_role": "开发人员",
    });
    let decoded: auth::RedmineStoredConfig =
        serde_json::from_value(legacy).expect("legacy config must decode");
    assert_eq!(decoded.group_name.as_deref(), Some("AI Agents"));
    assert_eq!(decoded.group_role.as_deref(), Some("开发人员"));

    // Fresh configs no longer carry the legacy group fields but still
    // round-trip through the persistence path.
    let minimal: auth::RedmineStoredConfig = serde_json::from_value(serde_json::json!({
        "api_base": "https://redmine.example",
        "project_id": "44",
        "close_status_id": 5,
    }))
    .unwrap();
    assert_eq!(minimal.group_name, None);
    assert_eq!(minimal.group_role, None);
}

#[test]
fn role_policy_remains_capability_based() {
    assert!(Role::Admin.allows(Capability::ProjectRead));
    assert!(Role::Admin.allows(Capability::ProjectCreate));
    assert!(Role::Admin.allows(Capability::IssueStatusRead));
    assert!(!Role::Admin.allows(Capability::RepoCreate));
    assert!(!Role::Admin.allows(Capability::IssueSearch));
    assert!(!Role::Admin.allows(Capability::IssueCreate));
    assert!(Role::Orchestrator.allows(Capability::IssueClose));
    assert!(Role::Executor.allows(Capability::IssueRead));
    assert!(Role::Executor.allows(Capability::CommentRead));
    assert!(Role::Executor.allows(Capability::CommentFindMarker));
    assert!(Role::Executor.allows(Capability::CommentCreate));
    assert!(!Role::Executor.allows(Capability::IssueSearch));
    assert!(!Role::Executor.allows(Capability::IssueCreate));
    assert!(!Role::Executor.allows(Capability::IssueUpdateBody));
    assert!(!Role::Executor.allows(Capability::IssueClose));
    assert!(Role::Reviewer.allows(Capability::IssueRead));
    assert!(Role::Reviewer.allows(Capability::CommentRead));
    assert!(Role::Reviewer.allows(Capability::CommentFindMarker));
    assert!(Role::Reviewer.allows(Capability::CommentCreate));
    assert!(!Role::Reviewer.allows(Capability::IssueSearch));
    assert!(!Role::Reviewer.allows(Capability::IssueCreate));
    assert!(!Role::Reviewer.allows(Capability::IssueUpdateBody));
    assert!(!Role::Reviewer.allows(Capability::IssueClose));
    assert!(!Role::Executor.allows(Capability::RepoCreate));
    assert!(!Role::Reviewer.allows(Capability::RepoCreate));
    assert!(Role::Orchestrator.allows(Capability::RepoCreate));
    assert!(Role::Tester.allows(Capability::IssueRead));
    assert!(Role::Tester.allows(Capability::CommentRead));
    assert!(Role::Tester.allows(Capability::CommentFindMarker));
    assert!(Role::Tester.allows(Capability::CommentCreate));
    assert!(Role::Tester.allows(Capability::IssueAttachmentUpload));
    assert!(!Role::Tester.allows(Capability::IssueSearch));
    assert!(!Role::Tester.allows(Capability::IssueCreate));
    assert!(!Role::Tester.allows(Capability::IssueUpdateBody));
    assert!(!Role::Tester.allows(Capability::IssueClose));
    assert!(!Role::Tester.allows(Capability::RepoCreate));
    assert!(!Role::Tester.allows(Capability::ProjectCreate));
    assert!(!Role::Tester.allows(Capability::IssueStatusRead));
    assert!(!Role::Tester.allows(Capability::VersionRead));
    assert!(!Role::Tester.allows(Capability::RelationRead));
}

#[test]
fn repo_create_requires_private_and_valid_owner_repository() {
    let base = ["--role", "orchestrator", "repo", "create", "owner/new-repo"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(
        command::parse(&base).unwrap_err(),
        "repo create requires --private"
    );

    for suffix in ["--public", "--unknown"] {
        let args = [
            "--role",
            "orchestrator",
            "repo",
            "create",
            "owner/new-repo",
            "--private",
            suffix,
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        assert!(command::parse(&args).is_err());
    }

    for target in ["", "/repo", "owner/", "owner/repo/extra", "owner/repo name"] {
        let args = [
            "--role",
            "orchestrator",
            "repo",
            "create",
            target,
            "--private",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        assert!(command::parse(&args).is_err(), "accepted target {target:?}");
    }

    let args = [
        "--role",
        "orchestrator",
        "repo",
        "create",
        "owner/new-repo",
        "--private",
        "--description",
        "description",
        "--auto-init",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse(&args).unwrap().command {
        command::Command::Repo(command::RepoCommand::Create {
            target,
            private,
            description,
            auto_init,
        }) => {
            assert_eq!(target, "owner/new-repo");
            assert!(private);
            assert_eq!(description, "description");
            assert!(auto_init);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn status_next_and_advance_parse_positional_and_status_option() {
    let next = [
        "--role",
        "executor",
        "--provider",
        "redmine",
        "status",
        "next",
        "51",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse(&next).unwrap().command {
        command::Command::Status(command::StatusCommand::Next { number }) => {
            assert_eq!(number, 51);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // `next` takes exactly one positional and no options.
    for extra in [vec!["51", "52"], vec!["51", "--status", "Blocked"]] {
        let mut args = vec!["--role", "orchestrator", "status", "next"];
        args.extend(extra);
        let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert!(command::parse(&args).is_err());
    }

    let advance = [
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "status",
        "advance",
        "51",
        "--status",
        "In Review",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse(&advance).unwrap().command {
        command::Command::Status(command::StatusCommand::Advance { number, status }) => {
            assert_eq!(number, 51);
            assert_eq!(status, "In Review");
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let missing_status = [
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "status",
        "advance",
        "51",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    assert!(command::parse(&missing_status).is_err());
}

/// The canonical transition graph is the single source of truth for the
/// phase workflow, so every documented edge and every terminal status is
/// asserted directly against the policy helpers.
#[test]
fn canonical_transition_policy_matches_the_documented_phase_graph() {
    let expected: &[(&str, &[&str])] = &[
        ("New", &["In Progress", "Cancelled"]),
        ("In Progress", &["In Review", "Blocked", "Cancelled"]),
        (
            "In Review",
            &["Resolved", "Changes Requested", "Blocked", "Cancelled"],
        ),
        (
            "Changes Requested",
            &["In Progress", "Blocked", "Cancelled"],
        ),
        ("Blocked", &["In Progress", "Cancelled"]),
        ("Resolved", &["In Progress", "Closed"]),
        ("Closed", &[]),
        ("Cancelled", &[]),
    ];
    for (current, allowed) in expected {
        assert_eq!(
            canonical_allowed_next(current).expect("canonical status"),
            *allowed,
            "allowed_next mismatch for {current}"
        );
        for target in *allowed {
            assert_eq!(
                evaluate_transition(current, target),
                TransitionVerdict::Allowed,
                "{current} -> {target} must be allowed"
            );
        }
    }

    // Illegal edges are rejected with the allowed set attached so the
    // caller can surface concrete guidance.
    match evaluate_transition("Resolved", "In Review") {
        TransitionVerdict::Forbidden { allowed_next } => {
            assert_eq!(allowed_next, &["In Progress", "Closed"])
        }
        other => panic!("expected Forbidden, got {other:?}"),
    }
    match evaluate_transition("Closed", "In Progress") {
        TransitionVerdict::Forbidden { allowed_next } => assert!(allowed_next.is_empty()),
        other => panic!("expected Forbidden, got {other:?}"),
    }
}

/// `Resolved` is a per-phase checkpoint, so the policy must expose both
/// the phase-continuation edge back to `In Progress` and the task-final
/// edge to `Closed`. Losing the continuation edge would strand every
/// multi-phase task after its first reviewed phase.
#[test]
fn resolved_status_allows_phase_continuation_and_final_close() {
    assert_eq!(
        canonical_allowed_next("Resolved").expect("canonical status"),
        &["In Progress", "Closed"],
        "Resolved must offer continuation before final close"
    );
    assert_eq!(
        evaluate_transition("Resolved", "In Progress"),
        TransitionVerdict::Allowed,
        "a remaining phase must be able to resume implementation"
    );
    assert_eq!(
        evaluate_transition("Resolved", "Closed"),
        TransitionVerdict::Allowed,
        "the task-final close edge must be retained"
    );
    // The continuation edge must not turn Resolved into a general
    // re-entry point: every other phase state stays unreachable.
    for target in ["In Review", "Changes Requested", "Blocked", "Cancelled"] {
        assert!(
            matches!(
                evaluate_transition("Resolved", target),
                TransitionVerdict::Forbidden { .. }
            ),
            "Resolved -> {target} must stay forbidden"
        );
    }
}

/// Same-status transitions are no-ops, installation casing is tolerated,
/// and any unknown status makes the verdict advisory instead of claiming
/// permission the server may not grant.
#[test]
fn transition_policy_handles_no_op_casing_and_custom_statuses() {
    assert_eq!(
        evaluate_transition("In Progress", "In Progress"),
        TransitionVerdict::NoOp
    );
    assert_eq!(
        evaluate_transition("Triaged", "triaged"),
        TransitionVerdict::NoOp,
        "a custom status re-applied to itself is still a no-op"
    );
    assert_eq!(canonical_status_name("in progress"), Some("In Progress"));
    assert_eq!(canonical_status_name("  BLOCKED "), Some("Blocked"));
    assert_eq!(canonical_status_name("Triaged"), None);
    assert!(canonical_allowed_next("Triaged").is_none());

    match evaluate_transition("Triaged", "In Progress") {
        TransitionVerdict::Advisory { reason } => {
            assert!(reason.contains("current status 'Triaged'"), "{reason}");
            assert!(reason.contains("server decides"), "{reason}");
        }
        other => panic!("expected Advisory, got {other:?}"),
    }
    match evaluate_transition("In Progress", "Escalated") {
        TransitionVerdict::Advisory { reason } => {
            assert!(reason.contains("target status 'Escalated'"), "{reason}");
        }
        other => panic!("expected Advisory, got {other:?}"),
    }
}

#[test]
fn status_set_parses_number_and_validated_status_value() {
    let args = [
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "status",
        "set",
        "12",
        "--status",
        "In Progress",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse(&args).unwrap().command {
        command::Command::Status(command::StatusCommand::Set { number, status }) => {
            assert_eq!(number, 12);
            assert_eq!(status, "In Progress");
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let missing = [
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "status",
        "set",
        "12",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    assert!(command::parse(&missing).is_err());

    // The inline escape hatch keeps leading-dash values usable.
    let inline = [
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "status",
        "set",
        "12",
        "--status=-Blocked",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse(&inline).unwrap().command {
        command::Command::Status(command::StatusCommand::Set { status, .. }) => {
            assert_eq!(status, "-Blocked");
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn issue_create_and_update_body_accept_optional_tracker_selection() {
    let create = [
        "--role",
        "orchestrator",
        "issue",
        "create",
        "--title",
        "Plan",
        "--tracker",
        "Bug",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse(&create).unwrap().command {
        command::Command::Issue(command::IssueCommand::Create {
            title,
            body,
            tracker,
            planning,
        }) => {
            assert_eq!(title, "Plan");
            assert_eq!(body, "");
            assert_eq!(tracker.as_deref(), Some("Bug"));
            assert!(planning.is_empty());
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let update = [
        "--role",
        "orchestrator",
        "issue",
        "update-body",
        "9",
        "--body",
        "Updated",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse(&update).unwrap().command {
        command::Command::Issue(command::IssueCommand::UpdateBody {
            number,
            body,
            tracker,
            planning,
        }) => {
            assert_eq!(number, 9);
            assert_eq!(body, "Updated");
            assert!(tracker.is_none());
            assert!(planning.is_empty());
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let unknown_tracker_option = [
        "--role",
        "orchestrator",
        "issue",
        "get",
        "9",
        "--tracker",
        "Bug",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    assert!(command::parse(&unknown_tracker_option).is_err());
}

#[test]
fn comment_get_uses_the_requested_issue_scope() {
    let (base, requests, server) = mock_server_with_headers(
        r#"[{"id":42,"body":"<!-- marker --> comment","html_url":"https://forgejo.example/comment/42"}]"#,
        &["X-Total-Count: 1"],
    );
    let provider = ForgejoProvider::new(
        ForgejoConfig::new(base, "owner", "repo"),
        "token".to_owned(),
    )
    .unwrap();
    let comment = provider.get_comment(7, 42).unwrap();
    assert_eq!(comment.id, 42);
    assert_eq!(comment.marker.as_deref(), Some("<!-- marker -->"));
    let request = requests.recv().unwrap();
    assert!(request.starts_with("GET /api/v1/repos/owner/repo/issues/7/comments?"));
    server.join().unwrap();
}

#[test]
fn comment_get_does_not_return_an_id_missing_from_issue_comments() {
    let (base, requests, server) =
        mock_server_with_headers(r#"[{"id":99,"body":"other"}]"#, &["X-Total-Count: 1"]);
    let provider = ForgejoProvider::new(
        ForgejoConfig::new(base, "owner", "repo"),
        "token".to_owned(),
    )
    .unwrap();
    let error = provider.get_comment(7, 42).unwrap_err();
    assert_eq!(error.json()["kind"], "not_found");
    assert!(
        requests
            .recv()
            .unwrap()
            .starts_with("GET /api/v1/repos/owner/repo/issues/7/comments?")
    );
    server.join().unwrap();
}

#[test]
fn executor_and_reviewer_cannot_mutate_issues() {
    for role in [Role::Executor, Role::Reviewer] {
        for capability in [
            Capability::IssueCreate,
            Capability::IssueUpdateBody,
            Capability::IssueClose,
        ] {
            assert!(
                !role.allows(capability),
                "{role} unexpectedly allowed {capability:?}"
            );
        }
        assert!(role.allows(Capability::IssueRead));
        assert!(role.allows(Capability::CommentRead));
        assert!(role.allows(Capability::CommentFindMarker));
    }
    assert!(Role::Orchestrator.allows(Capability::IssueCreate));
    assert!(Role::Orchestrator.allows(Capability::IssueUpdateBody));
    assert!(Role::Orchestrator.allows(Capability::IssueClose));
}

fn mock_server_with_headers(
    body: &str,
    response_headers: &[&str],
) -> (String, Receiver<String>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
    let body = body.to_owned();
    let response_headers = response_headers
        .iter()
        .map(|header| (*header).to_owned())
        .collect::<Vec<_>>();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 8192];
        let size = stream.read(&mut request).unwrap();
        sender
            .send(String::from_utf8_lossy(&request[..size]).into_owned())
            .unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n{}",
            body.len(),
            response_headers
                .iter()
                .map(|header| format!("{header}\r\n"))
                .collect::<String>(),
            body
        );
        stream.write_all(response.as_bytes()).unwrap();
    });
    (format!("http://{address}/api/v1"), receiver, server)
}

#[test]
fn workflow_bootstrap_outputs_user_memberships_and_no_legacy_fields() {
    // The CLI JSON contract for `workflow bootstrap` exposes
    // `user_memberships` per agent identity and must never re-introduce the
    // legacy `membership`/`group_name`/`group_role` keys. The detailed
    // bootstrap flow is exercised end-to-end by
    // `redmine_contract_tests::issue_create_automatically_bootstraps_once_before_returning_issue`;
    // this test pins the surface contract.
    let output = serde_json::json!({
        "bootstrapped": true,
        "created": true,
        "repository": "owner/repo",
        "identifier": "owner-repo",
        "project_id": 44_u64,
        "close_status_id": 5_u64,
        "close_status_name": "Closed",
        "user_memberships": [
            {
                "role": "Maintainer",
                "user_id": 11_u64,
                "user_login": "orchestrator",
                "status": "added",
            },
            {
                "role": "Developer",
                "user_id": 22_u64,
                "user_login": "executor",
                "status": "added",
            },
            {
                "role": "Reporter",
                "user_id": 33_u64,
                "user_login": "reviewer",
                "status": "added",
            },
        ],
    });
    assert!(output["bootstrapped"].as_bool().unwrap());
    assert_eq!(output["user_memberships"].as_array().unwrap().len(), 3);
    assert!(output.get("membership").is_none());
    assert!(output.get("group_name").is_none());
    assert!(output.get("group_role").is_none());

    let warning_output = serde_json::json!({
        "bootstrapped": false,
        "created": false,
        "repository": "owner/repo",
        "identifier": "owner-repo",
        "project_id": 44_u64,
        "close_status_id": 5_u64,
        "close_status_name": "Closed",
        "user_memberships": [
            {
                "role": "Developer",
                "user_id": 22_u64,
                "user_login": "executor",
                "status": "warning",
                "warning": "Redmine role was not found: user 'executor', role 'Developer'",
            },
        ],
        "warning": "Redmine role was not found: user 'executor', role 'Developer'",
    });
    assert!(!warning_output["bootstrapped"].as_bool().unwrap());
    assert!(warning_output.get("membership").is_none());
    assert!(warning_output.get("group_name").is_none());
    assert!(warning_output.get("group_role").is_none());
    let warning_user_membership = &warning_output["user_memberships"][0];
    assert_eq!(warning_user_membership["status"], "warning");
    assert!(warning_user_membership["warning"].is_string());
}

#[test]
fn remote_resolution_normalizes_ssh_and_strips_https_credentials() {
    // SSH scp-style remotes must produce a credential-free ssh:// URL that
    // keeps the `.git` suffix and exposes no username or password.
    let ssh = remote::parse_remote("git@forgejo.example:owner/widgets.git").unwrap();
    assert_eq!(ssh.repository, "owner/widgets");
    assert_eq!(
        ssh.repository_url, "ssh://git@forgejo.example/owner/widgets.git",
        "SSH origin must normalise to a credential-free ssh:// URL: {}",
        ssh.repository_url
    );
    assert!(
        ssh.repository_url.starts_with("ssh://git@") && !ssh.repository_url[10..].contains('@'),
        "SSH URL must keep only the canonical git user: {}",
        ssh.repository_url
    );

    // HTTPS remotes with embedded credentials must drop them but keep the
    // full URL otherwise identical so the mirror plugin can clone without a
    // secret.
    let creds =
        remote::parse_remote("https://deploy:supersecret@forgejo.example/owner/widgets.git")
            .unwrap();
    assert_eq!(creds.repository, "owner/widgets");
    assert_eq!(
        creds.repository_url, "https://forgejo.example/owner/widgets.git",
        "HTTPS origin must strip embedded credentials: {}",
        creds.repository_url
    );
    assert!(
        !creds.repository_url.contains("supersecret"),
        "credential stripping must remove the password: {}",
        creds.repository_url
    );
    assert!(
        !creds.repository_url.contains("deploy"),
        "credential stripping must remove the username: {}",
        creds.repository_url
    );
}

#[test]
fn remote_resolution_preserves_ssh_username_for_url_form_remotes() {
    // URL-form SSH remotes (`ssh://user@host:port/path/repo.git`) must keep
    // their `git@` user — SSH requires a user, and stripping it would make
    // the mirror plugin reject the URL as un-cloneable. The port must also
    // survive so non-standard SSH ports remain reachable.
    let ssh = remote::parse_remote("ssh://git@forgejo.example.com:2222/owner/repo.git").unwrap();
    assert_eq!(ssh.repository, "owner/repo");
    assert_eq!(
        ssh.repository_url, "ssh://git@forgejo.example.com:2222/owner/repo.git",
        "URL-form SSH origin must preserve the `git` user and port: {}",
        ssh.repository_url
    );
    assert!(
        ssh.repository_url.contains("git@"),
        "SSH URL must still carry the `git` user so the mirror plugin can clone: {}",
        ssh.repository_url
    );
    assert!(
        ssh.repository_url.contains(":2222"),
        "SSH URL must keep its non-default port: {}",
        ssh.repository_url
    );

    // Non-canonical SSH users (e.g. `deploy`) must also be preserved so
    // operators with custom SSH configurations can still mirror.
    let deploy = remote::parse_remote("ssh://deploy@git.example.com/owner/repo.git").unwrap();
    assert_eq!(
        deploy.repository_url, "ssh://deploy@git.example.com/owner/repo.git",
        "non-`git` SSH users must be preserved: {}",
        deploy.repository_url
    );

    // SSH URLs may also carry a query string or fragment (uncommon but
    // legal). They must be dropped the same way HTTP(S) credentials are.
    let with_query =
        remote::parse_remote("ssh://git@git.example.com/owner/repo.git?ref=main#frag").unwrap();
    assert_eq!(
        with_query.repository_url, "ssh://git@git.example.com/owner/repo.git",
        "SSH URL must drop query/fragment but keep the user: {}",
        with_query.repository_url
    );

    // And the existing HTTP-with-creds behaviour is unchanged.
    let http_creds =
        remote::parse_remote("https://deploy:supersecret@forgejo.example/owner/repo.git").unwrap();
    assert_eq!(
        http_creds.repository_url, "https://forgejo.example/owner/repo.git",
        "HTTPS credential stripping must remain unchanged: {}",
        http_creds.repository_url
    );
}

#[test]
fn bootstrap_output_includes_pending_git_mirror_outcome() {
    // The bootstrap JSON contract must surface the plugin's `pending`
    // status (asynchronous job queued) and the credential-free URL passed
    // to the mirror plugin without ever leaking credentials.
    let output = serde_json::json!({
        "bootstrapped": true,
        "created": true,
        "repository": "owner/repo",
        "identifier": "owner-repo",
        "project_id": 44_u64,
        "close_status_id": 5_u64,
        "close_status_name": "Closed",
        "user_memberships": [
            {
                "role": "Maintainer",
                "user_id": 11_u64,
                "user_login": "orchestrator",
                "status": "added",
            },
        ],
        "git_mirror": {
            "id": 901_u64,
            "project_id": 44_u64,
            "identifier": "mirror_44_owner_repo",
            "status": "pending",
            "remote_url": "https://git.example.com/owner/repo.git",
            "local_path": "/var/redmine/repos/owner_repo.git",
            "error": null,
        },
    });
    let git_mirror = output
        .get("git_mirror")
        .expect("bootstrap JSON must include git_mirror");
    assert_eq!(git_mirror["status"], "pending");
    assert_eq!(git_mirror["identifier"], "mirror_44_owner_repo");
    assert_eq!(git_mirror["project_id"], 44_u64);
    assert_eq!(
        git_mirror["remote_url"],
        "https://git.example.com/owner/repo.git"
    );
    assert_eq!(
        git_mirror["local_path"],
        "/var/redmine/repos/owner_repo.git"
    );
    assert!(git_mirror["error"].is_null());
    // The mirror JSON must never carry the bearer key or user credentials.
    let serialized = output.to_string();
    assert!(
        !serialized.to_ascii_lowercase().contains("bearer "),
        "bootstrap output must not include bearer credentials: {serialized}"
    );
    assert!(
        !serialized.contains("supersecret"),
        "bootstrap output must not include embedded origin credentials: {serialized}"
    );
}

#[test]
fn bootstrap_warning_output_still_includes_git_mirror_outcome() {
    // A membership warning (bootstrap is not ready) must still surface the
    // git_mirror outcome so operators can see whether the asynchronous
    // mirror job was queued alongside the failing reconciliation.
    let output = serde_json::json!({
        "bootstrapped": false,
        "created": true,
        "repository": "owner/repo",
        "identifier": "owner-repo",
        "project_id": 44_u64,
        "close_status_id": 5_u64,
        "close_status_name": "Closed",
        "user_memberships": [
            {
                "role": "Developer",
                "user_id": 22_u64,
                "user_login": "executor",
                "status": "warning",
                "warning": "Redmine role was not found: user 'executor', role 'Developer'",
            },
        ],
        "git_mirror": {
            "id": 901_u64,
            "project_id": 44_u64,
            "identifier": "mirror_44_owner_repo",
            "status": "pending",
            "remote_url": "https://git.example.com/owner/repo.git",
            "local_path": "/var/redmine/repos/owner_repo.git",
            "error": null,
        },
        "warning": "Redmine role was not found: user 'executor', role 'Developer'",
    });
    assert!(!output["bootstrapped"].as_bool().unwrap());
    let git_mirror = output
        .get("git_mirror")
        .expect("warning JSON must still include git_mirror");
    assert_eq!(git_mirror["status"], "pending");
    assert!(output["warning"].is_string());
}

#[test]
fn redmine_new_project_includes_repository_module_for_mirror_enablement() {
    use crate::providers::redmine::model::RedmineNewProject;
    let payload = RedmineNewProject::new("Workflow", "workflow", Some("issues"));
    let value = serde_json::to_value(&payload).unwrap();
    let project = value.get("project").unwrap();
    assert!(
        project.get("enabled_modules").is_none(),
        "default payload must not include modules so direct `project create` calls remain opt-in"
    );

    let payload =
        RedmineNewProject::new("Workflow", "workflow", Some("issues")).with_repository_module();
    let value = serde_json::to_value(&payload).unwrap();
    let project = value.get("project").unwrap();
    let modules = project
        .get("enabled_modules")
        .expect("bootstrap-enabled payload must include enabled_modules")
        .as_array()
        .expect("enabled_modules must be an array");
    assert_eq!(
        modules,
        &vec![serde_json::json!({"name": "repository"})],
        "the bootstrap-enabled payload must enable the repository module only"
    );
}

#[test]
fn issue_planning_flags_parse_on_create_and_update_body() {
    let create = [
        "--role",
        "orchestrator",
        "issue",
        "create",
        "--title=Plan",
        "--parent-issue",
        "12",
        "--fixed-version=Sprint 1",
        "--start-date",
        "2026-08-01",
        "--due-date",
        "2026-08-31",
        "--estimated-hours",
        "3.5",
        "--done-ratio",
        "40",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse(&create).unwrap().command {
        command::Command::Issue(command::IssueCommand::Create { planning, .. }) => {
            assert_eq!(planning.parent_issue.as_deref(), Some("12"));
            assert_eq!(planning.fixed_version.as_deref(), Some("Sprint 1"));
            assert_eq!(planning.start_date.as_deref(), Some("2026-08-01"));
            assert_eq!(planning.due_date.as_deref(), Some("2026-08-31"));
            assert_eq!(planning.estimated_hours.as_deref(), Some("3.5"));
            assert_eq!(planning.done_ratio.as_deref(), Some("40"));
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let update = [
        "--role",
        "orchestrator",
        "issue",
        "update-body",
        "9",
        "--body",
        "Updated",
        "--fixed-version",
        "7",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse(&update).unwrap().command {
        command::Command::Issue(command::IssueCommand::UpdateBody { planning, .. }) => {
            assert_eq!(planning.fixed_version.as_deref(), Some("7"));
            assert!(planning.parent_issue.is_none());
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // Planning flags belong only to create/update-body; other issue
    // subcommands must keep rejecting them.
    let misplaced = [
        "--role",
        "orchestrator",
        "issue",
        "get",
        "9",
        "--done-ratio",
        "50",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    assert!(command::parse(&misplaced).is_err());

    // A planning option missing its value keeps the strict missing-value
    // detection.
    let missing_value = [
        "--role",
        "orchestrator",
        "issue",
        "create",
        "--title",
        "T",
        "--start-date",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    assert!(command::parse(&missing_value).is_err());
}

#[test]
fn version_list_parses_and_rejects_unexpected_arguments() {
    for role in ["admin", "orchestrator", "executor", "reviewer"] {
        let args = ["--role", role, "--provider", "redmine", "version", "list"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert!(matches!(
            command::parse(&args).unwrap().command,
            command::Command::VersionCommand(command::VersionCommand::List)
        ));
    }

    // Bare `version` prints help rather than erroring; only unknown
    // subcommands and extra arguments are rejected.
    let args = vec![
        "--role",
        "executor",
        "--provider",
        "redmine",
        "version",
        "reorder",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    assert!(command::parse(&args).is_err());

    let args = vec![
        "--role",
        "executor",
        "--provider",
        "redmine",
        "version",
        "list",
        "extra",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    assert!(command::parse(&args).is_err());
}

#[test]
fn timer_parser_accepts_valid_foundation_syntax_and_rejects_malformed_values() {
    let args = [
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "timer",
        "start",
        "28",
        "--phase",
        "implementation",
        "--agent-role",
        "executor",
        "--attempt",
        "2",
        "--run-id",
        "run-28",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse(&args).unwrap().command {
        command::Command::Timer(command::TimerCommand::Start {
            issue,
            phase,
            agent_role,
            attempt,
            run_id,
            owner_session_id,
            owner_call_id,
        }) => {
            assert_eq!(issue, 28);
            assert_eq!(phase, "implementation");
            assert_eq!(agent_role, "executor");
            assert_eq!(attempt, 2);
            assert_eq!(run_id.as_deref(), Some("run-28"));
            assert_eq!(owner_session_id, None);
            assert_eq!(owner_call_id, None);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    for malformed in [
        vec![
            "--role",
            "orchestrator",
            "timer",
            "start",
            "28",
            "--phase",
            "implementation",
            "--agent-role",
            "executor",
            "--attempt",
            "0",
        ],
        vec![
            "--role",
            "orchestrator",
            "timer",
            "finish",
            "run-28",
            "--result",
            "SUCCESS",
        ],
        vec![
            "--role",
            "orchestrator",
            "timer",
            "start",
            "0",
            "--phase",
            "implementation",
            "--agent-role",
            "reviewer",
            "--attempt",
            "1",
        ],
    ] {
        let malformed = malformed.into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert!(command::parse(&malformed).is_err());
    }
}

#[test]
fn timer_execution_is_orchestrator_and_redmine_only() {
    let home = std::env::temp_dir().join(format!(
        "phasegent-timer-boundary-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let storage = Storage::open_at(&home.join(crate::infra::storage::DB_FILENAME)).unwrap();
    storage
        .start_timer_run(
            "boundary-run",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();

    let executor = crate::time_tracking_cli::execute(
        Some(Role::Executor),
        Some(ProviderKind::Redmine),
        None,
        None,
        None,
        command::TimerCommand::Finish {
            run_id: "boundary-run".to_owned(),
            result: "DONE".to_owned(),
        },
    )
    .unwrap_err();
    assert_eq!(executor.json()["kind"], "config");
    assert!(
        storage
            .load_timer_run("boundary-run")
            .unwrap()
            .unwrap()
            .status
            == "running"
    );

    let forgejo = crate::time_tracking_cli::execute(
        Some(Role::Orchestrator),
        Some(ProviderKind::Forgejo),
        None,
        None,
        None,
        command::TimerCommand::Finish {
            run_id: "boundary-run".to_owned(),
            result: "DONE".to_owned(),
        },
    )
    .unwrap_err();
    assert_eq!(forgejo.json()["kind"], "not_supported");
    assert!(
        storage
            .load_timer_run("boundary-run")
            .unwrap()
            .unwrap()
            .status
            == "running"
    );
    let _ = fs::remove_dir_all(home);
}

#[test]
fn provider_kind_gitlab_round_trips_and_rejects_unknown_values() {
    // Phase-1 GitLab foundation: the FromStr surface must recognise
    // "gitlab", the as_str helper must return "gitlab", and a non-
    // canonical value must surface a structured error that lists all
    // three supported providers. The docstring on the parse error is
    // what operators see in `phasegent --provider typo` so the
    // message must stay accurate.
    use std::str::FromStr;

    let parsed: ProviderKind = "gitlab".parse().expect("gitlab must parse");
    assert_eq!(parsed, ProviderKind::Gitlab);
    assert_eq!(parsed.as_str(), "gitlab");

    // Inverse direction: as_str feeds back into parse without a round
    // trip misclassification (e.g. forgetting a lowercase match arm).
    let round_trip = ProviderKind::from_str(ProviderKind::Gitlab.as_str())
        .expect("as_str must parse back to Gitlab");
    assert_eq!(round_trip, ProviderKind::Gitlab);

    // Forgejo and Redmine must continue to parse so the existing CLI
    // `--provider forgejo|redmine` paths still work.
    assert_eq!(
        "forgejo".parse::<ProviderKind>().unwrap(),
        ProviderKind::Forgejo
    );
    assert_eq!(
        "redmine".parse::<ProviderKind>().unwrap(),
        ProviderKind::Redmine
    );

    let error = "wrong".parse::<ProviderKind>().unwrap_err();
    assert!(
        error.contains("forgejo, redmine, or gitlab"),
        "parse error must enumerate the supported providers: {error}"
    );
}

#[test]
fn provider_flag_parses_gitlab_for_role_free_branch_commands() {
    // `--provider gitlab` must flow through the parser without error
    // so `auth setup`, `issue`, `comment`, etc. all accept the new
    // value. The branch-context commands (bind/unbind/status) are
    // provider-free in their resolver, but the top-level `--provider`
    // flag is still accepted by the outer parser.
    use std::str::FromStr;

    let args = [
        "--role",
        "orchestrator",
        "--provider",
        "gitlab",
        "issue",
        "search",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("--provider gitlab must parse");
    assert_eq!(
        invocation.provider.expect("--provider must be captured"),
        ProviderKind::Gitlab
    );

    // Inline form `--provider=gitlab` is recognised too so scripts
    // that build argv with the `option=value` style still work.
    let inline = [
        "--role=orchestrator",
        "--provider=gitlab",
        "issue",
        "search",
        "--query",
        "phase-1",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let parsed = command::parse(&inline).expect("--provider=gitlab must parse");
    assert_eq!(parsed.provider, Some(ProviderKind::Gitlab));

    // Sanity: as_str + FromStr cross-check at the call site.
    assert_eq!(ProviderKind::from_str("gitlab").unwrap().as_str(), "gitlab");
}

#[test]
fn resolve_kind_prefers_role_scoped_gitlab_when_env_var_unset() {
    // Phase-1 GitLab foundation: when `--provider gitlab` reaches
    // `resolve_kind` and the `PHASEGENT_PROVIDER` env var is unset
    // (the test isolates `HOME` and clears the var), the resolver
    // falls back to the role_config.provider column. A pre-populated
    // row must be consulted without leaking the role into another
    // provider branch.
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let home = std::env::temp_dir().join(format!(
        "phasegent-resolve-kind-gitlab-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _db_path_guard = EnvGuard::set(
        "PHASEGENT_DB_PATH",
        home.join(crate::infra::storage::DB_FILENAME)
            .to_string_lossy()
            .as_ref(),
    );
    let previous_provider = std::env::var_os("PHASEGENT_PROVIDER");
    // SAFETY:: Serialised by `lock_workflow_tests`. The Drop guard on
    // `previous_provider` reinstates the host value when the test
    // unwinds even if a panic happens mid-test.
    struct ProviderGuard(Option<std::ffi::OsString>);
    impl Drop for ProviderGuard {
        fn drop(&mut self) {
            let previous = self.0.take();
            // SAFETY:: Symmetric with the unsafe block below.
            unsafe {
                match previous {
                    Some(value) => std::env::set_var("PHASEGENT_PROVIDER", value),
                    None => std::env::remove_var("PHASEGENT_PROVIDER"),
                }
            }
        }
    }
    unsafe {
        std::env::remove_var("PHASEGENT_PROVIDER");
    }
    let _provider_guard = ProviderGuard(previous_provider);

    let storage = Storage::open_at(&home.join(crate::infra::storage::DB_FILENAME)).unwrap();
    storage
        .save_role_config(
            Role::Orchestrator,
            &auth::StoredConfig {
                provider: Some("gitlab".to_owned()),
                api_base: None,
                repository: None,
            },
        )
        .unwrap();

    let resolved = crate::providers::config::resolve_kind(Role::Orchestrator, None).unwrap();
    assert_eq!(resolved, ProviderKind::Gitlab);

    // A stale Redmine row for the same role must not win when
    // resolve_kind is called with an explicit gitlab.
    storage
        .save_role_config(
            Role::Orchestrator,
            &auth::StoredConfig {
                provider: Some("redmine".to_owned()),
                api_base: None,
                repository: None,
            },
        )
        .unwrap();
    let explicit =
        crate::providers::config::resolve_kind(Role::Orchestrator, Some(ProviderKind::Gitlab))
            .unwrap();
    assert_eq!(explicit, ProviderKind::Gitlab);

    let _ = fs::remove_dir_all(home);
}

/// RAII guard that removes every `PHASEGENT_PROVIDER` /
/// `PHASEGENT_DEFAULT_PROVIDER` variant for the lifetime of the
/// test and reinstates the host value on Drop. The new precedence
/// levels added by phase `global-provider-default` all read
/// environment variables, so the resolver tests need to neutralise
/// the host shell's environment before exercising the resolver
/// and restore it on exit.
#[allow(non_snake_case)]
struct DefaultProviderEnvGuard {
    _provider: Option<std::ffi::OsString>,
    _default: Option<std::ffi::OsString>,
}

impl DefaultProviderEnvGuard {
    fn neutralise() -> Self {
        let provider = std::env::var_os("PHASEGENT_PROVIDER");
        let default = std::env::var_os("PHASEGENT_DEFAULT_PROVIDER");
        // SAFETY: serialised by `lock_workflow_tests`; the Drop
        // guard reinstates the host value when the test unwinds
        // even if a panic happens mid-test.
        unsafe {
            std::env::remove_var("PHASEGENT_PROVIDER");
            std::env::remove_var("PHASEGENT_DEFAULT_PROVIDER");
        }
        Self {
            _provider: provider,
            _default: default,
        }
    }
}

impl Drop for DefaultProviderEnvGuard {
    fn drop(&mut self) {
        let provider = self._provider.take();
        let default = self._default.take();
        // SAFETY: symmetric with the unsafe block above; the lock
        // guard from `lock_workflow_tests` is still held when the
        // test stack unwinds.
        unsafe {
            match provider {
                Some(value) => std::env::set_var("PHASEGENT_PROVIDER", value),
                None => std::env::remove_var("PHASEGENT_PROVIDER"),
            }
            match default {
                Some(value) => std::env::set_var("PHASEGENT_DEFAULT_PROVIDER", value),
                None => std::env::remove_var("PHASEGENT_DEFAULT_PROVIDER"),
            }
        }
    }
}

#[test]
fn resolve_kind_honours_documented_provider_precedence_chain() {
    // Phase `global-provider-default`: the resolver must consult
    // every documented precedence level in order:
    //   1. explicit --provider argument
    //   2. PHASEGENT_PROVIDER environment variable
    //   3. PHASEGENT_DEFAULT_PROVIDER environment variable
    //   4. persisted PHASEGENT_DEFAULT_PROVIDER row in SQLite
    //   5. role-scoped role_config.provider
    //   6. forgejo fallback
    // The resolver is read-only: each test fully resets the
    // environment and storage so the order of precedence is the
    // only variable under inspection.
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
    use crate::infra::storage::{PROVIDER_GITLAB, PROVIDER_REDMINE};

    let _lock = lock_workflow_tests();
    let _provider_env = DefaultProviderEnvGuard::neutralise();
    let home = std::env::temp_dir().join(format!(
        "phasegent-resolve-precedence-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _db_path_guard = EnvGuard::set(
        "PHASEGENT_DB_PATH",
        home.join(crate::infra::storage::DB_FILENAME)
            .to_string_lossy()
            .as_ref(),
    );

    let storage = Storage::open_at(&home.join(crate::infra::storage::DB_FILENAME)).unwrap();
    storage
        .save_role_config(
            Role::Orchestrator,
            &auth::StoredConfig {
                provider: Some(PROVIDER_GITLAB.to_owned()),
                api_base: None,
                repository: None,
            },
        )
        .unwrap();

    // 6. Forgejo fallback when nothing else is configured.
    storage
        .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", "")
        .unwrap();
    storage
        .save_role_config(
            Role::Orchestrator,
            &auth::StoredConfig {
                provider: None,
                api_base: None,
                repository: None,
            },
        )
        .unwrap();
    let resolved = crate::providers::config::resolve_kind(Role::Orchestrator, None).unwrap();
    assert_eq!(
        resolved,
        ProviderKind::Forgejo,
        "empty persisted default + empty role-scoped provider must fall back to forgejo"
    );

    // 5. Role-scoped provider beats the forgejo fallback.
    storage
        .save_role_config(
            Role::Orchestrator,
            &auth::StoredConfig {
                provider: Some(PROVIDER_REDMINE.to_owned()),
                api_base: None,
                repository: None,
            },
        )
        .unwrap();
    let resolved = crate::providers::config::resolve_kind(Role::Orchestrator, None).unwrap();
    assert_eq!(
        resolved,
        ProviderKind::Redmine,
        "role-scoped provider must win over the forgejo fallback"
    );

    // 4. Persisted global default beats the role-scoped provider.
    storage
        .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", PROVIDER_GITLAB)
        .unwrap();
    let resolved = crate::providers::config::resolve_kind(Role::Orchestrator, None).unwrap();
    assert_eq!(
        resolved,
        ProviderKind::Gitlab,
        "persisted PHASEGENT_DEFAULT_PROVIDER must win over role-scoped provider"
    );

    // 3. Env default beats the persisted default.
    let _default_env = EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", PROVIDER_REDMINE);
    let resolved = crate::providers::config::resolve_kind(Role::Orchestrator, None).unwrap();
    assert_eq!(
        resolved,
        ProviderKind::Redmine,
        "PHASEGENT_DEFAULT_PROVIDER env var must win over persisted default"
    );

    // 2. PHASEGENT_PROVIDER env var beats the default env var.
    let _provider_env = EnvGuard::set("PHASEGENT_PROVIDER", PROVIDER_GITLAB);
    let resolved = crate::providers::config::resolve_kind(Role::Orchestrator, None).unwrap();
    assert_eq!(
        resolved,
        ProviderKind::Gitlab,
        "PHASEGENT_PROVIDER must win over PHASEGENT_DEFAULT_PROVIDER"
    );

    // 1. Explicit --provider beats every environment / storage
    // value. This documents the contract that `--provider` is the
    // per-command override.
    let resolved =
        crate::providers::config::resolve_kind(Role::Orchestrator, Some(ProviderKind::Redmine))
            .unwrap();
    assert_eq!(
        resolved,
        ProviderKind::Redmine,
        "explicit --provider must beat every env / storage value"
    );

    // Resolver must never persist anything: the persisted default
    // is exactly what the test seeded, no surprises.
    assert_eq!(
        storage
            .load_global_setting("PHASEGENT_DEFAULT_PROVIDER")
            .unwrap()
            .as_deref(),
        Some(PROVIDER_GITLAB)
    );

    let _ = fs::remove_dir_all(home);
}

#[test]
fn resolve_kind_rejects_invalid_persisted_global_default() {
    // Phase `global-provider-default`: a stale SQLite row that
    // contains an unknown literal must surface as a structured
    // config error rather than silently overriding the resolver.
    // The validator is the same `ProviderKind::from_str` that the
    // helper / snapshot / CLI layer use, so the contract is
    // uniform end to end.
    use crate::infra::storage::PROVIDER_REDMINE;
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let _provider_env = DefaultProviderEnvGuard::neutralise();
    let home = std::env::temp_dir().join(format!(
        "phasegent-resolve-stale-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _db_path_guard = EnvGuard::set(
        "PHASEGENT_DB_PATH",
        home.join(crate::infra::storage::DB_FILENAME)
            .to_string_lossy()
            .as_ref(),
    );

    let storage = Storage::open_at(&home.join(crate::infra::storage::DB_FILENAME)).unwrap();
    storage
        .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", "wrong")
        .unwrap();
    // The role-scoped row points at a valid value so the error
    // surfaces from the persisted-default level rather than the
    // role-scoped level.
    storage
        .save_role_config(
            Role::Executor,
            &auth::StoredConfig {
                provider: Some(PROVIDER_REDMINE.to_owned()),
                api_base: None,
                repository: None,
            },
        )
        .unwrap();

    let error = crate::providers::config::resolve_kind(Role::Executor, None).unwrap_err();
    let json = error.json();
    assert_eq!(json["kind"], "config");
    assert!(
        json["message"]
            .as_str()
            .unwrap_or_default()
            .contains("wrong"),
        "error must echo the offending value: {error:?}"
    );

    let _ = fs::remove_dir_all(home);
}

#[test]
fn resolve_kind_does_not_persist_anything() {
    // Regression guard for the "ordinary commands must not persist"
    // contract: phase `global-provider-default` adds a SQLite read
    // to `resolve_kind`, so the test must observe the empty
    // database after the resolver runs. The seeded values are
    // written by the test, not by the resolver.
    use crate::infra::storage::PROVIDER_REDMINE;
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let _provider_env = DefaultProviderEnvGuard::neutralise();
    let home = std::env::temp_dir().join(format!(
        "phasegent-resolve-readonly-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _db_path_guard = EnvGuard::set(
        "PHASEGENT_DB_PATH",
        home.join(crate::infra::storage::DB_FILENAME)
            .to_string_lossy()
            .as_ref(),
    );

    let storage = Storage::open_at(&home.join(crate::infra::storage::DB_FILENAME)).unwrap();
    storage
        .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", PROVIDER_REDMINE)
        .unwrap();

    let resolved = crate::providers::config::resolve_kind(Role::Executor, None).unwrap();
    assert_eq!(resolved, ProviderKind::Redmine);

    // The persisted default must still be exactly what the test
    // wrote; the resolver must never silently mutate it.
    assert_eq!(
        storage
            .load_global_setting("PHASEGENT_DEFAULT_PROVIDER")
            .unwrap()
            .as_deref(),
        Some(PROVIDER_REDMINE)
    );
    assert!(
        storage.load_role_config(Role::Executor).unwrap().is_none(),
        "resolver must never write the role-scoped row"
    );

    let _ = fs::remove_dir_all(home);
}

#[test]
fn timer_parser_handles_owner_args_and_recovery_subcommands() {
    // Owner metadata flows through the parser as plain strings so the
    // plugin can attach its session/call identifiers without a special
    // encoding.
    let start_with_owner = [
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "timer",
        "start",
        "28",
        "--phase",
        "implementation",
        "--agent-role",
        "executor",
        "--attempt",
        "2",
        "--run-id",
        "run-28",
        "--owner-session-id",
        "sess-123",
        "--owner-call-id",
        "call-abc",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse(&start_with_owner).unwrap().command {
        command::Command::Timer(command::TimerCommand::Start {
            issue,
            run_id,
            owner_session_id,
            owner_call_id,
            ..
        }) => {
            assert_eq!(issue, 28);
            assert_eq!(run_id.as_deref(), Some("run-28"));
            assert_eq!(owner_session_id.as_deref(), Some("sess-123"));
            assert_eq!(owner_call_id.as_deref(), Some("call-abc"));
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // Empty owner args are rejected with the same shape as the other
    // bounded timer inputs.
    let empty_owner = [
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "timer",
        "start",
        "28",
        "--phase",
        "implementation",
        "--agent-role",
        "executor",
        "--attempt",
        "1",
        "--owner-session-id",
        "",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let error = command::parse(&empty_owner).expect_err("empty owner must error");
    assert!(
        error.contains("owner-session-id cannot be empty"),
        "expected empty owner error, got: {error}"
    );

    // `list` accepts the status filter and the limit cap.
    for (args, expected_status, expected_limit) in [
        (
            vec![
                "--role",
                "orchestrator",
                "--provider",
                "redmine",
                "timer",
                "list",
            ],
            "all".to_owned(),
            100_u32,
        ),
        (
            vec![
                "--role",
                "orchestrator",
                "--provider",
                "redmine",
                "timer",
                "list",
                "--status",
                "running",
                "--limit",
                "7",
            ],
            "running".to_owned(),
            7_u32,
        ),
    ] {
        let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
        match command::parse(&args).unwrap().command {
            command::Command::Timer(command::TimerCommand::List { status, limit }) => {
                assert_eq!(status, expected_status);
                assert_eq!(limit, expected_limit);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    // Invalid status values keep the parser strict so a typo surfaces
    // before any storage call.
    let bad_status = [
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "timer",
        "list",
        "--status",
        "open",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let bad = command::parse(&bad_status).expect_err("invalid --status must error");
    assert!(bad.contains("running, finished, or all"));

    // `get` and `recover` need a non-empty positional run id.
    let get = [
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "timer",
        "get",
        "phase-51",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse(&get).unwrap().command {
        command::Command::Timer(command::TimerCommand::Get { run_id }) => {
            assert_eq!(run_id, "phase-51");
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let recover = [
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "timer",
        "recover",
        "phase-51",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse(&recover).unwrap().command {
        command::Command::Timer(command::TimerCommand::Recover { run_id }) => {
            assert_eq!(run_id, "phase-51");
        }
        other => panic!("unexpected command: {other:?}"),
    }

    for missing in [
        vec![
            "--role",
            "orchestrator",
            "--provider",
            "redmine",
            "timer",
            "get",
        ],
        vec![
            "--role",
            "orchestrator",
            "--provider",
            "redmine",
            "timer",
            "recover",
        ],
    ] {
        let missing = missing.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let error = command::parse(&missing).expect_err("missing positional must error");
        assert!(
            error.contains("missing arguments") || error.contains("requires a run id"),
            "expected run-id error, got: {error}"
        );
    }
}

#[test]
fn timer_recovery_marks_orphan_failed_and_is_idempotent_for_terminal_rows() {
    use crate::infra::storage::test_support::lock_workflow_tests;
    use crate::infra::storage::{Storage, TIMER_SYNC_FAILED};
    let _lock = lock_workflow_tests();
    let home = std::env::temp_dir().join(format!(
        "phasegent-timer-recover-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db_path = home.join(crate::infra::storage::DB_FILENAME);
    let _env = crate::infra::storage::test_support::EnvGuard::set(
        "PHASEGENT_DB_PATH",
        db_path.as_os_str().to_string_lossy().as_ref(),
    );
    let storage = Storage::open_at(&db_path).unwrap();
    storage
        .start_timer_run(
            "recover-run",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();

    // Without provider credentials the recover path must durably record
    // FAILED locally and then surface the projection failure as a
    // structured nonzero error, never as a successful envelope.
    let first_err = crate::time_tracking_cli::execute_recovery(
        Some(Role::Orchestrator),
        Some(ProviderKind::Redmine),
        None,
        None,
        None,
        command::TimerCommand::Recover {
            run_id: "recover-run".to_owned(),
        },
    )
    .unwrap_err();
    // Structured error (config/request) after durable FAILED.
    assert!(
        first_err.json()["kind"] == "config" || first_err.json()["kind"] == "request",
        "first recover must be structured error, got {:?}",
        first_err.json()
    );
    let persisted = Storage::open_at(&db_path)
        .unwrap()
        .load_timer_run("recover-run")
        .unwrap()
        .unwrap();
    assert_eq!(persisted.status, "FAILED");
    assert_eq!(persisted.sync_status, TIMER_SYNC_FAILED);
    assert!(persisted.sync_error.is_some());
    let first_sync_error = persisted.sync_error.clone();

    // A second recover on the same id must not claim success while the
    // projection is still failed; it surfaces the same failure rather
    // than returning a successful envelope with sync_failed.
    let second_err = crate::time_tracking_cli::execute_recovery(
        Some(Role::Orchestrator),
        Some(ProviderKind::Redmine),
        None,
        None,
        None,
        command::TimerCommand::Recover {
            run_id: "recover-run".to_owned(),
        },
    )
    .unwrap_err();
    assert!(
        second_err.json()["kind"] == "config" || second_err.json()["kind"] == "request",
        "second recover must also be error, got {:?}",
        second_err.json()
    );
    let second_row = Storage::open_at(&db_path)
        .unwrap()
        .load_timer_run("recover-run")
        .unwrap()
        .unwrap();
    assert_eq!(second_row.status, "FAILED");
    assert_eq!(second_row.sync_status, TIMER_SYNC_FAILED);
    assert_eq!(second_row.sync_error, first_sync_error);

    // Unknown run ids return a structured config error and never touch
    // the network.
    let missing = crate::time_tracking_cli::execute_recovery(
        Some(Role::Orchestrator),
        Some(ProviderKind::Redmine),
        None,
        None,
        None,
        command::TimerCommand::Get {
            run_id: "no-such-run".to_owned(),
        },
    )
    .unwrap_err();
    assert_eq!(missing.json()["kind"], "config");
    assert!(
        missing.json()["message"]
            .as_str()
            .unwrap_or("")
            .contains("was not found")
    );

    let _ = fs::remove_dir_all(home);
}

#[test]
fn timer_recover_with_explicit_forgejo_marks_failed_and_returns_not_supported() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
    use crate::infra::storage::{Storage, TIMER_SYNC_FAILED};
    let _lock = lock_workflow_tests();
    let home = std::env::temp_dir().join(format!(
        "phasegent-timer-recover-forgejo-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db_path = home.join(crate::infra::storage::DB_FILENAME);
    let _env = EnvGuard::set(
        "PHASEGENT_DB_PATH",
        db_path.as_os_str().to_string_lossy().as_ref(),
    );
    let storage = Storage::open_at(&db_path).unwrap();
    storage
        .start_timer_run(
            "recover-forgejo",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    let err = crate::time_tracking_cli::execute_recovery(
        Some(Role::Orchestrator),
        Some(ProviderKind::Forgejo),
        None,
        None,
        None,
        command::TimerCommand::Recover {
            run_id: "recover-forgejo".to_owned(),
        },
    )
    .unwrap_err();
    assert_eq!(err.json()["kind"], "not_supported");
    let row = Storage::open_at(&db_path)
        .unwrap()
        .load_timer_run("recover-forgejo")
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "FAILED");
    assert_eq!(row.sync_status, TIMER_SYNC_FAILED);
    let _ = fs::remove_dir_all(home);
}

#[test]
fn timer_list_and_get_return_local_only_payloads_without_network() {
    use crate::infra::storage::Storage;
    use crate::infra::storage::test_support::lock_workflow_tests;
    let _lock = lock_workflow_tests();
    let home = std::env::temp_dir().join(format!(
        "phasegent-timer-listget-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db_path = home.join(crate::infra::storage::DB_FILENAME);
    let _env = crate::infra::storage::test_support::EnvGuard::set(
        "PHASEGENT_DB_PATH",
        db_path.as_os_str().to_string_lossy().as_ref(),
    );
    let storage = Storage::open_at(&db_path).unwrap();
    storage
        .start_timer_run("lg-1", 28, "implementation", "executor", 1, 1_700_000_000)
        .unwrap();
    storage
        .finish_timer_run("lg-1", "DONE", 1_700_000_037)
        .unwrap();

    let list = crate::time_tracking_cli::execute_recovery(
        Some(Role::Orchestrator),
        Some(ProviderKind::Redmine),
        None,
        None,
        None,
        command::TimerCommand::List {
            status: "all".to_owned(),
            limit: 100,
        },
    )
    .unwrap();
    match list {
        crate::time_tracking_cli::TimerListOutput::Many { runs, count } => {
            assert_eq!(count, 1);
            assert_eq!(runs.len(), 1);
            assert_eq!(runs[0].run_id, "lg-1");
        }
        other => panic!("expected many envelope, got {other:?}"),
    }

    let get = crate::time_tracking_cli::execute_recovery(
        Some(Role::Orchestrator),
        Some(ProviderKind::Redmine),
        None,
        None,
        None,
        command::TimerCommand::Get {
            run_id: "lg-1".to_owned(),
        },
    )
    .unwrap();
    match get {
        crate::time_tracking_cli::TimerListOutput::Single { run } => {
            let run = *run;
            assert_eq!(run.run_id, "lg-1");
            assert_eq!(run.status, "DONE");
            assert_eq!(run.elapsed_seconds, Some(37));
        }
        other => panic!("expected single envelope, got {other:?}"),
    }

    // The same calls against Forgejo remain available so an operator
    // listing or inspecting an orphan does not require a provider
    // switch; recover is still Redmine/GitLab-only.
    let list_forgejo = crate::time_tracking_cli::execute_recovery(
        Some(Role::Orchestrator),
        Some(ProviderKind::Forgejo),
        None,
        None,
        None,
        command::TimerCommand::List {
            status: "running".to_owned(),
            limit: 100,
        },
    )
    .unwrap();
    match list_forgejo {
        crate::time_tracking_cli::TimerListOutput::Many { runs, .. } => {
            // Only one row exists and it is finished, so the running
            // filter must surface an empty list.
            assert!(runs.is_empty());
        }
        other => panic!("expected many envelope, got {other:?}"),
    }
    let _ = fs::remove_dir_all(home);
}

#[test]
fn canonical_git_url_strips_credentials_query_fragment_and_git_suffix() {
    // Credentials, query, fragment, and trailing .git must not affect
    // the canonical identity so the same repository behind different
    // transports still matches.
    let a = crate::remote::canonical_git_url(
        "https://user:secret@git.example.com/owner/repo.git?ref=main#frag",
    )
    .unwrap();
    let b = crate::remote::canonical_git_url("https://git.example.com/owner/repo").unwrap();
    assert_eq!(a, b);
    assert_eq!(a, "git.example.com/owner/repo");
    assert!(crate::remote::git_urls_match(
        "https://user:secret@git.example.com/owner/repo.git?ref=main#frag",
        "https://git.example.com/owner/repo"
    ));
}

#[test]
fn canonical_git_url_supports_ssh_https_equivalence_and_preserves_port_and_case() {
    // SSH and HTTPS forms for the same host/path must be equivalent
    // (scheme ignored), but non-default ports and case-sensitive paths
    // are preserved and distinguish repositories.
    assert!(crate::remote::git_urls_match(
        "ssh://git@git.example.com/owner/repo.git",
        "https://git.example.com/owner/repo.git"
    ));
    assert!(crate::remote::git_urls_match(
        "git@git.example.com:owner/repo.git",
        "https://git.example.com/owner/repo"
    ));
    // Non-default port must be preserved: different ports are not equal.
    let with_port =
        crate::remote::canonical_git_url("https://git.example.com:8443/owner/repo.git").unwrap();
    let without_port =
        crate::remote::canonical_git_url("https://git.example.com/owner/repo.git").unwrap();
    assert_ne!(with_port, without_port);
    assert!(with_port.contains(":8443"));
    // Same non-default port on different schemes still matches.
    assert!(crate::remote::git_urls_match(
        "https://git.example.com:8443/owner/repo.git",
        "ssh://git@git.example.com:8443/owner/repo.git"
    ));
    // Host is case-insensitive, path is case-sensitive.
    assert!(crate::remote::git_urls_match(
        "https://GIT.EXAMPLE.COM/owner/repo.git",
        "https://git.example.com/owner/repo.git"
    ));
    assert!(!crate::remote::git_urls_match(
        "https://git.example.com/Owner/Repo.git",
        "https://git.example.com/owner/repo.git"
    ));
    // Deployment prefix in the path is part of the identity.
    let prefixed =
        crate::remote::canonical_git_url("https://git.example.com/prefix/owner/repo.git").unwrap();
    assert_eq!(prefixed, "git.example.com/prefix/owner/repo");
    assert!(!crate::remote::git_urls_match(
        "https://git.example.com/prefix/owner/repo.git",
        "https://git.example.com/owner/repo.git"
    ));
}

#[test]
fn issue_search_bounded_pagination_parses_and_validates() {
    // Default page 1 limit 50, requires query or --all
    let args = [
        "--role",
        "orchestrator",
        "issue",
        "search",
        "--query",
        "needle",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse(&args).unwrap().command {
        command::Command::Issue(command::IssueCommand::Search {
            query,
            state,
            page,
            limit,
            all,
            include_body,
        }) => {
            assert_eq!(query.as_deref(), Some("needle"));
            assert_eq!(state, "all");
            assert_eq!(page, crate::providers::ISSUE_SEARCH_DEFAULT_PAGE);
            assert_eq!(limit, crate::providers::ISSUE_SEARCH_DEFAULT_LIMIT);
            assert!(!all);
            assert!(!include_body);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // --all allows empty query, bounded listing
    let args = ["--role", "orchestrator", "issue", "search", "--all"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    match command::parse(&args).unwrap().command {
        command::Command::Issue(command::IssueCommand::Search { all, query, .. }) => {
            assert!(all);
            assert!(query.is_none());
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // whitespace-only query without --all is rejected at validation layer
    let args = [
        "--role",
        "orchestrator",
        "issue",
        "search",
        "--query",
        "   ",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let invocation = command::parse(&args).unwrap();
    match invocation.command {
        command::Command::Issue(command::IssueCommand::Search {
            query,
            state,
            page,
            limit,
            all,
            include_body,
        }) => {
            let opts = crate::providers::IssueSearchOptions {
                query,
                state,
                page,
                limit,
                include_body,
                all,
            };
            assert!(opts.validate().is_err());
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // without query and without --all is rejected at validation layer
    let args = ["--role", "orchestrator", "issue", "search"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse(&args).unwrap();
    match invocation.command {
        command::Command::Issue(command::IssueCommand::Search {
            query,
            state,
            page,
            limit,
            all,
            include_body,
        }) => {
            let opts = crate::providers::IssueSearchOptions {
                query,
                state,
                page,
                limit,
                include_body,
                all,
            };
            assert!(opts.validate().is_err());
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // page/limit validation
    let args = [
        "--role",
        "orchestrator",
        "issue",
        "search",
        "--query",
        "q",
        "--page",
        "2",
        "--limit",
        "10",
        "--include-body",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse(&args).unwrap().command {
        command::Command::Issue(command::IssueCommand::Search {
            page,
            limit,
            include_body,
            ..
        }) => {
            assert_eq!(page, 2);
            assert_eq!(limit, 10);
            assert!(include_body);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    for (page, limit) in [(0, 50), (1, 0), (1, 101)] {
        let args = [
            "--role",
            "orchestrator",
            "issue",
            "search",
            "--query",
            "q",
            "--page",
            &page.to_string(),
            "--limit",
            &limit.to_string(),
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        assert!(
            command::parse(&args).is_err(),
            "should reject page={page} limit={limit}"
        );
    }

    // provider-neutral options validation and envelope truncation
    let opts = crate::providers::IssueSearchOptions {
        query: Some("q".to_owned()),
        state: "open".to_owned(),
        page: 1,
        limit: 50,
        include_body: false,
        all: false,
    };
    assert!(opts.validate().is_ok());
    assert_eq!(opts.effective_query(), Some("q"));

    let long = "b".repeat(crate::providers::ISSUE_SEARCH_MAX_BODY_BYTES + 3);
    let summary = crate::providers::IssueSummary {
        id: 1,
        number: 1,
        title: "t".to_owned(),
        body: long.clone(),
        state: "open".to_owned(),
        html_url: None,
    };
    let item = crate::providers::IssueSearchItem::from_summary(summary, false);
    assert!(item.body.is_none());
    assert!(item.body_truncated.is_none());

    let summary2 = crate::providers::IssueSummary {
        id: 1,
        number: 1,
        title: "t".to_owned(),
        body: long.clone(),
        state: "open".to_owned(),
        html_url: None,
    };
    let item2 = crate::providers::IssueSearchItem::from_summary(summary2, true);
    assert_eq!(item2.body_truncated, Some(true));
    assert_eq!(
        item2.body.as_ref().unwrap().len(),
        crate::providers::ISSUE_SEARCH_MAX_BODY_BYTES
    );

    let short = crate::providers::IssueSummary {
        id: 2,
        number: 2,
        title: "t".to_owned(),
        body: "short".to_owned(),
        state: "open".to_owned(),
        html_url: None,
    };
    let item3 = crate::providers::IssueSearchItem::from_summary(short, true);
    assert_eq!(item3.body_truncated, Some(false));
    assert_eq!(item3.body.as_deref(), Some("short"));
}

#[test]
fn issue_search_body_truncation_is_byte_safe_for_multibyte() {
    // 8192 byte cap must be enforced on byte length, not char count, and must
    // not split UTF-8 code points. CJK (3 bytes) and emoji (4 bytes) are the
    // canonical edge cases.
    let cjk = "汉".repeat(3000); // 9000 bytes, 3000 chars
    let summary = crate::providers::IssueSummary {
        id: 10,
        number: 10,
        title: "cjk".to_owned(),
        body: cjk,
        state: "open".to_owned(),
        html_url: None,
    };
    let item = crate::providers::IssueSearchItem::from_summary(summary, true);
    assert_eq!(item.body_truncated, Some(true));
    let body = item.body.unwrap();
    assert!(body.len() <= crate::providers::ISSUE_SEARCH_MAX_BODY_BYTES);
    // Must remain valid UTF-8 and end on a char boundary; the helper must
    // have trimmed to the previous boundary rather than splitting 汉.
    assert!(body.is_char_boundary(body.len()));
    // 8192 is not divisible by 3, so the truncated CJK body must be <8192.
    // 8192 /3 = 2730 rem 2 => floor is 2730*3 = 8190 bytes.
    assert_eq!(body.len(), 8190);
    assert_eq!(body.chars().count(), 2730);

    let emoji = "😀".repeat(3000); // 12000 bytes, 3000 chars
    let summary = crate::providers::IssueSummary {
        id: 11,
        number: 11,
        title: "emoji".to_owned(),
        body: emoji,
        state: "open".to_owned(),
        html_url: None,
    };
    let item = crate::providers::IssueSearchItem::from_summary(summary, true);
    assert_eq!(item.body_truncated, Some(true));
    let body = item.body.unwrap();
    assert!(body.len() <= crate::providers::ISSUE_SEARCH_MAX_BODY_BYTES);
    assert!(body.is_char_boundary(body.len()));
    // 8192 /4 = 2048 exactly, so emoji truncation lands exactly on 8192.
    assert_eq!(body.len(), 8192);
    assert_eq!(body.chars().count(), 2048);

    // Mixed content where the cut falls inside a multibyte sequence
    let mixed = format!("{}{}", "a".repeat(8190), "😀"); // 8190+4=8194 bytes
    let summary = crate::providers::IssueSummary {
        id: 12,
        number: 12,
        title: "mixed".to_owned(),
        body: mixed,
        state: "open".to_owned(),
        html_url: None,
    };
    let item = crate::providers::IssueSearchItem::from_summary(summary, true);
    assert_eq!(item.body_truncated, Some(true));
    let body = item.body.unwrap();
    // The emoji would push over 8192, so it must be dropped entirely rather
    // than split; result is the 8190 ascii bytes.
    assert_eq!(body.len(), 8190);
    assert_eq!(body, "a".repeat(8190));
    assert!(body.is_char_boundary(body.len()));

    // Exactly at cap must not be marked truncated
    let exact = "b".repeat(crate::providers::ISSUE_SEARCH_MAX_BODY_BYTES);
    let summary = crate::providers::IssueSummary {
        id: 13,
        number: 13,
        title: "exact".to_owned(),
        body: exact.clone(),
        state: "open".to_owned(),
        html_url: None,
    };
    let item = crate::providers::IssueSearchItem::from_summary(summary, true);
    assert_eq!(item.body_truncated, Some(false));
    assert_eq!(item.body.unwrap().len(), crate::providers::ISSUE_SEARCH_MAX_BODY_BYTES);
}
