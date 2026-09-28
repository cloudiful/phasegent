use super::*;
use crate::branch_links::{self, ActiveIssue, IssueKey};

fn key(number: u64) -> IssueKey {
    IssueKey::from_number("redmine", "tools/phasegent", number).expect("issue key")
}

#[test]
fn many_to_many_links_and_explicit_detach_history() {
    let connection = open_memory_db();
    let repo = "forge.example.com/owner/repo";
    let issue_a = key(616);
    let issue_b = key(628);

    for (branch, issue, number) in [
        ("feat/628", &issue_b, 628),
        ("feat/628", &issue_a, 616),
        ("feat/616", &issue_a, 616),
    ] {
        branch_links::store::link(
            &connection,
            &branch_links::LinkParams {
                repo_key: repo,
                branch,
                issue,
                issue_number: number,
                source: "manual",
                now: 1_700_000_001,
            },
        )
        .expect("link must insert");
    }

    let forward = branch_links::issues_for_branch(
        &connection,
        repo,
        "feat/628",
        false,
        &branch_links::UnknownState,
    )
    .expect("forward read");
    assert_eq!(forward.len(), 2);

    let reverse = branch_links::branches_for_issue(
        &connection,
        repo,
        &issue_a,
        false,
        &branch_links::UnknownState,
    )
    .expect("reverse read");
    let branches: Vec<_> = reverse.iter().map(|entry| entry.branch.as_str()).collect();
    assert_eq!(branches, vec!["feat/616", "feat/628"]);

    let outcome = branch_links::store::detach(
        &connection,
        repo,
        "feat/628",
        &issue_a,
        "manual",
        1_700_000_010,
    )
    .expect("detach must work");
    assert_eq!(outcome, branch_links::DetachOutcome::Detached);

    let active_only = branch_links::issues_for_branch(
        &connection,
        repo,
        "feat/628",
        false,
        &branch_links::UnknownState,
    )
    .expect("active read");
    assert_eq!(active_only.len(), 1);

    let with_history = branch_links::issues_for_branch(
        &connection,
        repo,
        "feat/628",
        true,
        &branch_links::UnknownState,
    )
    .expect("history read");
    assert_eq!(with_history.len(), 2);
    let detached = with_history
        .iter()
        .find(|entry| entry.issue == issue_a)
        .expect("detached row retained");
    assert_eq!(detached.status, "detached");
    assert!(detached.detached_at.is_some());
}

#[test]
fn link_is_idempotent_and_relink_restores_detached_row() {
    let connection = open_memory_db();
    let repo = "forge.example.com/owner/repo";
    let issue = key(628);
    let params = branch_links::LinkParams {
        repo_key: repo,
        branch: "feat/628",
        issue: &issue,
        issue_number: 628,
        source: "manual",
        now: 1_700_000_001,
    };
    assert_eq!(
        branch_links::store::link(&connection, &params).expect("first link"),
        branch_links::LinkOutcome::Created
    );
    assert_eq!(
        branch_links::store::link(&connection, &params).expect("second link"),
        branch_links::LinkOutcome::AlreadyLinked
    );
    branch_links::store::detach(
        &connection,
        repo,
        "feat/628",
        &issue,
        "manual",
        1_700_000_002,
    )
    .expect("detach");
    assert_eq!(
        branch_links::store::link(&connection, &params).expect("relink"),
        branch_links::LinkOutcome::Relinked
    );
}

#[test]
fn active_issue_is_never_guessed_for_multiple_links() {
    let connection = open_memory_db();
    let repo = "forge.example.com/owner/repo";
    let issue_a = key(616);
    let issue_b = key(628);
    for (issue, number) in [(&issue_a, 616), (&issue_b, 628)] {
        branch_links::store::link(
            &connection,
            &branch_links::LinkParams {
                repo_key: repo,
                branch: "feat/shared",
                issue,
                issue_number: number,
                source: "manual",
                now: 1_700_000_001,
            },
        )
        .expect("link");
    }
    let links = branch_links::issues_for_branch(
        &connection,
        repo,
        "feat/shared",
        false,
        &branch_links::UnknownState,
    )
    .expect("read");
    match branch_links::resolve_active_issue(&links) {
        ActiveIssue::Ambiguous(keys) => assert_eq!(keys.len(), 2),
        other => panic!("expected ambiguous, got {other:?}"),
    }

    let empty = branch_links::issues_for_branch(
        &connection,
        repo,
        "feat/missing",
        false,
        &branch_links::UnknownState,
    )
    .expect("read");
    assert_eq!(
        branch_links::resolve_active_issue(&empty),
        ActiveIssue::None
    );
}
