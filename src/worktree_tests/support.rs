use super::*;

#[derive(Debug, Clone)]
pub(crate) struct FakeResponse {
    /// Matched as a prefix of the actual argv (after `git`).
    pub(crate) args: Vec<String>,
    pub(crate) status: i32,
    pub(crate) stdout: String,
}

pub(crate) struct FakeWorktreeRunner {
    pub(crate) responses: RefCell<Vec<FakeResponse>>,
    pub(crate) calls: RefCell<Vec<(Vec<String>, PathBuf)>>,
}

impl FakeWorktreeRunner {
    pub(crate) fn new(responses: Vec<FakeResponse>) -> Self {
        Self {
            responses: RefCell::new(responses),
            calls: RefCell::new(Vec::new()),
        }
    }

    pub(crate) fn recorded(&self) -> Vec<(Vec<String>, PathBuf)> {
        self.calls.borrow().clone()
    }
}

impl WorktreeRunner for FakeWorktreeRunner {
    fn run(&self, args: &[&str], workdir: &Path) -> Result<GitOutput, WorktreeError> {
        let argv: Vec<String> = args.iter().map(|value| value.to_string()).collect();
        self.calls
            .borrow_mut()
            .push((argv.clone(), workdir.to_path_buf()));
        for response in self.responses.borrow().iter() {
            if argv.starts_with(&response.args) {
                return Ok(GitOutput {
                    status: response.status,
                    stdout: response.stdout.clone(),
                });
            }
        }
        Err(WorktreeError::new(
            "git",
            format!("unexpected git invocation {argv:?}"),
        ))
    }
}

pub(crate) struct TempDir(pub(crate) PathBuf);

impl TempDir {
    pub(crate) fn new(label: &str) -> Self {
        let dir = crate::test_scratch::root().join(format!(
            "phasegent-wt-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir create");
        Self(dir)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) struct TempRepo {
    pub(crate) dir: TempDir,
    pub(crate) head_branch: String,
}

impl TempRepo {
    pub(crate) fn init(label: &str) -> Option<Self> {
        let dir = TempDir::new(label);
        let runner = ProcessWorktreeRunner::new();
        if runner
            .run(&["init", "-q", "-b", "main"], dir.path())
            .is_err()
        {
            return None;
        }
        // Configure a committer identity so the initial commit works
        // even in a host with no global git config.
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
            dir.path(),
        );
        // If the initial commit failed (e.g. test runs in a CI without
        // user setup), the test still has a valid repo we can reason
        // about: `git worktree add` falls back to creating an empty
        // branch when HEAD is unborn. We just record whatever branch
        // `symbolic-ref` reports.
        let branch = match runner.run(&["symbolic-ref", "--quiet", "--short", "HEAD"], dir.path()) {
            Ok(out) if out.status == 0 => out.stdout.trim().to_string(),
            _ => "main".to_string(),
        };
        Some(Self {
            dir,
            head_branch: branch,
        })
    }

    /// Configure an `origin` remote so `lifecycle::origin_identity`
    /// resolves an OWNER/REPO slug (the managed-hook install gate).
    pub(crate) fn set_origin(&self, url: &str) {
        let runner = ProcessWorktreeRunner::new();
        let output = runner
            .run(&["remote", "add", "origin", url], self.dir.path())
            .expect("git remote add runs");
        assert_eq!(output.status, 0, "origin must be configured");
    }
}

/// Hook path as Git resolves it *inside* `checkout`, so the assertion
/// covers the exact hooks directory the acquire lifecycle installed into
/// (linked worktrees share the common hooks dir).
pub(crate) fn hook_path_in(checkout: &Path, name: &str) -> PathBuf {
    let runner = ProcessWorktreeRunner::new();
    let output = runner
        .run(&["rev-parse", "--git-path", "hooks"], checkout)
        .expect("git rev-parse runs");
    assert_eq!(output.status, 0, "git rev-parse --git-path hooks");
    checkout.join(output.stdout.trim()).join(name)
}

pub(crate) fn open_temp_db(label: &str) -> (TempDir, Storage, EnvGuard) {
    let temp = TempDir::new(label);
    let db = temp.path().join("phasegent.sqlite3");
    let env = EnvGuard::set(
        "PHASEGENT_DB_PATH",
        db.as_os_str().to_string_lossy().as_ref(),
    );
    let storage = Storage::open_at(&db).expect("storage open");
    (temp, storage, env)
}

pub(crate) fn unique_cache(label: &str) -> TempDir {
    TempDir::new(&format!("cache-{label}"))
}

/// Bind the temp repo's current branch to `issue` using the same local
/// git config key `branch_context::read_issue_id` reads
/// (`branch.<name>.redmine-issue-id`), so binding resolution in the
/// dirty-tree triggers sees exactly what a hand-written binding sets.
pub(crate) fn bind_current_branch(repo: &TempRepo, issue: u64) {
    let runner = ProcessWorktreeRunner::new();
    let key = crate::branch_context::config_key(&repo.head_branch);
    let issue_text = issue.to_string();
    let output = runner
        .run(
            &["config", "--local", key.as_str(), &issue_text],
            repo.dir.path(),
        )
        .expect("git config binding write");
    assert_eq!(output.status, 0, "binding write must succeed");
}

// ---------------------------------------------------------------------------
// Pure helper tests (no DB, no git).
// ---------------------------------------------------------------------------

pub(crate) fn insert_lease_row(
    storage: &Storage,
    lease_id: &str,
    identity: &str,
    status: &str,
    heartbeat_at: i64,
    worktree_path: &str,
) {
    insert_lease(
        storage,
        NewLease {
            lease_id,
            identity,
            issue: 1,
            session: "session-A",
            checkout_path: "/tmp/checkout",
            worktree_path,
            branch: "phasegent/1-aaaaaa",
            status,
            created_at: heartbeat_at,
            heartbeat_at,
        },
    )
    .expect("insert lease row");
}

pub(crate) fn heartbeat_of(identity: &str, lease_id: &str) -> i64 {
    leases_for_repo(identity)
        .expect("list leases")
        .into_iter()
        .find(|row| row.lease_id == lease_id)
        .expect("lease row")
        .heartbeat_at
}
