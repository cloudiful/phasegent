use super::support::*;
use super::*;

use crate::worktree::{find_active_lease_for_probe, probe_path};

#[test]
fn probe_missing_path_reports_a_structured_state_error() {
    let missing = crate::test_scratch::root().join(format!(
        "phasegent-probe-missing-{}-{}",
        std::process::id(),
        now_unix_secs()
    ));
    let _ = std::fs::remove_dir_all(&missing);
    let runner = ProcessWorktreeRunner::new();
    let facts = probe_path(&runner, &missing);
    assert!(!facts.exists, "a missing path must report exists=false");
    assert!(!facts.is_git_worktree);
    assert_eq!(facts.clean, None, "no checkout means unknown cleanliness");
    assert_eq!(facts.branch, None);
    assert_eq!(facts.head, None);
    assert!(
        facts
            .errors
            .iter()
            .any(|error| error.kind == "state" && error.message.contains("does not exist")),
        "a missing path must carry an actionable error: {:?}",
        facts.errors
    );
}

#[test]
fn probe_non_git_directory_reports_not_a_worktree() {
    let dir = TempDir::new("probe-non-git");
    let runner = ProcessWorktreeRunner::new();
    let facts = probe_path(&runner, dir.path());
    assert!(facts.exists);
    assert!(
        !facts.is_git_worktree,
        "a plain directory is not a worktree"
    );
    assert_eq!(facts.clean, None);
    assert!(
        facts
            .errors
            .iter()
            .any(|error| error.message.contains("not a Git work tree")),
        "a non-Git path must explain why facts are absent: {:?}",
        facts.errors
    );
}

#[test]
fn probe_main_checkout_reports_branch_head_and_main_flag() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("probe-main") else {
        return;
    };
    let runner = ProcessWorktreeRunner::new();
    let facts = probe_path(&runner, repo.dir.path());
    assert!(facts.exists);
    assert!(facts.is_git_worktree);
    assert_eq!(facts.clean, Some(true), "a fresh checkout is clean");
    assert_eq!(facts.branch.as_deref(), Some(repo.head_branch.as_str()));
    assert!(facts.head.is_some(), "a committed repo has a HEAD sha");
    assert_eq!(facts.is_main_checkout, Some(true));
    assert!(
        facts.errors.is_empty(),
        "a healthy probe reports no errors: {:?}",
        facts.errors
    );
}

#[test]
fn probe_dirty_checkout_reports_not_clean() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("probe-dirty") else {
        return;
    };
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "work in progress\n").expect("write scratch");
    let runner = ProcessWorktreeRunner::new();
    let facts = probe_path(&runner, repo.dir.path());
    assert_eq!(facts.clean, Some(false), "uncommitted work is dirty");
    let _ = std::fs::remove_file(&scratch);
}

#[test]
fn probe_linked_worktree_is_not_the_main_checkout() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("probe-linked") else {
        return;
    };
    let target = repo.dir.path().join("linked");
    let branch = "phasegent/595-probe0";
    worktree_add(
        &ProcessWorktreeRunner::new(),
        repo.dir.path(),
        &target,
        branch,
    )
    .expect("worktree add");
    let runner = ProcessWorktreeRunner::new();
    let facts = probe_path(&runner, &target);
    assert!(facts.exists && facts.is_git_worktree);
    assert_eq!(facts.clean, Some(true));
    assert_eq!(facts.branch.as_deref(), Some(branch));
    assert_eq!(
        facts.is_main_checkout,
        Some(false),
        "a linked worktree must not be reported as the main checkout"
    );
}

#[test]
fn probe_reports_unknown_cleanliness_when_git_status_fails() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("probe-unknown") else {
        return;
    };
    std::fs::write(repo.dir.path().join(".git/index"), b"not-a-valid-index")
        .expect("corrupt git index");
    let runner = ProcessWorktreeRunner::new();
    let facts = probe_path(&runner, repo.dir.path());
    assert_eq!(
        facts.clean, None,
        "a failing git status is unknown, never clean"
    );
    assert!(
        facts
            .errors
            .iter()
            .any(|error| error.message.contains("dirty probe failed")),
        "the failed probe must be reported: {:?}",
        facts.errors
    );
}

#[test]
fn probe_non_repo_reports_false_from_a_non_zero_rev_parse() {
    let runner = FakeWorktreeRunner::new(vec![FakeResponse {
        args: vec!["rev-parse".to_string(), "--is-inside-work-tree".to_string()],
        status: 128,
        stdout: "fatal: not a git repository".to_string(),
    }]);
    let facts = probe_path(&runner, Path::new("/tmp/phasegent-probe-not-a-repo"));
    assert!(!facts.is_git_worktree);
    assert_eq!(facts.clean, None);
}

#[test]
fn probe_resolves_the_issue_lease_with_optional_session() {
    let _lock = lock_workflow_tests();
    let (db_temp, storage, _env) = open_temp_db("probe-lease");
    crate::worktree::ensure_schema(&storage).expect("ensure_schema");
    let identity = "/tmp/phasegent-probe-lease/.git";
    let now = now_unix_secs();
    for (lease_id, session) in [("lease-a", "session-A"), ("lease-b", "session-B")] {
        insert_lease(
            &storage,
            NewLease {
                lease_id,
                identity,
                issue: 595,
                session,
                checkout_path: "/tmp/phasegent-probe-lease",
                worktree_path: &format!("/tmp/phasegent-probe-lease/{session}"),
                branch: "phasegent/595-aaaaaa",
                status: LEASE_STATUS_ACTIVE,
                created_at: now,
                heartbeat_at: now,
            },
        )
        .expect("insert lease");
    }
    let any = find_active_lease_for_probe(&storage, identity, 595, None)
        .expect("resolve lease")
        .expect("a lease exists");
    assert_eq!(any.issue, 595);
    let narrowed = find_active_lease_for_probe(&storage, identity, 595, Some("session-B"))
        .expect("resolve narrow lease")
        .expect("session-B lease exists");
    assert_eq!(narrowed.session, "session-B");
    let missing =
        find_active_lease_for_probe(&storage, identity, 600, None).expect("resolve missing");
    assert!(missing.is_none(), "an unknown issue must not guess a lease");
    drop(db_temp);
}
