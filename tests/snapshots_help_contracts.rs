//! Snapshot coverage for the CLI help surface that carries the
//! close/sync/worktree contracts.
//!
//! The help texts have rotted twice — a flag renamed without the page
//! following and a pointer naming a page that no longer resolved — and both
//! slips were only caught by a human reading the deep pages. A snapshot makes
//! the next edit visible as a review diff instead.
//!
//! The pages are pinned role-less (the "all roles" view, which is the superset
//! a documentation reader sees) plus the executor view of an orchestrator-only
//! command, so the role filter itself is covered too.

#[path = "snapshots_support/mod.rs"]
mod snapshots;

use insta::assert_snapshot;
use snapshots::{Scratch, regex_escape, run, stderr_text, stdout_text};

/// Render one help page and fail loudly when the page does not render.
fn help(scratch: &Scratch, args: &[&str]) -> String {
    let output = run(scratch, scratch.root(), args);
    assert!(
        output.status.success(),
        "{args:?} exited with {}: stderr={}",
        output.status,
        stderr_text(&output),
    );
    let stdout = stdout_text(&output);
    assert!(!stdout.trim().is_empty(), "{args:?} rendered no help");
    stdout.trim_end().to_owned()
}

#[test]
fn root_help_snapshot() {
    let scratch = Scratch::new("help-root");
    // The root page prints the crate version, which moves on every release
    // bump and is not part of the help contract.
    let version = regex_escape(&format!("phasegent {}", env!("CARGO_PKG_VERSION")));
    insta::with_settings!({filters => vec![(version.as_str(), "phasegent [VERSION]")]}, {
        assert_snapshot!("root_help", help(&scratch, &["--help"]));
    });
}

#[test]
fn issue_close_help_snapshot() {
    let scratch = Scratch::new("help-issue-close");
    assert_snapshot!(
        "issue_close_help",
        help(&scratch, &["--help", "issue", "close"])
    );
}

#[test]
fn issue_sync_help_snapshot() {
    let scratch = Scratch::new("help-issue-sync");
    assert_snapshot!(
        "issue_sync_help",
        help(&scratch, &["--help", "issue", "sync"])
    );
}

#[test]
fn issue_sync_help_hides_the_orchestrator_only_command_from_executor() {
    let scratch = Scratch::new("help-issue-sync-executor");
    assert_snapshot!(
        "issue_sync_help_executor",
        help(&scratch, &["--role", "executor", "--help", "issue", "sync"])
    );
}

#[test]
fn worktree_help_snapshot() {
    let scratch = Scratch::new("help-worktree");
    assert_snapshot!("worktree_help", help(&scratch, &["--help", "worktree"]));
}

#[test]
fn worktree_acquire_help_snapshot() {
    let scratch = Scratch::new("help-worktree-acquire");
    assert_snapshot!(
        "worktree_acquire_help",
        help(&scratch, &["--help", "worktree", "acquire"])
    );
}

#[test]
fn worktree_list_help_snapshot() {
    let scratch = Scratch::new("help-worktree-list");
    assert_snapshot!(
        "worktree_list_help",
        help(&scratch, &["--help", "worktree", "list"])
    );
}

#[test]
fn worktree_prune_help_snapshot() {
    let scratch = Scratch::new("help-worktree-prune");
    assert_snapshot!(
        "worktree_prune_help",
        help(&scratch, &["--help", "worktree", "prune"])
    );
}
