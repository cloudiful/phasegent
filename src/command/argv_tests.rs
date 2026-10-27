//! Parser acceptance tests for `argv`: global-option placement hints, the
//! removed `--role` flag, and the Phase 2 registry role gate.

use super::*;
use crate::command::{Command, HelpTopic};

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn removed_role_flag_is_rejected_as_an_unknown_option() {
    for token in ["--role", "--role=executor"] {
        let error = parse_with_role_env(
            &args(&[token, "executor", "issue", "get", "1"]),
            Some("executor"),
        )
        .expect_err("the removed --role flag must be rejected");
        assert!(
            error.starts_with("unknown option '--role"),
            "token {token}: {error}"
        );
    }
}

#[test]
fn misplaced_global_option_after_subcommand_hints_at_position() {
    // The reported flow: `--project-id` after `issue create` failed with a
    // bare "unknown option". The error must now state that global options
    // come before the subcommand and show a correct example.
    let error = parse_with_role_env(
        &args(&[
            "issue",
            "create",
            "--title",
            "T",
            "--body",
            "B",
            "--project-id",
            "23",
        ]),
        Some("orchestrator"),
    )
    .expect_err("a misplaced global option must be rejected");
    assert!(
        error.starts_with("unknown option '--project-id'"),
        "got: {error}"
    );
    assert!(
        error.contains("must come before the subcommand"),
        "got: {error}"
    );
    assert!(
        error.contains("--project-id 23 issue create"),
        "got: {error}"
    );
}

#[test]
fn global_option_hint_covers_inline_and_separator_variants() {
    for token in ["--project-id=23", "--project_id"] {
        let error = parse_with_role_env(
            &args(&["issue", "close", "42", token]),
            Some("orchestrator"),
        )
        .expect_err("a misplaced global option must be rejected");
        assert!(
            error.contains("must come before the subcommand"),
            "token {token}: {error}"
        );
    }
}

#[test]
fn unrelated_unknown_option_keeps_its_plain_message() {
    let error = parse_with_role_env(
        &args(&[
            "issue",
            "create",
            "--title",
            "T",
            "--body",
            "B",
            "--nonsense",
            "alpha",
        ]),
        Some("orchestrator"),
    )
    .expect_err("an unknown option must be rejected");
    assert_eq!(error, "unknown option '--nonsense'");
}

#[test]
fn global_option_hint_only_matches_global_options() {
    for token in [
        "--provider",
        "--api-base",
        "--repository",
        "--project-id",
        "--close-status-id",
        "--output",
        "--no-progress",
    ] {
        let error = parse_with_role_env(
            &args(&["issue", "bind", "42", token, "value"]),
            Some("orchestrator"),
        )
        .expect_err("a misplaced global option must be rejected");
        assert!(
            error.contains("must come before the subcommand"),
            "token {token}: {error}"
        );
    }
}

/// The global output selection is accepted in both spellings, and an unknown
/// value is a parser error before any side effect.
#[test]
fn output_selection_is_parsed_in_both_spellings() {
    for (argv, expected) in [
        (vec!["--output", "text"], OutputSelection::Text),
        (vec!["--output=text"], OutputSelection::Text),
        (vec!["--output", "json"], OutputSelection::Json),
        (vec!["--output=json"], OutputSelection::Json),
        (vec!["--output", "auto"], OutputSelection::Auto),
        (vec!["--output=auto"], OutputSelection::Auto),
    ] {
        let mut full = argv.clone();
        full.push("doctor");
        let invocation = parse_with_role_env(&args(&full), Some("orchestrator"))
            .unwrap_or_else(|error| panic!("{argv:?} must parse: {error}"));
        assert_eq!(invocation.output.selection, expected, "argv {argv:?}");
    }
}

/// An unknown or missing output value is a parser error.
#[test]
fn an_invalid_output_value_is_rejected() {
    let error = parse_with_role_env(&args(&["--output", "yaml", "doctor"]), Some("orchestrator"))
        .expect_err("an unknown output value must be rejected");
    assert!(
        error.contains("--output must be 'auto', 'text', or 'json'"),
        "unexpected error: {error}"
    );
    let missing = parse_with_role_env(&args(&["--output"]), Some("orchestrator"))
        .expect_err("a missing output value must be rejected");
    assert!(
        missing.contains("--output requires a value"),
        "got: {missing}"
    );
}

/// A repeated `--output` is accepted with the same value and rejected with a
/// contradictory one.
#[test]
fn a_contradictory_output_duplicate_is_rejected() {
    let same = parse_with_role_env(
        &args(&["--output", "text", "--output", "text", "doctor"]),
        Some("orchestrator"),
    )
    .expect("the same selection twice must parse");
    assert_eq!(same.output.selection, OutputSelection::Text);
    let error = parse_with_role_env(
        &args(&["--output", "text", "--output", "json", "doctor"]),
        Some("orchestrator"),
    )
    .expect_err("a contradictory duplicate must be rejected");
    assert!(
        error.contains("--output must not be given twice"),
        "unexpected error: {error}"
    );
}

/// `--no-progress` is a global flag and never changes the output format.
#[test]
fn no_progress_is_parsed_as_a_global_flag() {
    let invocation = parse_with_role_env(&args(&["--no-progress", "doctor"]), Some("orchestrator"))
        .expect("the progress switch must parse");
    assert!(invocation.output.no_progress);
    assert_eq!(invocation.output.selection, OutputSelection::Auto);
}

/// A misplaced global option still gets the placement hint.
#[test]
fn a_misplaced_output_option_hints_at_its_position() {
    for token in ["--output", "--output=text", "--no-progress"] {
        let error = parse_with_role_env(
            &args(&["issue", "close", "42", token]),
            Some("orchestrator"),
        )
        .expect_err("a misplaced global option must be rejected");
        assert!(
            error.contains("must come before the subcommand"),
            "token {token}: {error}"
        );
    }
}

/// The prune keeps its legacy format flags, and a contradiction with an
/// explicit global `--output` is rejected at parse time.
#[test]
fn the_prune_legacy_format_agrees_with_the_global_selection() {
    let plain = parse_with_role_env(&args(&["worktree", "prune"]), Some("orchestrator"))
        .expect("the default prune must parse");
    assert_eq!(plain.output.legacy, None, "no legacy format is requested");
    assert_eq!(plain.output.selection, OutputSelection::Auto);

    let legacy_text = parse_with_role_env(
        &args(&["worktree", "prune", "--format", "text"]),
        Some("orchestrator"),
    )
    .expect("an explicit legacy format must parse");
    assert_eq!(legacy_text.output.legacy, Some(OutputMode::Text));

    let legacy_json = parse_with_role_env(
        &args(&["worktree", "prune", "--json"]),
        Some("orchestrator"),
    )
    .expect("an explicit legacy format must parse");
    assert_eq!(legacy_json.output.legacy, Some(OutputMode::Json));

    // A matching pair is accepted.
    parse_with_role_env(
        &args(&["--output", "text", "worktree", "prune", "--format", "text"]),
        Some("orchestrator"),
    )
    .expect("a matching global and legacy format must parse");

    // A contradictory pair is rejected before any storage or host work.
    let error = parse_with_role_env(
        &args(&["--output", "text", "worktree", "prune", "--format", "json"]),
        Some("orchestrator"),
    )
    .expect_err("a contradictory format pair must be rejected");
    assert!(error.contains("conflicts with --output"), "got: {error}");

    // An automatic global selection defers to the legacy flag.
    let automatic = parse_with_role_env(
        &args(&["--output", "auto", "worktree", "prune", "--format", "json"]),
        Some("orchestrator"),
    )
    .expect("an automatic selection must defer to the legacy flag");
    assert_eq!(automatic.output.legacy, Some(OutputMode::Json));
}

/// Phase 2 registry gate: a role-denied command fails at parse time with the
/// stable permission message that names the command's operation, and stays
/// distinct from the unknown-command error.
#[test]
fn role_denied_commands_fail_at_parse_with_the_stable_permission_message() {
    for (role, argv, operation) in [
        (
            "executor",
            &["issue", "create", "--title", "T", "--body", "B"][..],
            "issue create",
        ),
        ("executor", &["issue", "sync"][..], "issue sync"),
        ("executor", &["issue", "bind", "23"][..], "issue bind"),
        (
            "admin",
            &["worktree", "acquire", "--issue", "1"][..],
            "worktree acquire",
        ),
        ("executor", &["timer", "list"][..], "timer list"),
        (
            "executor",
            &["status", "set", "5", "--status", "Closed"][..],
            "issue status update",
        ),
        (
            "orchestrator",
            &["admin", "config", "set", "redmine-close-status-id", "5"][..],
            "admin config set",
        ),
    ] {
        let error = parse_with_role_env(&args(argv), Some(role))
            .expect_err("a denied role must fail at parse time");
        assert_eq!(
            error,
            format!("role '{role}' is not allowed to perform {operation}"),
            "argv {argv:?}"
        );
    }
}

#[test]
fn issue_bind_and_unbind_parse_bounded_deduplicated_id_sets() {
    let bind = parse_with_role_env(
        &args(&["issue", "bind", "12", "7", "12"]),
        Some("orchestrator"),
    )
    .expect("a multi-id bind must parse");
    match bind.command {
        Command::Issue(IssueCommand::Bind { issue_ids }) => {
            assert_eq!(issue_ids, vec![12, 7], "order is kept, duplicates collapse")
        }
        other => panic!("expected bind, got {other:?}"),
    }

    let single = parse_with_role_env(&args(&["issue", "bind", "42"]), Some("orchestrator"))
        .expect("a single-id bind must parse");
    match single.command {
        Command::Issue(IssueCommand::Bind { issue_ids }) => assert_eq!(issue_ids, vec![42]),
        other => panic!("expected bind, got {other:?}"),
    }

    for (argv, expected) in [
        (vec!["issue", "unbind"], Vec::new()),
        (vec!["issue", "unbind", "5", "5", "3"], vec![5, 3]),
    ] {
        let invocation = parse_with_role_env(&args(&argv), Some("orchestrator"))
            .expect("unbind accepts zero or more ids");
        match invocation.command {
            Command::Issue(IssueCommand::Unbind { issue_ids }) => {
                assert_eq!(issue_ids, expected, "{argv:?}")
            }
            other => panic!("expected unbind, got {other:?}"),
        }
    }

    // Repeated ids collapse before the 20-id cap, so a long duplicate-heavy
    // invocation with 20 distinct ids is accepted.
    let mut repeated = vec!["issue".to_owned(), "bind".to_owned()];
    repeated.extend((1..=20).flat_map(|id| [id.to_string(), id.to_string()]));
    let invocation = parse_with_role_env(&repeated, Some("orchestrator"))
        .expect("duplicates are removed before the batch cap");
    match invocation.command {
        Command::Issue(IssueCommand::Bind { issue_ids }) => {
            assert_eq!(issue_ids, (1..=20).collect::<Vec<_>>())
        }
        other => panic!("expected bind, got {other:?}"),
    }
}

#[test]
fn issue_bind_and_unbind_reject_invalid_batches_before_any_write() {
    for argv in [
        vec!["issue", "bind"],
        vec!["issue", "bind", "0"],
        vec!["issue", "bind", "12", "abc"],
        vec!["issue", "bind", "12", "0"],
        vec!["issue", "bind", "12", "--nonsense"],
        vec!["issue", "unbind", "12", "abc"],
        vec!["issue", "unbind", "0"],
    ] {
        parse_with_role_env(&args(&argv), Some("orchestrator"))
            .expect_err("an invalid id batch must fail at parse time");
    }
    let mut argv = vec!["issue".to_owned(), "bind".to_owned()];
    argv.extend((1..=21).map(|id| id.to_string()));
    let error = parse_with_role_env(&argv, Some("orchestrator"))
        .expect_err("more than 20 ids must fail at parse time");
    assert!(error.contains("at most 20 issue ids"), "got: {error}");
}

#[test]
fn unknown_command_keeps_its_distinct_error() {
    let error = parse_with_role_env(&args(&["frobnicate"]), Some("orchestrator")).unwrap_err();
    assert_eq!(error, "unknown command 'frobnicate'");
    assert!(
        !error.contains("not allowed"),
        "unknown command must not be reported as a permission denial: {error}"
    );
}

#[test]
fn roleless_help_keeps_the_superset_view() {
    for argv in [
        &["--help"][..],
        &["--help", "admin"][..],
        &["--help", "timer", "start"][..],
        &["--help", "workflow", "bootstrap"][..],
    ] {
        parse_with_role_env(&args(argv), None)
            .unwrap_or_else(|error| panic!("roleless help {argv:?} must route: {error}"));
    }
    // A role request that is denied for an explicit role still routes as
    // help; the help gate (not the parser) decides what it prints.
    for argv in [&["--help", "issue", "sync"][..], &["--help", "admin"][..]] {
        parse_with_role_env(&args(argv), Some("executor"))
            .unwrap_or_else(|error| panic!("help {argv:?} must route: {error}"));
    }
}

/// Unregistered top-level names are unknown commands, never permission
/// denials, and a registry lookup never resolves them for any role.
#[test]
fn unregistered_names_are_unknown_commands() {
    for name in ["frobnicate", "bogus"] {
        let error = parse_with_role_env(&args(&[name]), None).unwrap_err();
        assert_eq!(error, format!("unknown command '{name}'"));
        assert!(
            crate::command::registry_unavailability(None, &[name]).is_none()
                && !crate::command::registry_allows_role(Role::Orchestrator, &[name]),
            "{name} must not resolve to a registry node"
        );
    }

    let denied = parse_with_role_env(&args(&["issue", "sync"]), Some("executor")).unwrap_err();
    assert_eq!(
        denied,
        "role 'executor' is not allowed to perform issue sync"
    );
}

/// An empty argv parses to the root help for a bare launch; an unmanaged
/// Operator (no role, no managed session) parses ordinary business commands
/// while a managed session without a role fails closed.
#[test]
fn empty_argv_is_root_help_and_role_is_required_for_operations() {
    let invocation = parse_with_role_env(&[], None).expect("empty args must parse");
    assert!(matches!(invocation.command, Command::Help(HelpTopic::Root)));

    // Unmanaged Operator parses business commands (execution uses the admin
    // credential centrally).
    let invocation = parse_with_role_env(&args(&["issue", "get", "1"]), None)
        .expect("operator issue get must parse");
    assert_eq!(invocation.caller, crate::policy::CallerContext::Operator);

    // Managed session without a role fails closed before credentials.
    let error = parse_with_caller_env(&args(&["issue", "get", "1"]), None, true)
        .expect_err("managed issue get without a role must fail");
    assert!(
        error.contains("managed session requires a role"),
        "unexpected error: {error}"
    );

    // Invalid roles always error, even with a managed marker.
    let error = parse_with_caller_env(&args(&["issue", "get", "1"]), Some("wizard"), true)
        .expect_err("invalid role must error");
    assert!(error.contains("PHASEGENT_ROLE is invalid"), "got: {error}");
}

#[test]
fn operator_parses_business_commands_and_utility_stays_available_for_managed() {
    // Operator parses every ordinary/admin surface (record create parses;
    // execution denies it with a comment-create hint).
    for argv in [
        &["issue", "create", "--title", "T", "--body", "B"][..],
        &["comment", "create", "1", "--marker", "m", "--body", "m"][..],
        &["status", "set", "5", "--status", "Closed"][..],
        &[
            "timer",
            "start",
            "1",
            "--phase",
            "P1",
            "--agent-role",
            "executor",
            "--attempt",
            "1",
        ][..],
        &["worktree", "acquire", "--issue", "1"][..],
        &["admin", "workflow", "bootstrap"][..],
        &[
            "record",
            "create",
            "1",
            "--kind",
            "executor",
            "--key",
            "k",
            "--phase",
            "P1",
            "--attempt",
            "1",
            "--body",
            "n",
        ][..],
    ] {
        let invocation = parse_with_caller_env(&args(argv), None, false)
            .unwrap_or_else(|error| panic!("operator {argv:?} must parse: {error}"));
        assert_eq!(invocation.caller, crate::policy::CallerContext::Operator);
    }
    // Managed without a role keeps the roleless utility allowlist.
    for argv in [
        &["--help"][..],
        &["doctor"][..],
        &["issue", "bind", "23"][..],
        &["issue", "status"][..],
    ] {
        parse_with_caller_env(&args(argv), None, true)
            .unwrap_or_else(|error| panic!("managed utility {argv:?} must parse: {error}"));
    }
    // Managed without a role denies business, including role-scoped config.
    for argv in [
        &["issue", "create", "--title", "T", "--body", "B"][..],
        &["status", "set", "5", "--status", "Closed"][..],
        &["timer", "list"][..],
        &["admin", "config", "set", "redmine-close-status-id", "5"][..],
    ] {
        let error =
            parse_with_caller_env(&args(argv), None, true).expect_err("managed business must fail");
        assert!(
            error.contains("managed session requires a role"),
            "argv {argv:?}: {error}"
        );
    }
    // Global config stays available for managed sessions.
    parse_with_caller_env(
        &args(&[
            "admin",
            "config",
            "set",
            "redmine-api-base",
            "https://example",
        ]),
        None,
        true,
    )
    .expect("managed global config must parse");
}

/// Host marker presence (issue #859 P1): `OPENCODE_SESSION_ID` is
/// classification-only and selects the same managed-missing gate as
/// `PHASEGENT_SESSION_ID`. Pure helper stays deterministic via the injected
/// bool, while the real `session_present()` reads both process markers.
#[test]
fn host_marker_selects_the_same_managed_missing_gate() {
    // Either marker alone (even blank) is managed-missing without a role.
    let error = parse_with_caller_env(&args(&["issue", "get", "1"]), None, true)
        .expect_err("host-managed issue get without a role must fail");
    assert!(
        error.contains("managed session requires a role"),
        "got: {error}"
    );
    // Pure helper covers both markers without touching process env.
    assert!(crate::policy::session_present_for_both(
        None,
        Some("host-session")
    ));
    assert!(crate::policy::session_present_for_both(None, Some("")));
    assert!(crate::policy::session_present_for_both(
        Some("managed"),
        None
    ));
    assert!(crate::policy::session_present_for_both(
        Some("a"),
        Some("b")
    ));
    assert!(!crate::policy::session_present_for_both(None, None));

    // Real process probe respects both markers under the workflow lock.
    {
        let _lock = crate::infra::storage::test_support::lock_workflow_tests();
        let saved_phasegent = std::env::var_os(crate::policy::SESSION_ENV);
        let saved_host = std::env::var_os(crate::policy::HOST_SESSION_ENV);
        // SAFETY: serialised by the workflow lock.
        unsafe {
            std::env::remove_var(crate::policy::SESSION_ENV);
            std::env::remove_var(crate::policy::HOST_SESSION_ENV);
        }
        assert!(
            !crate::policy::session_present(),
            "no marker must read as Operator"
        );
        // SAFETY: same lock.
        unsafe {
            std::env::set_var(crate::policy::HOST_SESSION_ENV, "host-only");
        }
        assert!(
            crate::policy::session_present(),
            "host-only marker must read as managed"
        );
        // SAFETY: restore under the same lock.
        unsafe {
            match saved_phasegent {
                Some(value) => std::env::set_var(crate::policy::SESSION_ENV, value),
                None => std::env::remove_var(crate::policy::SESSION_ENV),
            }
            match saved_host {
                Some(value) => std::env::set_var(crate::policy::HOST_SESSION_ENV, value),
                None => std::env::remove_var(crate::policy::HOST_SESSION_ENV),
            }
        }
    }

    // An explicit role with either marker stays an Agent (marker never
    // overrides the role).
    let invocation = parse_with_caller_env(&args(&["issue", "get", "1"]), Some("explore"), true)
        .expect("explicit role with a marker must parse");
    assert_eq!(
        invocation.caller,
        crate::policy::CallerContext::Agent(crate::policy::Role::Explore)
    );
}
