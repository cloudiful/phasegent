//! `comment list` parsing tests.
//!
//! Listing takes exactly one issue number and resolves to the same
//! `CommentRead` capability as `comment get`; execution envelope
//! behaviour is pinned per provider in the contract suites.

use crate::command::{self, Command, CommentCommand};

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

#[test]
fn comment_list_parses_single_issue_number() {
    let invocation = command::parse(&strings(&["--role", "executor", "comment", "list", "17"]))
        .expect("comment list must parse");
    assert!(matches!(
        invocation.command,
        Command::Comment(CommentCommand::List { issue: 17 })
    ));
}

#[test]
fn comment_list_rejects_missing_and_non_numeric_numbers() {
    let missing = command::parse(&strings(&["--role", "executor", "comment", "list"]))
        .expect_err("missing number must error");
    assert!(
        missing.contains("unexpected arguments"),
        "unexpected error: {missing}"
    );

    let non_numeric = command::parse(&strings(&["--role", "executor", "comment", "list", "abc"]))
        .expect_err("non-numeric number must error");
    assert!(
        non_numeric.contains("numeric issue number"),
        "unexpected error: {non_numeric}"
    );
}
