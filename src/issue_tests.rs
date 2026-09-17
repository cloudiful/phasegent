//! `issue get` batch parsing tests.
//!
//! A single number keeps the legacy `Get` shape (and its
//! single-object output); two or more become `GetBatch`. Execution
//! envelope behaviour lives in the Redmine contract suite, which can
//! drive `batch_fetch_issues` against a mock server.

use crate::command::{self, AssigneeOption, Command, IssueCommand};

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

fn create_assignee(argv: &[&str]) -> AssigneeOption {
    let mut args = vec![
        "--role", "executor", "issue", "create", "--title", "T", "--body", "B",
    ];
    args.extend_from_slice(argv);
    let invocation = command::parse(&strings(&args)).expect("create must parse");
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
    let conflict = command::parse(&strings(&[
        "--role",
        "executor",
        "issue",
        "create",
        "--title",
        "T",
        "--body",
        "B",
        "--assignee",
        "alice",
        "--no-assign",
    ]))
    .expect_err("--assignee with --no-assign must error");
    assert!(
        conflict.contains("mutually exclusive"),
        "unexpected error: {conflict}"
    );

    let empty = command::parse(&strings(&[
        "--role",
        "executor",
        "issue",
        "create",
        "--title",
        "T",
        "--body",
        "B",
        "--assignee",
        "",
    ]))
    .expect_err("empty --assignee must error");
    assert!(
        empty.contains("non-empty --assignee"),
        "unexpected error: {empty}"
    );
}

#[test]
fn issue_get_single_number_keeps_legacy_shape() {
    let invocation = command::parse(&strings(&["--role", "executor", "issue", "get", "17"]))
        .expect("single issue get must parse");
    assert!(matches!(
        invocation.command,
        Command::Issue(IssueCommand::Get { number: 17 })
    ));
}

#[test]
fn issue_get_multiple_numbers_become_batch() {
    let invocation = command::parse(&strings(&[
        "--role", "executor", "issue", "get", "17", "19", "23",
    ]))
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
    let missing = command::parse(&strings(&["--role", "executor", "issue", "get"]))
        .expect_err("missing number must error");
    assert!(
        missing.contains("requires an issue number"),
        "unexpected error: {missing}"
    );

    let non_numeric = command::parse(&strings(&["--role", "executor", "issue", "get", "abc"]))
        .expect_err("non-numeric number must error");
    assert!(
        non_numeric.contains("numeric issue number"),
        "unexpected error: {non_numeric}"
    );

    let zero = command::parse(&strings(&["--role", "executor", "issue", "get", "0"]))
        .expect_err("zero must error");
    assert!(
        zero.contains("greater than zero"),
        "unexpected error: {zero}"
    );

    let duplicate = command::parse(&strings(&["--role", "executor", "issue", "get", "7", "7"]))
        .expect_err("duplicate must error");
    assert!(
        duplicate.contains("duplicate issue number 7"),
        "unexpected error: {duplicate}"
    );

    let many: Vec<String> = ["--role", "executor", "issue", "get"]
        .into_iter()
        .map(str::to_owned)
        .chain((1..=21).map(|number| number.to_string()))
        .collect();
    let oversize = command::parse(&many).expect_err("more than 20 numbers must error");
    assert!(
        oversize.contains("at most 20"),
        "unexpected error: {oversize}"
    );
}
