use super::support::*;
use super::*;

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

pub(crate) fn prune_command(
    repo: &TempRepo,
    release_stale: bool,
    remove: bool,
    reason: Option<&str>,
) -> WorktreeCommand {
    WorktreeCommand::Prune {
        repo: Some(repo.dir.path().to_string_lossy().to_string()),
        stale_days: 14,
        release_stale,
        remove,
        reason: reason.map(str::to_owned),
        no_sync: false,
    }
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

/// Run `action` with the process cwd temporarily inside the temp repo.
pub(crate) fn in_temp_repo<T>(repo: &TempRepo, action: impl FnOnce() -> T) -> T {
    let previous_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    std::env::set_current_dir(repo.dir.path()).expect("set cwd to temp repo");
    let result = action();
    let _ = std::env::set_current_dir(&previous_cwd);
    result
}

pub(crate) fn bind_issue_command(issue_id: u64, session: Option<&str>) -> IssueCommand {
    IssueCommand::Bind {
        issue_id,
        replace: false,
        session: session.map(Into::into),
    }
}

/// The branch binding `read_issue_id` would see, or `None` when unset.
pub(crate) fn read_branch_binding(repo: &TempRepo) -> Option<String> {
    let runner = ProcessWorktreeRunner::new();
    let key = crate::branch_context::config_key(&repo.head_branch);
    let output = runner
        .run(
            &["config", "--local", "--get", key.as_str()],
            repo.dir.path(),
        )
        .expect("git config read");
    if output.status == 0 {
        Some(output.stdout.trim().to_owned())
    } else {
        None
    }
}
