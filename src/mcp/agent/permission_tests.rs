//! Permission-decision regression tests.
//!
//! The read-only explorer contract fails closed on two axes, and both
//! are exercised here through a real `session/request_permission` round
//! trip: the tool *kind* must be a read, and the *paths* the call names
//! must stay inside the worktree. The second axis is what keeps the
//! phasegent credential store and the agent's own configuration
//! unreachable from a model-driven turn. `fetch` is the one read kind
//! with no path, so it is bounded by a URL contract instead.

use super::protocol_tests::connect;
use super::test_kit::PermissionScript;
use super::types::ExplorerPrompt;

/// One scripted permission request, with the outcome the adapter chose
/// and the config selections the turn made before it.
async fn permission_outcome(
    script: PermissionScript,
) -> (serde_json::Value, Vec<(String, String)>) {
    let (session, agent) = connect(super::test_kit::FakeAgentOptions {
        permission_request: Some(script),
        ..Default::default()
    })
    .await;
    session.negotiate_explorer().await.expect("negotiate");
    session
        .prompt(&ExplorerPrompt::new("run the tool"))
        .await
        .expect("prompt completes");
    let outcomes = agent.permission_outcomes.lock().await.clone();
    assert_eq!(outcomes.len(), 1, "one permission reply expected");
    let selections = agent.selections.lock().await.clone();
    (outcomes[0].clone(), selections)
}

fn selected_option(outcome: &serde_json::Value) -> Option<&str> {
    outcome["outcome"]["optionId"].as_str()
}

#[tokio::test]
async fn write_permission_request_is_denied_not_allowed() {
    let (outcome, _) = permission_outcome(PermissionScript::write("src/main.rs")).await;
    assert_eq!(outcome["outcome"]["outcome"], "selected");
    assert_eq!(selected_option(&outcome), Some("deny"));
}

#[tokio::test]
async fn search_inside_the_worktree_is_allowed() {
    // `search` is the explorer's primary tool: denying it made the
    // delegation unusable.
    let (outcome, selections) =
        permission_outcome(PermissionScript::search("fn main", Some("src"))).await;
    assert_eq!(selected_option(&outcome), Some("allow-once"));
    assert_eq!(
        selections.len(),
        3,
        "the turn negotiated before the tool ran"
    );

    // A path-less search is bounded by the process cwd, which is the
    // worktree.
    let (outcome, _) = permission_outcome(PermissionScript::search("**/*.rs", None)).await;
    assert_eq!(selected_option(&outcome), Some("allow-once"));
}

#[tokio::test]
async fn read_outside_the_worktree_is_denied_even_for_a_read_kind() {
    for path in [
        "/home/dev/.config/phasegent/phasegent.sqlite3",
        "/home/dev/.minimax/auth.json",
        "/home/dev/.ssh/id_ed25519",
        "../../../etc/shadow",
    ] {
        let (outcome, _) = permission_outcome(PermissionScript::read(path)).await;
        assert_eq!(
            selected_option(&outcome),
            Some("deny"),
            "{path} must be denied"
        );
    }
    // An escape that only appears in a nested input field is caught too.
    let (outcome, _) = permission_outcome(PermissionScript::read_nested_escape(
        "token",
        "/home/dev/.config/phasegent/phasegent.sqlite3",
    ))
    .await;
    assert_eq!(selected_option(&outcome), Some("deny"));
}

#[tokio::test]
async fn reads_inside_the_worktree_are_allowed() {
    let (outcome, _) = permission_outcome(PermissionScript::read("src/main.rs")).await;
    assert_eq!(selected_option(&outcome), Some("allow-once"));
}

#[tokio::test]
async fn a_fetch_must_name_a_credential_free_absolute_url() {
    // `fetch` names no path, so the worktree check is vacuous for it: it
    // needs its own bound, or a model-driven turn has an unbounded
    // network read.
    let (outcome, _) =
        permission_outcome(PermissionScript::fetch("https://docs.example.com/guide")).await;
    assert_eq!(selected_option(&outcome), Some("allow-once"));

    for script in [
        PermissionScript::fetch_without_a_target(),
        PermissionScript::fetch("/etc/shadow"),
        PermissionScript::fetch("file:///etc/passwd"),
        PermissionScript::fetch("https://user:s3cret@example.com/x"),
    ] {
        let (outcome, _) = permission_outcome(script).await;
        assert_eq!(
            selected_option(&outcome),
            Some("deny"),
            "a fetch outside the URL contract must be denied"
        );
    }
}
