use super::support::*;
use super::*;

#[test]
fn worktree_add_and_remove_create_then_drop_a_real_worktree() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("wt-add-remove") else {
        return;
    };
    let runner = ProcessWorktreeRunner::new();
    let target = repo.dir.path().join("wt-test");
    worktree_add(&runner, repo.dir.path(), &target, "phasegent/239-aaaaaa").expect("worktree add");
    assert!(target.exists());
    let porcelain = Command::new("git")
        .arg("-C")
        .arg(target.to_string_lossy().to_string())
        .args(["status", "--porcelain"])
        .output();
    if let Ok(out) = porcelain {
        assert_eq!(out.status.code().unwrap_or(-1), 0);
    }
    worktree_remove(&runner, repo.dir.path(), &target).expect("worktree remove");
    assert!(!target.exists());
}

#[test]
fn is_clean_returns_true_for_pristine_and_false_for_dirty() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("is-clean") else {
        return;
    };
    let runner = ProcessWorktreeRunner::new();
    assert!(is_clean(&runner, repo.dir.path()).expect("clean check"));
    let dirty = repo.dir.path().join("scratch.txt");
    std::fs::write(&dirty, "scratch\n").expect("write scratch");
    assert!(!is_clean(&runner, repo.dir.path()).expect("dirty check"));
    let _ = std::fs::remove_file(&dirty);
}

#[test]
fn is_clean_reports_structured_error_for_git_failure() {
    let _lock = lock_workflow_tests();
    let runner = FakeWorktreeRunner::new(vec![FakeResponse {
        args: vec!["status".to_string(), "--porcelain".to_string()],
        status: 128,
        stdout: "fatal: not a git repo".to_string(),
    }]);
    let error = is_clean(&runner, Path::new("/tmp/nope")).unwrap_err();
    assert_eq!(error.kind, "git");
}

#[test]
fn worktree_add_surfaces_git_failure_as_structured_error() {
    let _lock = lock_workflow_tests();
    let runner = FakeWorktreeRunner::new(vec![FakeResponse {
        args: vec!["worktree".to_string(), "add".to_string()],
        status: 128,
        stdout: "fatal: bad ref".to_string(),
    }]);
    let error = worktree_add(
        &runner,
        Path::new("/tmp/repo"),
        Path::new("/tmp/repo/wt"),
        "phasegent/1-aaaaaa",
    )
    .unwrap_err();
    assert_eq!(error.kind, "git");
}
