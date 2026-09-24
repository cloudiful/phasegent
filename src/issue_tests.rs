//! `issue get` batch parsing tests.
//!
//! A single number keeps the legacy `Get` shape (and its
//! single-object output); two or more become `GetBatch`. Execution
//! envelope behaviour lives in the Redmine contract suite, which can
//! drive `batch_fetch_issues` against a mock server.

use crate::command::{self, AssigneeOption, BranchOption, Command, IssueCommand};

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

fn create_assignee(argv: &[&str]) -> AssigneeOption {
    let mut args = vec!["issue", "create", "--title", "T", "--body", "B"];
    args.extend_from_slice(argv);
    let invocation =
        command::parse_with_role_env(&strings(&args), Some("executor")).expect("create must parse");
    match invocation.command {
        Command::Issue(IssueCommand::Create { assignee, .. }) => assignee,
        other => panic!("expected Create, got {other:?}"),
    }
}

#[test]
fn issue_create_assignee_defaults_to_unset() {
    assert_eq!(create_assignee(&[]), AssigneeOption::Unset);
}

#[test]
fn issue_create_assignee_parses_explicit_value_and_no_assign() {
    assert_eq!(
        create_assignee(&["--assignee", "alice"]),
        AssigneeOption::Explicit("alice".to_owned())
    );
    assert_eq!(
        create_assignee(&["--assignee=42"]),
        AssigneeOption::Explicit("42".to_owned())
    );
    assert_eq!(
        create_assignee(&["--no-assign"]),
        AssigneeOption::Unassigned
    );
}

#[test]
fn issue_create_rejects_assignee_with_no_assign_and_empty_value() {
    let conflict = command::parse_with_role_env(
        &strings(&[
            "issue",
            "create",
            "--title",
            "T",
            "--body",
            "B",
            "--assignee",
            "alice",
            "--no-assign",
        ]),
        Some("executor"),
    )
    .expect_err("--assignee with --no-assign must error");
    assert!(
        conflict.contains("mutually exclusive"),
        "unexpected error: {conflict}"
    );

    let empty = command::parse_with_role_env(
        &strings(&[
            "issue",
            "create",
            "--title",
            "T",
            "--body",
            "B",
            "--assignee",
            "",
        ]),
        Some("executor"),
    )
    .expect_err("empty --assignee must error");
    assert!(
        empty.contains("non-empty --assignee"),
        "unexpected error: {empty}"
    );
}

#[test]
fn issue_get_single_number_keeps_legacy_shape() {
    let invocation =
        command::parse_with_role_env(&strings(&["issue", "get", "17"]), Some("executor"))
            .expect("single issue get must parse");
    assert!(matches!(
        invocation.command,
        Command::Issue(IssueCommand::Get { number: 17 })
    ));
}

#[test]
fn issue_get_multiple_numbers_become_batch() {
    let invocation = command::parse_with_role_env(
        &strings(&["issue", "get", "17", "19", "23"]),
        Some("executor"),
    )
    .expect("batch issue get must parse");
    match invocation.command {
        Command::Issue(IssueCommand::GetBatch { numbers }) => {
            assert_eq!(numbers, vec![17, 19, 23]);
        }
        other => panic!("expected GetBatch, got {other:?}"),
    }
}

#[test]
fn issue_get_rejects_empty_non_numeric_zero_duplicate_and_oversize() {
    let missing = command::parse_with_role_env(&strings(&["issue", "get"]), Some("executor"))
        .expect_err("missing number must error");
    assert!(
        missing.contains("requires an issue number"),
        "unexpected error: {missing}"
    );

    let non_numeric =
        command::parse_with_role_env(&strings(&["issue", "get", "abc"]), Some("executor"))
            .expect_err("non-numeric number must error");
    assert!(
        non_numeric.contains("numeric issue number"),
        "unexpected error: {non_numeric}"
    );

    let zero = command::parse_with_role_env(&strings(&["issue", "get", "0"]), Some("executor"))
        .expect_err("zero must error");
    assert!(
        zero.contains("greater than zero"),
        "unexpected error: {zero}"
    );

    let duplicate =
        command::parse_with_role_env(&strings(&["issue", "get", "7", "7"]), Some("executor"))
            .expect_err("duplicate must error");
    assert!(
        duplicate.contains("duplicate issue number 7"),
        "unexpected error: {duplicate}"
    );

    let many: Vec<String> = ["issue", "get"]
        .into_iter()
        .map(str::to_owned)
        .chain((1..=21).map(|number| number.to_string()))
        .collect();
    let oversize = command::parse_with_role_env(&many, Some("executor"))
        .expect_err("more than 20 numbers must error");
    assert!(
        oversize.contains("at most 20"),
        "unexpected error: {oversize}"
    );
}

fn create_session(argv: &[&str]) -> Option<Box<str>> {
    let mut args = vec!["issue", "create", "--title", "T", "--body", "B"];
    args.extend_from_slice(argv);
    let invocation = command::parse_with_role_env(&strings(&args), Some("orchestrator"))
        .expect("create must parse");
    match invocation.command {
        Command::Issue(IssueCommand::Create { session, .. }) => session,
        other => panic!("expected Create, got {other:?}"),
    }
}

#[test]
fn issue_create_session_defaults_to_none_and_round_trips() {
    assert_eq!(create_session(&[]), None);
    assert_eq!(create_session(&["--session", "s1"]).as_deref(), Some("s1"));
    assert_eq!(
        create_session(&["--session=env-session"]).as_deref(),
        Some("env-session")
    );
}

#[test]
fn issue_create_rejects_blank_and_overlong_session() {
    for raw in ["", "   "] {
        let error = command::parse_with_role_env(
            &strings(&[
                "issue",
                "create",
                "--title",
                "T",
                "--body",
                "B",
                "--session",
                raw,
            ]),
            Some("executor"),
        )
        .expect_err("blank session must error");
        assert!(error.contains("session"), "unexpected error: {error}");
    }
    let overlong = "s".repeat(129);
    let error = command::parse_with_role_env(
        &strings(&[
            "issue",
            "create",
            "--title",
            "T",
            "--body",
            "B",
            "--session",
            &overlong,
        ]),
        Some("executor"),
    )
    .expect_err("overlong session must error");
    assert!(
        error.contains("session") && error.contains("128"),
        "unexpected error: {error}"
    );
}

fn create_branch(argv: &[&str]) -> (BranchOption, Option<String>) {
    let mut args = vec!["issue", "create", "--title", "T", "--body", "B"];
    args.extend_from_slice(argv);
    let invocation = command::parse_with_role_env(&strings(&args), Some("orchestrator"))
        .expect("create must parse");
    match invocation.command {
        Command::Issue(IssueCommand::Create { branch, base, .. }) => (branch, base),
        other => panic!("expected Create, got {other:?}"),
    }
}

#[test]
fn issue_create_branch_defaults_to_unset_and_base_defaults_to_none() {
    assert_eq!(create_branch(&[]), (BranchOption::Unset, None));
}

#[test]
fn issue_create_branch_bare_means_auto_and_named_forms_use_name() {
    assert_eq!(create_branch(&["--branch"]), (BranchOption::Auto, None));
    assert_eq!(
        create_branch(&["--branch", "feat/452"]),
        (BranchOption::Named("feat/452".to_owned()), None)
    );
    assert_eq!(
        create_branch(&["--branch=fix/452"]),
        (BranchOption::Named("fix/452".to_owned()), None)
    );
    assert_eq!(
        create_branch(&["--branch", "--tracker", "Bug"]),
        (BranchOption::Auto, None)
    );
}

#[test]
fn issue_create_branch_with_base_round_trips() {
    assert_eq!(
        create_branch(&["--branch", "--base", "main"]),
        (BranchOption::Auto, Some("main".to_owned()))
    );
    assert_eq!(
        create_branch(&["--branch", "feat/452", "--base", "main"]),
        (
            BranchOption::Named("feat/452".to_owned()),
            Some("main".to_owned())
        )
    );
    assert_eq!(
        create_branch(&["--branch=feat/452", "--base=main"]),
        (
            BranchOption::Named("feat/452".to_owned()),
            Some("main".to_owned())
        )
    );
}

#[test]
fn issue_create_branch_rejects_base_without_branch_and_bad_names() {
    let dangling = command::parse_with_role_env(
        &strings(&[
            "issue", "create", "--title", "T", "--body", "B", "--base", "main",
        ]),
        Some("orchestrator"),
    )
    .expect_err("--base without --branch must error");
    assert!(
        dangling.contains("--base requires --branch"),
        "unexpected error: {dangling}"
    );

    let empty = command::parse_with_role_env(
        &strings(&[
            "issue",
            "create",
            "--title",
            "T",
            "--body",
            "B",
            "--branch=",
        ]),
        Some("orchestrator"),
    );
    // Empty inline `--branch=` is the bare auto form, not an error.
    match empty {
        Ok(invocation) => match invocation.command {
            Command::Issue(IssueCommand::Create { branch, .. }) => {
                assert_eq!(branch, BranchOption::Auto);
            }
            other => panic!("expected Create, got {other:?}"),
        },
        Err(error) => panic!("empty --branch= must mean auto, got: {error}"),
    }

    let whitespace = command::parse_with_role_env(
        &strings(&[
            "issue", "create", "--title", "T", "--body", "B", "--branch", "bad name",
        ]),
        Some("orchestrator"),
    )
    .expect_err("whitespace branch name must error");
    assert!(
        whitespace.contains("--branch"),
        "unexpected error: {whitespace}"
    );
}

/// Issue 552 Phase 2: `issue sync [--all] [--no-clean]` parses both
/// switches, defaults them off, rejects unknown options, and keeps the
/// orchestrator gate at the role-required level (the command-level gate
/// runs at execution time).
#[test]
fn issue_sync_parses_scope_and_report_switches() {
    let both = command::parse_with_role_env(
        &strings(&["issue", "sync", "--all", "--no-clean"]),
        Some("orchestrator"),
    )
    .expect("issue sync --all --no-clean must parse");
    match both.command {
        Command::Issue(IssueCommand::Sync { all, no_clean }) => {
            assert!(all, "--all must round-trip");
            assert!(no_clean, "--no-clean must round-trip");
        }
        other => panic!("expected Sync, got {other:?}"),
    }

    let default = command::parse_with_role_env(&strings(&["issue", "sync"]), Some("orchestrator"))
        .expect("bare issue sync must parse");
    match default.command {
        Command::Issue(IssueCommand::Sync { all, no_clean }) => {
            assert!(!all, "--all must default off");
            assert!(!no_clean, "--no-clean must default off");
        }
        other => panic!("expected Sync, got {other:?}"),
    }

    let unknown = command::parse_with_role_env(
        &strings(&["issue", "sync", "--nonsense"]),
        Some("orchestrator"),
    )
    .expect_err("an unknown option must be rejected");
    assert!(
        unknown.contains("unknown option '--nonsense'"),
        "got: {unknown}"
    );

    // Unlike the local branch context commands, `issue sync` needs a role.
    let missing_role = command::parse_with_role_env(&strings(&["issue", "sync"]), None)
        .expect_err("a role must be required");
    assert!(
        missing_role.contains("a role is required"),
        "got: {missing_role}"
    );
}
