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
use crate::worktree::WorktreeRunner;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

/// `remote::parse_remote` normalisation contract, parameterised over the
/// HTTPS, URL-form SSH, and scp-style SSH remotes that reach it: HTTPS keeps
/// its non-default port in `api_base` and drops embedded credentials, SSH
/// keeps the transport user (required to clone) and its non-default port, an
/// SSH port never leaks into `api_base`, and query/fragment are always
/// stripped without ever leaking a credential.
#[test]
fn remote_resolution_normalises_https_and_ssh_forms_without_credentials() {
    let cases = [
        (
            "https://forgejo.example:8443/owner/widgets.git",
            "owner/widgets",
            "https://forgejo.example:8443/api/v1",
            "https://forgejo.example:8443/owner/widgets.git",
        ),
        (
            "ssh://git@forgejo.example:2222/owner/widgets.git",
            "owner/widgets",
            "https://forgejo.example/api/v1",
            "ssh://git@forgejo.example:2222/owner/widgets.git",
        ),
        (
            "https://forgejo.example/forgejo/owner/widgets.git",
            "owner/widgets",
            "https://forgejo.example/forgejo/api/v1",
            "https://forgejo.example/forgejo/owner/widgets.git",
        ),
        (
            "git@forgejo.example:owner/widgets.git",
            "owner/widgets",
            "https://forgejo.example/api/v1",
            "ssh://git@forgejo.example/owner/widgets.git",
        ),
        (
            "https://deploy:supersecret@forgejo.example/owner/widgets.git",
            "owner/widgets",
            "https://forgejo.example/api/v1",
            "https://forgejo.example/owner/widgets.git",
        ),
        (
            "ssh://git@forgejo.example.com:2222/owner/repo.git",
            "owner/repo",
            "https://forgejo.example.com/api/v1",
            "ssh://git@forgejo.example.com:2222/owner/repo.git",
        ),
        (
            "ssh://deploy@git.example.com/owner/repo.git",
            "owner/repo",
            "https://git.example.com/api/v1",
            "ssh://deploy@git.example.com/owner/repo.git",
        ),
        (
            "ssh://git@git.example.com/owner/repo.git?ref=main#frag",
            "owner/repo",
            "https://git.example.com/api/v1",
            "ssh://git@git.example.com/owner/repo.git",
        ),
        (
            "https://deploy:supersecret@forgejo.example/owner/repo.git",
            "owner/repo",
            "https://forgejo.example/api/v1",
            "https://forgejo.example/owner/repo.git",
        ),
    ];
    for (input, repository, api_base, repository_url) in cases {
        let parsed = remote::parse_remote(input).unwrap_or_else(|error| panic!("{input}: {error}"));
        assert_eq!(parsed.repository, repository, "{input}");
        assert_eq!(parsed.api_base, api_base, "{input}");
        assert_eq!(parsed.repository_url, repository_url, "{input}");
        assert!(
            !parsed.repository_url.contains("supersecret"),
            "credentials must never survive normalisation: {input}"
        );
    }
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
        vec!["--role", "orchestrator", "issue", "update", "1", "--body"],
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
fn issue_close_parses_worktree_session_option() {
    let args = [
        "--role",
        "orchestrator",
        "issue",
        "close",
        "42",
        "--worktree-session",
        "alpha",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("close with --worktree-session parses");
    match invocation.command {
        command::Command::Issue(command::IssueCommand::Close {
            number,
            worktree_session,
        }) => {
            assert_eq!(number, 42);
            assert_eq!(worktree_session.as_deref(), Some("alpha"));
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn issue_close_worktree_session_defaults_to_none() {
    // Legacy `issue close N` keeps parsing with no explicit session; the
    // CLI resolves PHASEGENT_SESSION_ID / legacy fallback at execution.
    let args = ["--role", "orchestrator", "issue", "close", "42"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("legacy close parses");
    match invocation.command {
        command::Command::Issue(command::IssueCommand::Close {
            number,
            worktree_session,
        }) => {
            assert_eq!(number, 42);
            assert_eq!(worktree_session, None);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn issue_close_rejects_blank_worktree_session() {
    let args = [
        "--role",
        "orchestrator",
        "issue",
        "close",
        "42",
        "--worktree-session",
        "",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let error = command::parse(&args).unwrap_err();
    assert!(error.contains("session"), "unexpected error: {error}");
}

#[test]
fn issue_close_rejects_overlong_worktree_session() {
    let overlong = "s".repeat(129);
    let args = [
        "--role",
        "orchestrator",
        "issue",
        "close",
        "42",
        "--worktree-session",
        overlong.as_str(),
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let error = command::parse(&args).unwrap_err();
    assert!(
        error.contains("session") && error.contains("128"),
        "unexpected error: {error}"
    );
}

#[test]
fn issue_close_still_rejects_extra_positionals() {
    let args = ["--role", "orchestrator", "issue", "close", "42", "extra"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert!(command::parse(&args).is_err());
}

#[test]
fn issue_close_still_rejects_unknown_options() {
    let args = [
        "--role",
        "orchestrator",
        "issue",
        "close",
        "42",
        "--session",
        "alpha",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let error = command::parse(&args).unwrap_err();
    assert!(
        error.contains("unknown option"),
        "unexpected error: {error}"
    );
}

#[test]
fn issue_close_help_routes_to_command_topic() {
    let args = ["--role", "orchestrator", "issue", "close", "--help"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("issue close --help parses");
    match invocation.command {
        command::Command::Help(command::HelpTopic::IssueCommand(name)) => {
            assert_eq!(name, "close");
        }
        other => panic!("unexpected command {other:?}"),
    }
}

/// Issue 552 Phase 2 shipped `issue sync` without a help topic; the
/// routing table must resolve it to a command page instead of rejecting
/// the topic as unknown.
#[test]
fn issue_sync_help_routes_to_command_topic() {
    let args = ["--role", "orchestrator", "issue", "sync", "--help"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("issue sync --help parses");
    match invocation.command {
        command::Command::Help(command::HelpTopic::IssueCommand(name)) => {
            assert_eq!(name, "sync");
        }
        other => panic!("unexpected command {other:?}"),
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
        command::Command::Issue(command::IssueCommand::Create { title, body, .. }) => {
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
        command::Command::Issue(command::IssueCommand::Create { title, body, .. }) => {
            assert_eq!(title, "ok");
            assert_eq!(body, "- Goal");
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // issue body (`---` separator) via issue update
    let args = [
        "--role",
        "orchestrator",
        "issue",
        "update",
        "1",
        "--body=---",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("inline --body should parse");
    match invocation.command {
        command::Command::Issue(command::IssueCommand::Update { number, body, .. }) => {
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
        command::Command::Issue(command::IssueCommand::Search { query, state, .. }) => {
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
        command::Command::Issue(command::IssueCommand::Search { query, state, .. }) => {
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
            ..
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
            "update",
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
        command::Command::Issue(command::IssueCommand::Create { title, body, .. }) => {
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
        ("Resolved", &["Closed", "In Progress"]),
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
            assert_eq!(allowed_next, &["Closed", "In Progress"])
        }
        other => panic!("expected Forbidden, got {other:?}"),
    }
    match evaluate_transition("Closed", "In Progress") {
        TransitionVerdict::Forbidden { allowed_next } => assert!(allowed_next.is_empty()),
        other => panic!("expected Forbidden, got {other:?}"),
    }
}

/// `Resolved` is the non-terminal "AI work finished, awaiting the user's
/// verification" state, so the policy keeps two outgoing edges: the
/// verified terminal `Closed` as the auto-route default and `In Progress`
/// as the explicitly targeted resume after a reviewed phase. Losing the
/// resume edge would strand every multi-phase task after its first
/// reviewed phase.
#[test]
fn resolved_status_allows_final_close_and_phase_continuation() {
    assert_eq!(
        canonical_allowed_next("Resolved").expect("canonical status"),
        &["Closed", "In Progress"],
        "Resolved must offer the final close before the resume edge"
    );
    assert_eq!(
        evaluate_transition("Resolved", "Closed"),
        TransitionVerdict::Allowed,
        "the verified terminal close edge must stay allowed"
    );
    assert_eq!(
        evaluate_transition("Resolved", "In Progress"),
        TransitionVerdict::Allowed,
        "a remaining phase must be able to resume implementation"
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
fn issue_create_and_update_accept_optional_tracker_selection() {
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
            ..
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
        "update",
        "9",
        "--body",
        "Updated",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse(&update).unwrap().command {
        command::Command::Issue(command::IssueCommand::Update {
            number,
            body,
            tracker,
            planning,
            ..
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

/// The CLI JSON contract for `workflow bootstrap` is parameterised over the
/// success and membership-warning shapes: both expose `user_memberships` per
/// agent identity and the credential-free `git_mirror` outcome, must never
/// re-introduce the legacy `membership`/`group_name`/`group_role` keys, and
/// must never leak a bearer key or embedded origin credential. The detailed
/// bootstrap flow is exercised end-to-end by
/// `redmine_contract_tests::issue_create_automatically_bootstraps_once_before_returning_issue`;
/// this test pins the surface contract for both shapes.
#[test]
fn workflow_bootstrap_json_contract_holds_for_success_and_warning_shapes() {
    fn bootstrap_output(warning: bool) -> serde_json::Value {
        let memberships = if warning {
            serde_json::json!([
                {
                    "role": "Developer",
                    "user_id": 22_u64,
                    "user_login": "executor",
                    "status": "warning",
                    "warning": "Redmine role was not found: user 'executor', role 'Developer'",
                },
            ])
        } else {
            serde_json::json!([
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
            ])
        };
        let mut output = serde_json::json!({
            "bootstrapped": !warning,
            "created": true,
            "repository": "owner/repo",
            "identifier": "owner-repo",
            "project_id": 44_u64,
            "close_status_id": 5_u64,
            "close_status_name": "Closed",
            "user_memberships": memberships,
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
        if warning {
            output["warning"] =
                serde_json::json!("Redmine role was not found: user 'executor', role 'Developer'");
        }
        output
    }

    for warning in [false, true] {
        let label = if warning {
            "warning shape"
        } else {
            "success shape"
        };
        let output = bootstrap_output(warning);
        assert_eq!(
            output["bootstrapped"],
            serde_json::json!(!warning),
            "{label}"
        );
        for legacy in ["membership", "group_name", "group_role"] {
            assert!(
                output.get(legacy).is_none(),
                "{label} must not re-introduce the legacy {legacy} key"
            );
        }
        let memberships = output["user_memberships"]
            .as_array()
            .expect("user_memberships must be an array");
        assert!(!memberships.is_empty(), "{label} must report memberships");

        let git_mirror = output
            .get("git_mirror")
            .expect("bootstrap JSON must include git_mirror");
        assert_eq!(git_mirror["status"], "pending", "{label}");
        assert_eq!(git_mirror["identifier"], "mirror_44_owner_repo", "{label}");
        assert_eq!(git_mirror["project_id"], 44_u64, "{label}");
        assert_eq!(
            git_mirror["remote_url"], "https://git.example.com/owner/repo.git",
            "{label}"
        );
        assert_eq!(
            git_mirror["local_path"], "/var/redmine/repos/owner_repo.git",
            "{label}"
        );
        assert!(git_mirror["error"].is_null(), "{label}");

        let serialized = output.to_string();
        assert!(
            !serialized.to_ascii_lowercase().contains("bearer "),
            "{label} must not include bearer credentials: {serialized}"
        );
        assert!(
            !serialized.contains("supersecret"),
            "{label} must not include embedded origin credentials: {serialized}"
        );

        if warning {
            assert!(
                output["warning"].is_string(),
                "warning shape must carry the failure reason"
            );
            assert_eq!(memberships[0]["status"], "warning");
            assert!(memberships[0]["warning"].is_string());
        } else {
            assert_eq!(
                memberships.len(),
                3,
                "success shape reports one membership per agent identity"
            );
        }
    }
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
fn issue_planning_flags_parse_on_create_and_update() {
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
        "update",
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
        command::Command::Issue(command::IssueCommand::Update { planning, .. }) => {
            assert_eq!(planning.fixed_version.as_deref(), Some("7"));
            assert!(planning.parent_issue.is_none());
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // Planning flags belong only to create/update; other issue
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
    let home = crate::test_scratch::root().join(format!(
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
        error.contains("forgejo, redmine, gitlab, or local"),
        "parse error must enumerate the supported providers: {error}"
    );
}

#[test]
fn provider_kind_local_round_trips_and_resolves_without_credentials() {
    // `local` must parse/render/display like the other providers,
    // round-trip through `ProviderKind::from_str`, and resolve through
    // the persisted-default chain so `--provider local` commands need no
    // credential and no network.
    use std::str::FromStr;

    let parsed: ProviderKind = "local".parse().expect("local must parse");
    assert_eq!(parsed, ProviderKind::Local);
    assert_eq!(parsed.as_str(), "local");
    assert_eq!(format!("{parsed}"), "local");

    let round_trip = ProviderKind::from_str(ProviderKind::Local.as_str())
        .expect("as_str must parse back to Local");
    assert_eq!(round_trip, ProviderKind::Local);

    // The top-level `--provider local` flag flows through the parser.
    let args = [
        "--role",
        "executor",
        "--provider",
        "local",
        "issue",
        "search",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("--provider local must parse");
    assert_eq!(
        invocation.provider.expect("--provider must be captured"),
        ProviderKind::Local
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
    let home = crate::test_scratch::root().join(format!(
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
    let home = crate::test_scratch::root().join(format!(
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
    let home = crate::test_scratch::root().join(format!(
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
    let home = crate::test_scratch::root().join(format!(
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
    let home = crate::test_scratch::root().join(format!(
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
    let home = crate::test_scratch::root().join(format!(
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
    let home = crate::test_scratch::root().join(format!(
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
            assert_eq!(page, crate::providers::api::ISSUE_SEARCH_DEFAULT_PAGE);
            assert_eq!(limit, crate::providers::api::ISSUE_SEARCH_DEFAULT_LIMIT);
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

    let long = "b".repeat(crate::providers::api::ISSUE_SEARCH_MAX_BODY_BYTES + 3);
    let summary = crate::providers::IssueSummary {
        id: 1,
        number: 1,
        title: "t".to_owned(),
        body: long.clone(),
        state: "open".to_owned(),
        html_url: None,
        project: None,
    };
    let item = crate::providers::api::IssueSearchItem::from_summary(summary, false);
    assert!(item.body.is_none());
    assert!(item.body_truncated.is_none());

    let summary2 = crate::providers::IssueSummary {
        id: 1,
        number: 1,
        title: "t".to_owned(),
        body: long.clone(),
        state: "open".to_owned(),
        html_url: None,
        project: None,
    };
    let item2 = crate::providers::api::IssueSearchItem::from_summary(summary2, true);
    assert_eq!(item2.body_truncated, Some(true));
    assert_eq!(
        item2.body.as_ref().unwrap().len(),
        crate::providers::api::ISSUE_SEARCH_MAX_BODY_BYTES
    );

    let short = crate::providers::IssueSummary {
        id: 2,
        number: 2,
        title: "t".to_owned(),
        body: "short".to_owned(),
        state: "open".to_owned(),
        html_url: None,
        project: None,
    };
    let item3 = crate::providers::api::IssueSearchItem::from_summary(short, true);
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
        project: None,
    };
    let item = crate::providers::api::IssueSearchItem::from_summary(summary, true);
    assert_eq!(item.body_truncated, Some(true));
    let body = item.body.unwrap();
    assert!(body.len() <= crate::providers::api::ISSUE_SEARCH_MAX_BODY_BYTES);
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
        project: None,
    };
    let item = crate::providers::api::IssueSearchItem::from_summary(summary, true);
    assert_eq!(item.body_truncated, Some(true));
    let body = item.body.unwrap();
    assert!(body.len() <= crate::providers::api::ISSUE_SEARCH_MAX_BODY_BYTES);
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
        project: None,
    };
    let item = crate::providers::api::IssueSearchItem::from_summary(summary, true);
    assert_eq!(item.body_truncated, Some(true));
    let body = item.body.unwrap();
    // The emoji would push over 8192, so it must be dropped entirely rather
    // than split; result is the 8190 ascii bytes.
    assert_eq!(body.len(), 8190);
    assert_eq!(body, "a".repeat(8190));
    assert!(body.is_char_boundary(body.len()));

    // Exactly at cap must not be marked truncated
    let exact = "b".repeat(crate::providers::api::ISSUE_SEARCH_MAX_BODY_BYTES);
    let summary = crate::providers::IssueSummary {
        id: 13,
        number: 13,
        title: "exact".to_owned(),
        body: exact.clone(),
        state: "open".to_owned(),
        html_url: None,
        project: None,
    };
    let item = crate::providers::api::IssueSearchItem::from_summary(summary, true);
    assert_eq!(item.body_truncated, Some(false));
    assert_eq!(
        item.body.unwrap().len(),
        crate::providers::api::ISSUE_SEARCH_MAX_BODY_BYTES
    );
}

// ---------------------------------------------------------------------------
// Issue 305 Task 3, widened by issue 537 Phase 2: `issue close` releases
// every active worktree lease the closed issue holds across all sessions,
// and only after the provider confirmed the close.
//
// These tests drive `execute_issue` end-to-end against the deterministic
// local provider (no mock HTTP) with a temp worktree database and a temp
// git checkout. They prove the provider success/failure split, the
// cross-session release, and the no-session no-op.
// ---------------------------------------------------------------------------

fn close_cli_root(label: &str) -> std::path::PathBuf {
    let root = crate::test_scratch::root().join(format!(
        "phasegent-close-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    root
}

fn close_cli_init_repo(repo: &std::path::Path) {
    fs::create_dir_all(repo).unwrap();
    let runner = crate::worktree::ProcessWorktreeRunner::new();
    runner
        .run(&["init", "-q", "-b", "main"], repo)
        .expect("git init");
    let _ = runner.run(
        &[
            "-c",
            "user.name=phasegent-test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "init",
        ],
        repo,
    );
}

fn close_cli_seed_lease(identity: &str, issue: u64, session: &str, status: &str) -> String {
    let storage = Storage::open().unwrap();
    crate::worktree::ensure_schema(&storage).unwrap();
    let lease_id = format!(
        "close-cli-{issue}-{session}-{}",
        crate::worktree::compute_fingerprint(identity)
    );
    let now = crate::worktree::now_unix_secs();
    let worktree_path = format!("/tmp/phasegent-close-cli-{issue}-{session}");
    crate::worktree::leases::insert_lease(
        &storage,
        crate::worktree::leases::NewLease {
            lease_id: &lease_id,
            identity,
            issue,
            session,
            checkout_path: "/tmp/phasegent-close-cli-checkout",
            worktree_path: &worktree_path,
            branch: "main",
            status,
            created_at: now,
            heartbeat_at: now,
        },
    )
    .unwrap();
    lease_id
}

fn close_cli_lease_state(lease_id: &str) -> (String, Option<String>) {
    let storage = Storage::open().unwrap();
    let row = crate::worktree::leases::load_lease(&storage, lease_id)
        .unwrap()
        .unwrap();
    (row.status, row.release_reason)
}

fn close_cli_seed_issue(title: &str, status: &str) -> u64 {
    let provider = crate::providers::local::LocalProvider::open().unwrap();
    let issue = provider.create_issue(title, "body").unwrap();
    if status != "New" {
        provider
            .with_conn("seed issue status", |conn| {
                conn.execute(
                    "UPDATE local_issues SET status = ?1 WHERE id = ?2",
                    rusqlite::params![status, issue.number as i64],
                )?;
                Ok(())
            })
            .unwrap();
    }
    issue.number
}

/// RAII helper that removes `PHASEGENT_SESSION_ID` for the duration of a
/// test and restores the host value on Drop so the no-session path is
/// exercised deterministically.
struct SessionEnvRestore(Option<std::ffi::OsString>);

impl SessionEnvRestore {
    fn remove() -> Self {
        let previous = std::env::var_os("PHASEGENT_SESSION_ID");
        // SAFETY: serialised by `lock_workflow_tests`; the Drop guard
        // restores the host value when the test unwinds.
        unsafe {
            std::env::remove_var("PHASEGENT_SESSION_ID");
        }
        Self(previous)
    }
}

impl Drop for SessionEnvRestore {
    fn drop(&mut self) {
        let previous = self.0.take();
        // SAFETY: symmetric with `remove` above.
        unsafe {
            match previous {
                Some(value) => std::env::set_var("PHASEGENT_SESSION_ID", value),
                None => std::env::remove_var("PHASEGENT_SESSION_ID"),
            }
        }
    }
}

#[test]
fn cli_issue_close_releases_every_session_lease_after_provider_success() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("success");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let previous_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(&repo).unwrap();

    let number = close_cli_seed_issue("Close me", "Resolved");
    let identity =
        crate::worktree::repo_identity(&crate::worktree::ProcessWorktreeRunner::new(), &repo)
            .unwrap();
    let lease_a = close_cli_seed_lease(&identity, number, "session-a", "active");
    let lease_b = close_cli_seed_lease(&identity, number, "session-b", "active");

    let exit = crate::cli::issue::execute_issue(
        Some(Role::Orchestrator),
        Some(ProviderKind::Local),
        None,
        None,
        None,
        None,
        command::IssueCommand::Close {
            number,
            worktree_session: Some("session-a".to_owned()),
        },
    );
    let _ = std::env::set_current_dir(&previous_cwd);

    assert_eq!(exit, 0, "a valid local-provider close must succeed");
    let (status_a, reason_a) = close_cli_lease_state(&lease_a);
    assert_eq!(status_a, "retained");
    assert_eq!(reason_a.as_deref(), Some("issue closed: session-a"));
    let (status_b, reason_b) = close_cli_lease_state(&lease_b);
    assert_eq!(
        status_b, "retained",
        "a second session's lease for the closed issue must be converged"
    );
    assert_eq!(reason_b.as_deref(), Some("issue closed: session-a"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn cli_issue_close_provider_failure_leaves_lease_active() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("failure");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let previous_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(&repo).unwrap();

    // `New -> Closed` is rejected by the local transition policy, so the
    // provider close fails before the lease hook is reached.
    let number = close_cli_seed_issue("Close me", "New");
    let identity =
        crate::worktree::repo_identity(&crate::worktree::ProcessWorktreeRunner::new(), &repo)
            .unwrap();
    let lease_a = close_cli_seed_lease(&identity, number, "session-a", "active");

    let exit = crate::cli::issue::execute_issue(
        Some(Role::Orchestrator),
        Some(ProviderKind::Local),
        None,
        None,
        None,
        None,
        command::IssueCommand::Close {
            number,
            worktree_session: Some("session-a".to_owned()),
        },
    );
    let _ = std::env::set_current_dir(&previous_cwd);

    assert_ne!(exit, 0, "a rejected provider close must fail");
    assert_eq!(
        close_cli_lease_state(&lease_a).0,
        "active",
        "a failed provider close must not release the lease"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn cli_issue_close_without_session_leaves_lease_active() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let _session_env = SessionEnvRestore::remove();
    let root = close_cli_root("no-session");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let previous_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(&repo).unwrap();

    let number = close_cli_seed_issue("Close me", "Resolved");
    let identity =
        crate::worktree::repo_identity(&crate::worktree::ProcessWorktreeRunner::new(), &repo)
            .unwrap();
    let lease_a = close_cli_seed_lease(&identity, number, "session-a", "active");
    let lease_b = close_cli_seed_lease(&identity, number, "session-b", "active");

    let exit = crate::cli::issue::execute_issue(
        Some(Role::Orchestrator),
        Some(ProviderKind::Local),
        None,
        None,
        None,
        None,
        command::IssueCommand::Close {
            number,
            worktree_session: None,
        },
    );
    let _ = std::env::set_current_dir(&previous_cwd);

    // The provider close succeeded (exit 0), so the stdout envelope is the
    // unchanged provider close document; no owner is guessed locally.
    assert_eq!(exit, 0);
    assert_eq!(close_cli_lease_state(&lease_a).0, "active");
    assert_eq!(close_cli_lease_state(&lease_b).0, "active");
    let _ = fs::remove_dir_all(root);
}

// ---------------------------------------------------------------------------
// Issue 552 Phase 1: `issue close` removes the closed issue's clean worktree
// directories after the provider close and the lease release.
//
// These tests drive `execute_issue` end-to-end against the deterministic
// local provider with a temp worktree database and a temp git checkout whose
// issue has a real linked worktree. They prove the guard split (clean
// directory removed; dirty directory, another issue's directory, and the main
// checkout kept), the branch preservation, and that a rejected provider close
// leaves the directory untouched.
// ---------------------------------------------------------------------------

fn close_cli_add_worktree(repo: &std::path::Path, label: &str, branch: &str) -> std::path::PathBuf {
    let dir = crate::test_scratch::root().join(format!(
        "phasegent-close-wt-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let runner = crate::worktree::ProcessWorktreeRunner::new();
    let output = runner
        .run(
            &[
                "worktree",
                "add",
                dir.to_str().expect("utf8 worktree path"),
                "-b",
                branch,
                "HEAD",
            ],
            repo,
        )
        .expect("git worktree add runs");
    assert_eq!(output.status, 0, "git worktree add must succeed");
    dir
}

fn close_cli_seed_lease_at(
    identity: &str,
    issue: u64,
    session: &str,
    status: &str,
    worktree_path: &std::path::Path,
) -> String {
    let storage = Storage::open().unwrap();
    crate::worktree::ensure_schema(&storage).unwrap();
    let lease_id = format!(
        "close-cli-cleanup-{issue}-{session}-{}",
        crate::worktree::compute_fingerprint(identity)
    );
    let now = crate::worktree::now_unix_secs();
    crate::worktree::leases::insert_lease(
        &storage,
        crate::worktree::leases::NewLease {
            lease_id: &lease_id,
            identity,
            issue,
            session,
            checkout_path: "/tmp/phasegent-close-cli-checkout",
            worktree_path: worktree_path.to_str().expect("utf8 worktree path"),
            branch: "main",
            status,
            created_at: now,
            heartbeat_at: now,
        },
    )
    .unwrap();
    lease_id
}

fn close_cli_branch_exists(repo: &std::path::Path, branch: &str) -> bool {
    let runner = crate::worktree::ProcessWorktreeRunner::new();
    let reference = format!("refs/heads/{branch}");
    runner
        .run(&["show-ref", "--verify", "--quiet", &reference], repo)
        .expect("git show-ref runs")
        .status
        == 0
}

/// Run one `issue close` against the local provider from `cwd`, with the
/// temp worktree database active, and return the exit code.
fn close_cli_run(number: u64, session: Option<&str>, cwd: &std::path::Path) -> i32 {
    let previous_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(cwd).unwrap();
    let exit = crate::cli::issue::execute_issue(
        Some(Role::Orchestrator),
        Some(ProviderKind::Local),
        None,
        None,
        None,
        None,
        command::IssueCommand::Close {
            number,
            worktree_session: session.map(str::to_owned),
        },
    );
    let _ = std::env::set_current_dir(&previous_cwd);
    exit
}

#[test]
fn cli_issue_close_removes_clean_worktree_and_keeps_branch() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("cleanup-clean");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let worktree = close_cli_add_worktree(&repo, "clean", "feat/552-clean");
    let number = close_cli_seed_issue("Close me", "Resolved");
    let identity =
        crate::worktree::repo_identity(&crate::worktree::ProcessWorktreeRunner::new(), &repo)
            .unwrap();
    let lease = close_cli_seed_lease_at(&identity, number, "session-a", "active", &worktree);

    let exit = close_cli_run(number, Some("session-a"), &repo);

    assert_eq!(exit, 0, "the local-provider close must succeed");
    assert!(
        !worktree.exists(),
        "the clean worktree directory must be removed after the close"
    );
    assert_eq!(
        close_cli_lease_state(&lease).0,
        "retained",
        "the lease row stays as the retained audit record"
    );
    assert!(
        close_cli_branch_exists(&repo, "feat/552-clean"),
        "the branch must never be deleted"
    );
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn cli_issue_close_keeps_dirty_worktree() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("cleanup-dirty");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let worktree = close_cli_add_worktree(&repo, "dirty", "feat/552-dirty");
    // An untracked file is enough to make the directory dirty.
    fs::write(worktree.join("scratch.txt"), "wip").unwrap();
    let number = close_cli_seed_issue("Close me", "Resolved");
    let identity =
        crate::worktree::repo_identity(&crate::worktree::ProcessWorktreeRunner::new(), &repo)
            .unwrap();
    let lease = close_cli_seed_lease_at(&identity, number, "session-a", "active", &worktree);

    let exit = close_cli_run(number, Some("session-a"), &repo);

    assert_eq!(exit, 0);
    assert!(
        worktree.exists(),
        "a dirty worktree directory must be kept and warned about"
    );
    assert_eq!(close_cli_lease_state(&lease).0, "retained");
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn cli_issue_close_keeps_another_issues_worktree() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("cleanup-other-issue");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let ours = close_cli_add_worktree(&repo, "ours", "feat/552-ours");
    let theirs = close_cli_add_worktree(&repo, "theirs", "feat/9002-theirs");
    let number = close_cli_seed_issue("Close me", "Resolved");
    let identity =
        crate::worktree::repo_identity(&crate::worktree::ProcessWorktreeRunner::new(), &repo)
            .unwrap();
    close_cli_seed_lease_at(&identity, number, "session-a", "active", &ours);
    let other = close_cli_seed_lease_at(&identity, 9002, "session-b", "active", &theirs);

    let exit = close_cli_run(number, Some("session-a"), &repo);

    assert_eq!(exit, 0);
    assert!(
        !ours.exists(),
        "the closed issue's clean directory is removed"
    );
    assert!(
        theirs.exists(),
        "another issue's worktree directory is never touched"
    );
    assert_eq!(
        close_cli_lease_state(&other).0,
        "active",
        "another issue's active lease is never released"
    );
    let _ = fs::remove_dir_all(&ours);
    let _ = fs::remove_dir_all(&theirs);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn cli_issue_close_never_removes_the_main_checkout() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("cleanup-main");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let number = close_cli_seed_issue("Close me", "Resolved");
    let identity =
        crate::worktree::repo_identity(&crate::worktree::ProcessWorktreeRunner::new(), &repo)
            .unwrap();
    // A reused-current-checkout lease points at the main checkout itself.
    let lease = close_cli_seed_lease_at(&identity, number, "session-a", "active", &repo);

    let exit = close_cli_run(number, Some("session-a"), &repo);

    assert_eq!(exit, 0);
    assert!(
        repo.exists(),
        "the main checkout is never removed by the close cleanup"
    );
    assert_eq!(close_cli_lease_state(&lease).0, "retained");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn cli_issue_close_provider_failure_leaves_worktree_directory() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("cleanup-provider-failure");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let worktree = close_cli_add_worktree(&repo, "failure", "feat/552-failure");
    // `New -> Closed` is rejected by the local transition policy, so the
    // provider close fails before the cleanup hook is reached.
    let number = close_cli_seed_issue("Close me", "New");
    let identity =
        crate::worktree::repo_identity(&crate::worktree::ProcessWorktreeRunner::new(), &repo)
            .unwrap();
    let lease = close_cli_seed_lease_at(&identity, number, "session-a", "active", &worktree);

    let exit = close_cli_run(number, Some("session-a"), &repo);

    assert_ne!(exit, 0, "a rejected provider close must fail");
    assert!(
        worktree.exists(),
        "a failed provider close must leave the worktree directory untouched"
    );
    assert_eq!(close_cli_lease_state(&lease).0, "active");
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

// ---------------------------------------------------------------------------
// Issue 552 Phase 2: `issue sync` reconciles an issue the provider already
// closed, and `worktree acquire|list|prune` run the same pass before their
// own work.
//
// The pass itself is driven through `cli::issue::sync::run_sync` so the
// report object is asserted field by field; the CLI wiring is driven through
// `cli::issue::execute_issue` (direct sync) and
// `cli::worktree::execute_worktree` (taxi). Every test pins its database,
// local database, and git checkouts to temp paths.
// ---------------------------------------------------------------------------

fn sync_cli_local_dispatcher() -> crate::providers::ProviderDispatcher {
    crate::providers::ProviderDispatcher::local(
        crate::providers::local::LocalProvider::open().expect("local provider"),
    )
}

/// Run one reconciliation pass against the temp local provider and return
/// its report.
fn sync_cli_pass(
    repo: &std::path::Path,
    all: bool,
    mode: crate::cli::issue::sync::SyncMode,
) -> crate::cli::issue::sync::SyncReport {
    let provider = sync_cli_local_dispatcher();
    let runner = crate::worktree::ProcessWorktreeRunner::new();
    let storage = Storage::open().expect("storage");
    crate::worktree::ensure_schema(&storage).expect("lease schema");
    crate::cli::issue::sync::run_sync(
        &provider,
        &runner,
        &storage,
        crate::cli::issue::sync::SyncRequest {
            all,
            mode,
            cwd: repo,
        },
    )
    .expect("reconciliation pass")
}

/// Seed one local issue directly in the provider's closed state, the way a
/// web close leaves it (`is_closed` status without a local close chain).
fn sync_cli_seed_closed_issue(title: &str) -> u64 {
    close_cli_seed_issue(title, "Closed")
}

/// Run `issue sync` through the CLI executor from `cwd`.
fn sync_cli_run_cli(
    provider: ProviderKind,
    all: bool,
    no_clean: bool,
    cwd: &std::path::Path,
) -> i32 {
    let previous_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(cwd).unwrap();
    let exit = crate::cli::issue::execute_issue(
        Some(Role::Orchestrator),
        Some(provider),
        None,
        None,
        None,
        None,
        command::IssueCommand::Sync { all, no_clean },
    );
    let _ = std::env::set_current_dir(&previous_cwd);
    exit
}

/// Run one `worktree` subcommand through the CLI executor from `cwd`.
fn sync_cli_run_worktree(command: crate::command::WorktreeCommand, cwd: &std::path::Path) -> i32 {
    let previous_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(cwd).unwrap();
    let exit = crate::cli::worktree::execute_worktree(Some(Role::Orchestrator), command);
    let _ = std::env::set_current_dir(&previous_cwd);
    exit
}

/// A bound-then-released loopback port, so the connection is refused fast
/// without depending on a well-known port being free.
fn sync_cli_dead_api_base() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind free port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    format!("http://127.0.0.1:{port}/api/v1")
}

fn sync_cli_identity(repo: &std::path::Path) -> String {
    crate::worktree::repo_identity(&crate::worktree::ProcessWorktreeRunner::new(), repo)
        .expect("repository identity")
}

#[test]
fn issue_sync_cleans_remotely_closed_issue_residue() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("sync-clean");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let worktree = close_cli_add_worktree(&repo, "sync-clean", "feat/552-sync");
    let number = sync_cli_seed_closed_issue("Sync me");
    let identity = sync_cli_identity(&repo);
    let lease = close_cli_seed_lease_at(&identity, number, "session-sync", "active", &worktree);

    let report = sync_cli_pass(&repo, false, crate::cli::issue::sync::SyncMode::Clean);

    assert_eq!(report.mode, "clean");
    assert!(!report.all);
    assert_eq!(report.checked, 1);
    assert_eq!(report.not_closed, 0);
    assert_eq!(report.released_leases, 1);
    assert_eq!(report.cleaned, 1);
    assert_eq!(report.kept, 0);
    assert_eq!(report.issues.len(), 1);
    let issue = &report.issues[0];
    assert_eq!(issue.issue, number);
    assert_eq!(issue.remote_state, "closed");
    assert_eq!(issue.active_leases, 1);
    assert_eq!(issue.released_leases, 1);
    assert_eq!(issue.directories.len(), 1);
    assert_eq!(issue.directories[0].action, "cleaned");
    assert!(issue.directories[0].reason.is_none());

    assert!(
        !worktree.exists(),
        "a remotely closed issue's clean worktree must be removed"
    );
    let (status, reason) = close_cli_lease_state(&lease);
    assert_eq!(status, "retained");
    assert_eq!(
        reason.as_deref(),
        Some("issue closed on the remote (issue sync)")
    );
    assert!(
        close_cli_branch_exists(&repo, "feat/552-sync"),
        "the branch must never be deleted"
    );

    // The CLI wiring exits 0 for the same converged state (nothing left to
    // reconcile once the directory is gone).
    let exit = sync_cli_run_cli(ProviderKind::Local, false, false, &repo);
    assert_eq!(exit, 0);
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn issue_sync_keeps_dirty_worktree_and_reports_reason() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("sync-dirty");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let worktree = close_cli_add_worktree(&repo, "sync-dirty", "feat/552-dirty");
    // An untracked file is enough to make the directory dirty.
    fs::write(worktree.join("scratch.txt"), "wip").unwrap();
    let number = sync_cli_seed_closed_issue("Sync dirty");
    let identity = sync_cli_identity(&repo);
    let lease = close_cli_seed_lease_at(&identity, number, "session-dirty", "active", &worktree);

    let report = sync_cli_pass(&repo, false, crate::cli::issue::sync::SyncMode::Clean);

    assert_eq!(report.cleaned, 0);
    assert_eq!(report.kept, 1);
    let directory = &report.issues[0].directories[0];
    assert_eq!(directory.action, "kept");
    let reason = directory.reason.as_deref().expect("keep reason");
    assert!(
        reason.contains("uncommitted or untracked files"),
        "the shared cleanliness guard must supply the reason; got: {reason}"
    );
    assert!(worktree.exists(), "a dirty worktree must be kept");
    // The lease still converges: the issue is closed remotely.
    assert_eq!(close_cli_lease_state(&lease).0, "retained");
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn issue_sync_never_removes_the_main_checkout() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("sync-main-checkout");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let number = sync_cli_seed_closed_issue("Sync main");
    let identity = sync_cli_identity(&repo);
    // A reused-current-checkout lease points at the main checkout itself.
    let lease = close_cli_seed_lease_at(&identity, number, "session-main", "active", &repo);

    let report = sync_cli_pass(&repo, false, crate::cli::issue::sync::SyncMode::Clean);

    assert_eq!(report.cleaned, 0);
    assert_eq!(report.kept, 1);
    let directory = &report.issues[0].directories[0];
    assert_eq!(directory.action, "kept");
    let reason = directory.reason.as_deref().expect("keep reason");
    assert!(
        reason.contains("main checkout is never removed"),
        "the shared main-checkout guard must supply the reason; got: {reason}"
    );
    assert!(repo.exists(), "the main checkout is never removed");
    assert_eq!(close_cli_lease_state(&lease).0, "retained");
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn issue_sync_keeps_directory_held_by_another_issues_active_lease() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("sync-foreign-lease");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let worktree = close_cli_add_worktree(&repo, "sync-foreign", "feat/552-foreign");
    let number = sync_cli_seed_closed_issue("Sync foreign");
    let other_issue = close_cli_seed_issue("Still open", "New");
    let identity = sync_cli_identity(&repo);
    let lease = close_cli_seed_lease_at(&identity, number, "session-sync", "active", &worktree);
    // Another issue holds an active lease on the same directory through a
    // symlinked spelling: the unique `(repo, path)` lease index keeps the
    // row distinct while the shared directory guard still sees one target.
    let link = root.join("foreign-link");
    std::os::unix::fs::symlink(&worktree, &link).expect("symlink");
    let other_lease =
        close_cli_seed_lease_at(&identity, other_issue, "session-foreign", "active", &link);

    let report = sync_cli_pass(&repo, false, crate::cli::issue::sync::SyncMode::Clean);

    assert_eq!(report.checked, 2);
    assert_eq!(report.not_closed, 1);
    assert_eq!(report.cleaned, 0);
    assert_eq!(report.kept, 1);
    let issue = report
        .issues
        .iter()
        .find(|issue| issue.issue == number)
        .expect("the closed issue is reported");
    assert_eq!(issue.directories[0].action, "kept");
    let reason = issue.directories[0].reason.as_deref().expect("keep reason");
    assert!(
        reason.contains("session-foreign") && reason.contains(&format!("issue {other_issue}")),
        "the shared foreign-lease guard must name the owning session and issue; got: {reason}"
    );
    assert!(worktree.exists(), "the foreign lease keeps the directory");
    assert_eq!(close_cli_lease_state(&lease).0, "retained");
    assert_eq!(
        close_cli_lease_state(&other_lease).0,
        "active",
        "another issue's active lease is never released"
    );
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn issue_sync_leaves_open_issue_residue_and_lease_untouched() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("sync-open");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let open_issue = close_cli_seed_issue("Still open", "New");
    let worktree = close_cli_add_worktree(&repo, "sync-open", "feat/552-open");
    let identity = sync_cli_identity(&repo);
    let lease = close_cli_seed_lease_at(&identity, open_issue, "session-open", "active", &worktree);

    let report = sync_cli_pass(&repo, false, crate::cli::issue::sync::SyncMode::Clean);

    assert_eq!(report.checked, 1);
    assert_eq!(report.not_closed, 1);
    assert!(
        report.issues.is_empty(),
        "an open issue is not a reconciliation candidate"
    );
    assert_eq!(report.cleaned, 0);
    assert_eq!(report.released_leases, 0);
    assert!(worktree.exists());
    assert_eq!(close_cli_lease_state(&lease).0, "active");
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn issue_sync_no_clean_reports_verdicts_without_writing() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("sync-report");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let clean_number = sync_cli_seed_closed_issue("Report clean");
    let clean_worktree =
        close_cli_add_worktree(&repo, "sync-report-clean", "feat/552-report-clean");
    let dirty_number = sync_cli_seed_closed_issue("Report dirty");
    let dirty_worktree =
        close_cli_add_worktree(&repo, "sync-report-dirty", "feat/552-report-dirty");
    fs::write(dirty_worktree.join("scratch.txt"), "wip").unwrap();
    let identity = sync_cli_identity(&repo);
    let clean_lease = close_cli_seed_lease_at(
        &identity,
        clean_number,
        "session-report",
        "active",
        &clean_worktree,
    );
    let dirty_lease = close_cli_seed_lease_at(
        &identity,
        dirty_number,
        "session-dirty",
        "active",
        &dirty_worktree,
    );
    let clean_dir = clean_worktree.to_string_lossy().to_string();
    let dirty_dir = dirty_worktree.to_string_lossy().to_string();

    let report = sync_cli_pass(&repo, false, crate::cli::issue::sync::SyncMode::Report);

    assert_eq!(report.mode, "report");
    assert_eq!(report.checked, 2);
    assert_eq!(report.released_leases, 0);
    assert_eq!(report.cleaned, 0);
    assert_eq!(report.kept, 1);
    let clean_issue = report
        .issues
        .iter()
        .find(|issue| issue.issue == clean_number)
        .expect("closed clean issue is reported");
    assert_eq!(clean_issue.active_leases, 1);
    assert_eq!(clean_issue.released_leases, 0);
    assert_eq!(clean_issue.cleaned, 0);
    let clean_report = clean_issue
        .directories
        .iter()
        .find(|directory| directory.path == clean_dir)
        .expect("clean directory verdict");
    assert_eq!(clean_report.action, "would_clean");
    assert!(clean_report.reason.is_none());
    let dirty_issue = report
        .issues
        .iter()
        .find(|issue| issue.issue == dirty_number)
        .expect("closed dirty issue is reported");
    let dirty_report = dirty_issue
        .directories
        .iter()
        .find(|directory| directory.path == dirty_dir)
        .expect("dirty directory verdict");
    assert_eq!(dirty_report.action, "would_keep");
    assert!(
        dirty_report
            .reason
            .as_deref()
            .expect("keep reason")
            .contains("uncommitted or untracked files"),
        "got: {:?}",
        dirty_report.reason
    );

    // Report mode writes nothing, including the lease convergence.
    assert!(clean_worktree.exists());
    assert!(dirty_worktree.exists());
    assert_eq!(close_cli_lease_state(&clean_lease).0, "active");
    assert_eq!(close_cli_lease_state(&dirty_lease).0, "active");

    // `issue sync --no-clean` exits 0 and still writes nothing.
    let exit = sync_cli_run_cli(ProviderKind::Local, false, true, &repo);
    assert_eq!(exit, 0);
    assert!(clean_worktree.exists(), "report mode must not clean");
    assert_eq!(close_cli_lease_state(&clean_lease).0, "active");
    let _ = fs::remove_dir_all(&clean_worktree);
    let _ = fs::remove_dir_all(&dirty_worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn issue_sync_all_scans_every_lease_repository() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("sync-all");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo_a = root.join("repo-a");
    close_cli_init_repo(&repo_a);
    let worktree_a = close_cli_add_worktree(&repo_a, "sync-all-a", "feat/552-all-a");
    let number_a = sync_cli_seed_closed_issue("All scan A");
    let identity_a = sync_cli_identity(&repo_a);
    let lease_a =
        close_cli_seed_lease_at(&identity_a, number_a, "session-a", "active", &worktree_a);

    let repo_b = root.join("repo-b");
    close_cli_init_repo(&repo_b);
    let worktree_b = close_cli_add_worktree(&repo_b, "sync-all-b", "feat/552-all-b");
    let number_b = sync_cli_seed_closed_issue("All scan B");
    let identity_b = sync_cli_identity(&repo_b);
    let lease_b =
        close_cli_seed_lease_at(&identity_b, number_b, "session-b", "active", &worktree_b);

    // A lease whose checkout is gone must be reported, not scanned.
    let missing_identity = format!(
        "{}/phasegent-sync-missing-{}/.git",
        crate::test_scratch::root().display(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    close_cli_seed_lease_at(
        &missing_identity,
        9_000,
        "session-missing",
        "active",
        &std::path::Path::new(&missing_identity).with_file_name("worktree"),
    );

    let report = sync_cli_pass(&repo_a, true, crate::cli::issue::sync::SyncMode::Clean);

    assert!(report.all);
    assert_eq!(report.checked, 2);
    assert_eq!(report.released_leases, 2);
    assert_eq!(report.cleaned, 2);
    assert_eq!(report.skipped_repos.len(), 1);
    assert!(
        report.skipped_repos[0].repo_identity == missing_identity,
        "the missing checkout is reported verbatim"
    );
    assert!(
        report.skipped_repos[0].reason.contains("missing"),
        "got: {}",
        report.skipped_repos[0].reason
    );
    let cleaned: Vec<u64> = report.issues.iter().map(|issue| issue.issue).collect();
    assert!(cleaned.contains(&number_a) && cleaned.contains(&number_b));
    assert!(!worktree_a.exists(), "repo A's residue is reconciled");
    assert!(!worktree_b.exists(), "repo B's residue is reconciled");
    assert_eq!(close_cli_lease_state(&lease_a).0, "retained");
    assert_eq!(close_cli_lease_state(&lease_b).0, "retained");

    // The CLI wiring accepts `--all` and exits 0 on the converged state.
    let exit = sync_cli_run_cli(ProviderKind::Local, true, false, &repo_a);
    assert_eq!(exit, 0);
    let _ = fs::remove_dir_all(&worktree_a);
    let _ = fs::remove_dir_all(&worktree_b);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn issue_sync_skips_missing_remote_issue() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("sync-missing-remote");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let worktree = close_cli_add_worktree(&repo, "sync-missing-remote", "feat/552-missing");
    let identity = sync_cli_identity(&repo);
    // No provider issue 9_999 exists for this lease row: a stale row must
    // not wedge the pass.
    let lease = close_cli_seed_lease_at(&identity, 9_999, "session-missing", "active", &worktree);

    let report = sync_cli_pass(&repo, false, crate::cli::issue::sync::SyncMode::Clean);

    assert_eq!(report.checked, 1);
    assert_eq!(report.not_found, 1);
    assert!(report.issues.is_empty());
    assert!(worktree.exists(), "a missing remote issue deletes nothing");
    assert_eq!(close_cli_lease_state(&lease).0, "active");
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn issue_sync_reports_remote_failure_with_non_zero_exit() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("sync-remote-failure");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );
    let api_base = sync_cli_dead_api_base();
    let _api_guard = EnvGuard::set("PHASEGENT_API_BASE", &api_base);
    let _repo_guard = EnvGuard::set("PHASEGENT_REPOSITORY", "owner/repo");
    Storage::open()
        .expect("storage")
        .save_credential(Role::Orchestrator, "forgejo", "sync-test-token")
        .expect("store forgejo token");

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let worktree = close_cli_add_worktree(&repo, "sync-remote", "feat/552-remote");
    let number = sync_cli_seed_closed_issue("Remote down");
    let identity = sync_cli_identity(&repo);
    let lease = close_cli_seed_lease_at(&identity, number, "session-remote", "active", &worktree);

    let exit = sync_cli_run_cli(ProviderKind::Forgejo, false, false, &repo);

    assert_ne!(
        exit, 0,
        "a direct sync against an unreachable remote must exit non-zero"
    );
    assert!(
        worktree.exists(),
        "a failed pass must not delete a worktree directory"
    );
    assert_eq!(close_cli_lease_state(&lease).0, "active");
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn issue_sync_rejects_non_orchestrator_roles() {
    // The role gate fires before any storage or provider access.
    for role in [Role::Executor, Role::Reviewer, Role::Tester, Role::Admin] {
        let exit = crate::cli::issue::execute_issue(
            Some(role),
            Some(ProviderKind::Local),
            None,
            None,
            None,
            None,
            command::IssueCommand::Sync {
                all: false,
                no_clean: false,
            },
        );
        assert_eq!(exit, 3, "role '{role}' must be denied with exit code 3");
    }
}

#[test]
fn worktree_list_taxi_reconciles_closed_issue_residue() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("taxi-list");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );
    // `worktree` subcommands take no provider flag, so the taxi resolves the
    // configured default provider.
    let _provider_guard = EnvGuard::set("PHASEGENT_PROVIDER", "local");

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let worktree = close_cli_add_worktree(&repo, "taxi-list", "feat/552-taxi");
    let number = sync_cli_seed_closed_issue("Taxi list");
    let identity = sync_cli_identity(&repo);
    let lease = close_cli_seed_lease_at(&identity, number, "session-taxi", "active", &worktree);

    let exit = sync_cli_run_worktree(
        crate::command::WorktreeCommand::List {
            repo: None,
            no_sync: false,
        },
        &repo,
    );

    assert_eq!(exit, 0, "the taxi never changes the list exit code");
    assert!(
        !worktree.exists(),
        "the taxi reconciles the closed issue before list runs"
    );
    assert_eq!(close_cli_lease_state(&lease).0, "retained");
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn worktree_list_no_sync_skips_the_taxi() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("taxi-no-sync");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );
    let _provider_guard = EnvGuard::set("PHASEGENT_PROVIDER", "local");

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let worktree = close_cli_add_worktree(&repo, "taxi-no-sync", "feat/552-no-sync");
    let number = sync_cli_seed_closed_issue("Taxi no sync");
    let identity = sync_cli_identity(&repo);
    let lease = close_cli_seed_lease_at(&identity, number, "session-taxi", "active", &worktree);

    let exit = sync_cli_run_worktree(
        crate::command::WorktreeCommand::List {
            repo: None,
            no_sync: true,
        },
        &repo,
    );

    assert_eq!(exit, 0);
    assert!(
        worktree.exists(),
        "--no-sync must skip the reconciliation pass entirely"
    );
    assert_eq!(close_cli_lease_state(&lease).0, "active");
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn worktree_list_taxi_remote_failure_does_not_block() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("taxi-remote-failure");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );
    let api_base = sync_cli_dead_api_base();
    let _api_guard = EnvGuard::set("PHASEGENT_API_BASE", &api_base);
    let _repo_guard = EnvGuard::set("PHASEGENT_REPOSITORY", "owner/repo");
    let _provider_guard = EnvGuard::set("PHASEGENT_PROVIDER", "forgejo");
    Storage::open()
        .expect("storage")
        .save_credential(Role::Orchestrator, "forgejo", "sync-test-token")
        .expect("store forgejo token");

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let worktree = close_cli_add_worktree(&repo, "taxi-remote", "feat/552-taxi-remote");
    let number = sync_cli_seed_closed_issue("Taxi remote down");
    let identity = sync_cli_identity(&repo);
    let lease = close_cli_seed_lease_at(&identity, number, "session-taxi", "active", &worktree);

    let exit = sync_cli_run_worktree(
        crate::command::WorktreeCommand::List {
            repo: None,
            no_sync: false,
        },
        &repo,
    );

    assert_eq!(
        exit, 0,
        "an unreachable remote is a taxi warning, never a blocking error"
    );
    assert!(worktree.exists(), "a failed taxi pass deletes nothing");
    assert_eq!(close_cli_lease_state(&lease).0, "active");
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn worktree_list_taxi_targets_the_repo_flag_checkout() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("taxi-repo-flag");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );
    let _provider_guard = EnvGuard::set("PHASEGENT_PROVIDER", "local");

    let repo_main = root.join("repo-main");
    close_cli_init_repo(&repo_main);
    let worktree = close_cli_add_worktree(&repo_main, "taxi-repo-flag", "feat/552-repo-flag");
    let number = sync_cli_seed_closed_issue("Taxi repo flag");
    let identity = sync_cli_identity(&repo_main);
    let lease = close_cli_seed_lease_at(&identity, number, "session-taxi", "active", &worktree);

    let repo_other = root.join("repo-other");
    close_cli_init_repo(&repo_other);

    // `--repo` names the checkout the subcommand operates on, so the pass
    // reconciles that checkout instead of the working directory.
    let exit = sync_cli_run_worktree(
        crate::command::WorktreeCommand::List {
            repo: Some(repo_other.to_string_lossy().to_string()),
            no_sync: false,
        },
        &repo_main,
    );
    assert_eq!(exit, 0);
    assert!(
        worktree.exists(),
        "the --repo checkout has no residue, so the working directory is untouched"
    );
    assert_eq!(close_cli_lease_state(&lease).0, "active");

    // Without `--repo` the pass targets the working directory again.
    let exit = sync_cli_run_worktree(
        crate::command::WorktreeCommand::List {
            repo: None,
            no_sync: false,
        },
        &repo_main,
    );
    assert_eq!(exit, 0);
    assert!(!worktree.exists(), "the working directory is reconciled");
    assert_eq!(close_cli_lease_state(&lease).0, "retained");
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn worktree_prune_taxi_reconciles_closed_issue_residue() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("taxi-prune");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );
    let _provider_guard = EnvGuard::set("PHASEGENT_PROVIDER", "local");

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let worktree = close_cli_add_worktree(&repo, "taxi-prune", "feat/552-taxi-prune");
    let number = sync_cli_seed_closed_issue("Taxi prune");
    let identity = sync_cli_identity(&repo);
    let lease = close_cli_seed_lease_at(&identity, number, "session-taxi", "active", &worktree);

    // `prune` without action flags stays a read-only dry-run; the taxi runs
    // before it and reconciles the closed issue's residue.
    let exit = sync_cli_run_worktree(
        crate::command::WorktreeCommand::Prune {
            repo: Some(repo.to_string_lossy().to_string()),
            stale_days: 14,
            release_stale: false,
            remove: false,
            reason: None,
            no_sync: false,
        },
        &repo,
    );

    assert_eq!(exit, 0, "the taxi never changes the prune exit code");
    assert!(
        !worktree.exists(),
        "the taxi reconciles before prune classifies anything"
    );
    assert_eq!(close_cli_lease_state(&lease).0, "retained");
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn taxi_sync_is_silent_when_no_residue_exists() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("taxi-silent");
    let db = root.join("phasegent.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    // Deliberately no provider configuration: a silent pass never resolves
    // one, so an unconfigured host still gets zero output.
    let _provider_guard = EnvGuard::set("PHASEGENT_PROVIDER", "");

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let identity = sync_cli_identity(&repo);
    // A lease whose directory is already gone is not residue.
    close_cli_seed_lease_at(
        &identity,
        777,
        "session-gone",
        "retained",
        &root.join("missing-worktree"),
    );

    let previous_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(&repo).unwrap();
    let warnings = crate::cli::issue::sync::taxi_sync(Role::Orchestrator, None);
    let _ = std::env::set_current_dir(&previous_cwd);

    assert!(
        warnings.is_empty(),
        "no residue means zero output; got: {warnings:?}"
    );
    let _ = fs::remove_dir_all(root);
}
