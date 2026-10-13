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
