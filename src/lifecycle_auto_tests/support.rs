use super::*;

pub(crate) fn unique_temp_dir(label: &str) -> std::path::PathBuf {
    crate::test_scratch::root().join(format!(
        "phasegent-auto-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

pub(crate) fn open_temp_storage(label: &str) -> (std::path::PathBuf, Storage, EnvGuard) {
    let temp = unique_temp_dir(label);
    let db = temp.join(crate::infra::storage::DB_FILENAME);
    let env = EnvGuard::set(
        "PHASEGENT_DB_PATH",
        db.as_os_str().to_string_lossy().as_ref(),
    );
    let storage = Storage::open_at(&db).unwrap();
    (temp, storage, env)
}

pub(crate) fn running_for(storage: &Storage, issue: u64) -> Vec<TimerRun> {
    storage
        .list_timer_runs(TimerStatusFilter::Running, 256)
        .unwrap()
        .into_iter()
        .filter(|run| run.issue == issue)
        .collect()
}

pub(crate) fn temp_git_repo(label: &str) -> Option<(std::path::PathBuf, String)> {
    let dir = unique_temp_dir(&format!("close-repo-{label}"));
    fs::create_dir_all(&dir).ok()?;
    let runner = crate::worktree::ProcessWorktreeRunner::new();
    if runner.run(&["init", "-q", "-b", "main"], &dir).is_err() {
        let _ = fs::remove_dir_all(&dir);
        return None;
    }
    // Best-effort initial commit: an unborn HEAD is still a valid repo
    // identity for the lease-release hook under test.
    let _ = runner.run(
        &[
            "-c",
            "user.name=phasegent-test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "init",
        ],
        &dir,
    );
    let identity = crate::worktree::repo_identity(&runner, &dir).ok()?;
    Some((dir, identity))
}

pub(crate) fn seed_lease(identity: &str, issue: u64, session: &str, status: &str) -> String {
    let storage = Storage::open().expect("storage for lease seed");
    crate::worktree::ensure_schema(&storage).expect("worktree schema");
    let lease_id = format!(
        "lease-{issue}-{session}-{}",
        crate::worktree::compute_fingerprint(identity)
    );
    let now = crate::worktree::now_unix_secs();
    let worktree_path = format!("/tmp/phasegent-lease-{issue}-{session}");
    crate::worktree::leases::insert_lease(
        &storage,
        crate::worktree::leases::NewLease {
            lease_id: &lease_id,
            identity,
            issue,
            session,
            checkout_path: "/tmp/phasegent-checkout",
            worktree_path: &worktree_path,
            branch: "main",
            status,
            created_at: now,
            heartbeat_at: now,
        },
    )
    .expect("seed lease");
    lease_id
}

pub(crate) fn lease_state(lease_id: &str) -> (String, Option<String>) {
    let storage = Storage::open().expect("storage for lease read");
    let row = crate::worktree::leases::load_lease(&storage, lease_id)
        .expect("load lease")
        .expect("lease exists");
    (row.status, row.release_reason)
}

/// Add a linked worktree at `<scratch>/cleanup-wt-<label>` on a fresh
/// `branch` and return its path; the caller removes the path.
pub(crate) fn add_cleanup_worktree(
    repo_dir: &std::path::Path,
    label: &str,
    branch: &str,
) -> std::path::PathBuf {
    let dir = unique_temp_dir(&format!("cleanup-wt-{label}"));
    let runner = crate::worktree::ProcessWorktreeRunner::new();
    let output = runner
        .run(
            &[
                "worktree",
                "add",
                dir.to_str().expect("utf8 worktree path"),
                "-b",
                branch,
                "HEAD",
            ],
            repo_dir,
        )
        .expect("git worktree add runs");
    assert_eq!(output.status, 0, "git worktree add must succeed");
    dir
}

/// Seed a lease row pointing at a real worktree path so the cleanup
/// helper sees a directory on disk instead of the placeholder paths the
/// close-release tests use.
pub(crate) fn seed_lease_at(
    identity: &str,
    issue: u64,
    session: &str,
    status: &str,
    worktree_path: &std::path::Path,
) -> String {
    let storage = Storage::open().expect("storage for lease seed");
    crate::worktree::ensure_schema(&storage).expect("worktree schema");
    let lease_id = format!(
        "lease-cleanup-{issue}-{session}-{}",
        crate::worktree::compute_fingerprint(identity)
    );
    let now = crate::worktree::now_unix_secs();
    crate::worktree::leases::insert_lease(
        &storage,
        crate::worktree::leases::NewLease {
            lease_id: &lease_id,
            identity,
            issue,
            session,
            checkout_path: "/tmp/phasegent-cleanup-checkout",
            worktree_path: worktree_path.to_str().expect("utf8 worktree path"),
            branch: "main",
            status,
            created_at: now,
            heartbeat_at: now,
        },
    )
    .expect("seed lease");
    lease_id
}

pub(crate) fn branch_exists(repo_dir: &std::path::Path, branch: &str) -> bool {
    let runner = crate::worktree::ProcessWorktreeRunner::new();
    let reference = format!("refs/heads/{branch}");
    runner
        .run(&["show-ref", "--verify", "--quiet", &reference], repo_dir)
        .expect("git show-ref runs")
        .status
        == 0
}

pub(crate) fn cleanup_outcome(
    repo_dir: &std::path::Path,
    issue: u64,
    session: Option<&str>,
) -> crate::lifecycle::AutoCleanupOutcome {
    crate::lifecycle::cleanup_closed_issue_worktrees(
        &crate::worktree::ProcessWorktreeRunner::new(),
        repo_dir,
        issue,
        session,
    )
}
