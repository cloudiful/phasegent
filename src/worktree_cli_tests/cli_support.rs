use super::support::*;
use super::*;

use crate::worktree::opencode::ProcessOpenCodeApi;

pub(crate) fn run_cli_acquire_in_temp_repo(
    repo: &TempRepo,
    _db_path: &std::path::Path,
    isolate: bool,
) -> i32 {
    run_cli_acquire_with_session(repo, isolate, Some("session-A"))
}

pub(crate) fn run_cli_acquire_with_session(
    repo: &TempRepo,
    isolate: bool,
    session: Option<&str>,
) -> i32 {
    let previous_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    std::env::set_current_dir(repo.dir.path()).expect("set cwd to temp repo");
    let exit = execute_worktree(
        Some(Role::Orchestrator),
        WorktreeCommand::Acquire {
            issue: 245,
            session: session.map(str::to_owned),
            base: None,
            format: "json".to_owned(),
            isolate,
            no_sync: false,
        },
    );
    let _ = std::env::set_current_dir(&previous_cwd);
    exit
}

pub(crate) fn corrupt_git_index(repo: &TempRepo) {
    std::fs::write(repo.dir.path().join(".git/index"), b"not-a-valid-index")
        .expect("corrupt git index");
}

pub(crate) fn insert_lease_for_identity(
    storage: &Storage,
    lease_id: &str,
    identity: &str,
    session: &str,
    status: &str,
    heartbeat_at: i64,
    worktree_path: &str,
) {
    insert_lease(
        storage,
        NewLease {
            lease_id,
            identity,
            issue: 7,
            session,
            checkout_path: "/tmp/checkout",
            worktree_path,
            branch: "phasegent/7-aaaaaa",
            status,
            created_at: heartbeat_at,
            heartbeat_at,
        },
    )
    .expect("insert row");
}

/// Run `worktree prune` for `repo` through the injectable core with a
/// scripted issue-state source, so no test reaches a real provider. `stale_days`
/// is 999 by default in the issue 916 tests: directory removal must not depend
/// on it. The production host and a no-op progress sink are used.
pub(crate) fn run_scoped_prune(
    repo: &TempRepo,
    stale_days: u32,
    release_stale: bool,
    reason: Option<&str>,
    dry_run: bool,
    format: &str,
    source: &dyn IssueStateSource,
) -> i32 {
    let path = repo.dir.path().to_string_lossy().into_owned();
    execute_prune_with_source(
        Some(&path),
        stale_days,
        release_stale,
        reason.map(str::to_owned),
        dry_run,
        format,
        source,
        &ProcessOpenCodeApi::new(),
        &|_| {},
    )
}

/// Run the global (no `--repo`) prune through the injectable core.
pub(crate) fn run_global_prune(
    stale_days: u32,
    dry_run: bool,
    format: &str,
    source: &dyn IssueStateSource,
) -> i32 {
    execute_prune_with_source(
        None,
        stale_days,
        false,
        None,
        dry_run,
        format,
        source,
        &ProcessOpenCodeApi::new(),
        &|_| {},
    )
}

/// Create a real linked worktree on a fresh generated branch and return
/// its path. Uses the same wrapper the production acquire path uses so
/// prune runs against a real worktree rather than a stub directory.
pub(crate) fn add_real_worktree(repo: &TempRepo, branch: &str) -> PathBuf {
    let runner = ProcessWorktreeRunner::new();
    let target = repo.dir.path().join(branch.replace('/', "-"));
    worktree_add(&runner, repo.dir.path(), &target, branch).expect("worktree add");
    target
}

pub(crate) fn branch_exists(repo: &TempRepo, branch: &str) -> bool {
    let runner = ProcessWorktreeRunner::new();
    runner
        .run(&["branch", "--list", branch], repo.dir.path())
        .map(|out| out.status == 0 && !out.stdout.trim().is_empty())
        .unwrap_or(false)
}

/// Run `action` with the process cwd temporarily set to `dir`.
pub(crate) fn in_dir<T>(dir: &std::path::Path, action: impl FnOnce() -> T) -> T {
    let previous_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    std::env::set_current_dir(dir).expect("set cwd");
    let result = action();
    let _ = std::env::set_current_dir(&previous_cwd);
    result
}

/// Run `action` with the process cwd temporarily inside the temp repo.
pub(crate) fn in_temp_repo<T>(repo: &TempRepo, action: impl FnOnce() -> T) -> T {
    let previous_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    std::env::set_current_dir(repo.dir.path()).expect("set cwd to temp repo");
    let result = action();
    let _ = std::env::set_current_dir(&previous_cwd);
    result
}

pub(crate) fn bind_issue_command(issue_id: u64) -> IssueCommand {
    IssueCommand::Bind { issue_id }
}
