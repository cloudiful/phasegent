//! Parser acceptance tests for `argv`: global-option placement hints, the
//! removed `--role` flag, and the Phase 2 registry role gate.

use super::*;

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
            &["admin", "config", "set", "api-base", "x"][..],
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

/// Phase 3: the compile-time boundary is a visibility/explanation gate, not a
/// parser rejection. `gui` stays parseable without a role so the execution
/// layer can return its structured not-compiled error; unknown commands and
/// role denials keep their distinct parse errors.
#[test]
fn gui_feature_boundary_does_not_change_the_parser_contract() {
    let invocation = parse_with_role_env(&args(&["gui"]), None)
        .expect("gui must parse without a role even when the shell was not compiled");
    assert!(invocation.role.is_none());
    assert!(matches!(invocation.command, Command::Gui));

    if !crate::command::top_level_compiled("gui") {
        assert!(matches!(
            crate::command::registry_unavailability(None, &["gui"]),
            Some(crate::command::Unavailable::NotCompiled(_))
        ));
        assert!(
            !crate::command::registry_allows_role(Role::Orchestrator, &["gui"]),
            "an uncompiled command must not count as runnable for a role"
        );
        assert_eq!(
            crate::command::registry_denied_operation(Role::Orchestrator, &["gui"]),
            None,
            "the parser must not turn a not-compiled command into a permission denial"
        );
    }

    let unknown = parse_with_role_env(&args(&["frobnicate"]), None).unwrap_err();
    assert_eq!(unknown, "unknown command 'frobnicate'");
    let denied = parse_with_role_env(&args(&["issue", "sync"]), Some("executor")).unwrap_err();
    assert_eq!(
        denied,
        "role 'executor' is not allowed to perform issue sync"
    );
}
