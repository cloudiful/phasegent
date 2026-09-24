use super::*;

#[test]
fn inline_form_accepts_leading_dash_values_for_required_options() {
    // Markdown list bullets (`- Goal`) and separator lines (`---`) must reach the
    // server intact when supplied via the explicit `--option=value` token form.
    // Two-arg `--option value` still treats a leading-dash next token as missing,
    // so this regression covers only the escape hatch.

    // issue title leading dash
    let args = ["issue", "create", "--title=-starts-with-dash", "--body=ok"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse_with_role_env(&args, Some("orchestrator"))
        .expect("inline --title should parse");
    match invocation.command {
        command::Command::Issue(command::IssueCommand::Create { title, body, .. }) => {
            assert_eq!(title, "-starts-with-dash");
            assert_eq!(body, "ok");
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // issue body (Markdown bullet) via issue create
    let args = ["issue", "create", "--title=ok", "--body=- Goal"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse_with_role_env(&args, Some("orchestrator"))
        .expect("inline --body should parse");
    match invocation.command {
        command::Command::Issue(command::IssueCommand::Create { title, body, .. }) => {
            assert_eq!(title, "ok");
            assert_eq!(body, "- Goal");
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // issue body (`---` separator) via issue update
    let args = ["issue", "update", "1", "--body=---"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse_with_role_env(&args, Some("orchestrator"))
        .expect("inline --body should parse");
    match invocation.command {
        command::Command::Issue(command::IssueCommand::Update { number, body, .. }) => {
            assert_eq!(number, 1);
            assert_eq!(body, "---");
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // issue search query beginning with a dash (negative filter style)
    let args = ["issue", "search", "--query=-tag:regression"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse_with_role_env(&args, Some("orchestrator"))
        .expect("inline --query should parse");
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
        "issue",
        "search",
        "--state=closed",
        "--query=-tag:regression",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let invocation = command::parse_with_role_env(&args, Some("orchestrator"))
        .expect("inline --state should parse");
    match invocation.command {
        command::Command::Issue(command::IssueCommand::Search { query, state, .. }) => {
            assert_eq!(query.as_deref(), Some("-tag:regression"));
            assert_eq!(state, "closed");
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // comment body leading dash via comment create
    let args = [
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
    let invocation =
        command::parse_with_role_env(&args, Some("executor")).expect("inline --body should parse");
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
    let args = ["comment", "find-marker", "1", "--marker=---"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse_with_role_env(&args, Some("executor"))
        .expect("inline --marker should parse");
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
            "issue",
            "create",
            "--title",
            "Title",
            "--body",
            "-not-value",
        ],
        // --body followed by a separator-style value should still error
        vec!["issue", "update", "1", "--body", "---"],
        // --query followed by a leading-dash value should still error
        vec!["issue", "search", "--query", "-tag:regression"],
        // --marker followed by a leading-dash value should still error
        vec!["comment", "find-marker", "1", "--marker", "---"],
    ] {
        let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert!(
            command::parse_with_role_env(&args, Some("executor")).is_err(),
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
        "issue",
        "create",
        "--title=ok",
        "--bodyline=should-not-match-body",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let error = command::parse_with_role_env(&args, Some("orchestrator"))
        .expect_err("unknown long option must error");
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
    let args = ["issue", "create", "--title=ok", "--body="]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse_with_role_env(&args, Some("orchestrator"))
        .expect("inline empty body should parse");
    match invocation.command {
        command::Command::Issue(command::IssueCommand::Create { title, body, .. }) => {
            assert_eq!(title, "ok");
            assert_eq!(body, "");
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let args = ["comment", "create", "1", "--body=ok", "--marker="]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let error = command::parse_with_role_env(&args, Some("executor"))
        .expect_err("empty inline marker must error");
    assert!(
        error.contains("non-empty"),
        "expected non-empty marker error, got: {error}"
    );
}

#[test]
fn empty_marker_is_rejected_by_parser_and_provider() {
    let args = ["comment", "find-marker", "1", "--marker", ""]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert!(command::parse_with_role_env(&args, Some("orchestrator")).is_err());

    let provider = ForgejoProvider::new(
        ForgejoConfig::new("http://127.0.0.1:1/api/v1", "owner", "repo"),
        "token".to_owned(),
    )
    .unwrap();
    let error = provider.find_marker(1, "").unwrap_err();
    assert_eq!(error.json()["kind"], "config");
}
