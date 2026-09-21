use super::*;

pub(crate) fn strings<const N: usize>(values: [&str; N]) -> Vec<String> {
    values.into_iter().map(str::to_owned).collect()
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

pub(crate) fn fresh_lease_row(
    lease_id: &str,
    issue: u64,
    session: &str,
    worktree_path: &str,
    branch: &str,
    status: &str,
    heartbeat_at: i64,
) -> LeaseRow {
    LeaseRow {
        lease_id: lease_id.to_owned(),
        repo_identity: "/tmp/repo".to_owned(),
        issue,
        session: session.to_owned(),
        checkout_path: "/tmp/repo".to_owned(),
        worktree_path: worktree_path.to_owned(),
        branch: branch.to_owned(),
        status: status.to_owned(),
        created_at: heartbeat_at,
        heartbeat_at,
        release_reason: None,
    }
}

pub(crate) struct TempDir(pub(crate) PathBuf);

impl TempDir {
    pub(crate) fn new(label: &str) -> Self {
        let dir = crate::test_scratch::root().join(format!(
            "phasegent-wt-cli-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir create");
        Self(dir)
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn insert_row(storage: &Storage, row: &LeaseRow) {
    insert_lease(
        storage,
        NewLease {
            lease_id: &row.lease_id,
            identity: &row.repo_identity,
            issue: row.issue,
            session: &row.session,
            checkout_path: &row.checkout_path,
            worktree_path: &row.worktree_path,
            branch: &row.branch,
            status: &row.status,
            created_at: row.created_at,
            heartbeat_at: row.heartbeat_at,
        },
    )
    .expect("insert row");
}

// ---------------------------------------------------------------------------
// CLI parsing
// ---------------------------------------------------------------------------

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
        let branch = match runner.run(&["symbolic-ref", "--quiet", "--short", "HEAD"], dir.path()) {
            Ok(out) if out.status == 0 => out.stdout.trim().to_string(),
            _ => "main".to_string(),
        };
        Some(Self {
            dir,
            head_branch: branch,
        })
    }
}

/// Bind the temp repo's current branch to `issue` through the same local
/// git config key `branch_context::read_issue_id` reads.
pub(crate) fn bind_current_branch(repo: &TempRepo, issue: u64) {
    let runner = ProcessWorktreeRunner::new();
    let key = crate::branch_context::config_key(&repo.head_branch);
    let output = runner
        .run(
            &["config", "--local", key.as_str(), &issue.to_string()],
            repo.dir.path(),
        )
        .expect("git config binding write");
    assert_eq!(output.status, 0, "binding write must succeed");
}

/// `(temp_dir, cache_dir, db_env, cache_env)` where `db_env` pins
/// `PHASEGENT_DB_PATH` and `cache_env` pins `PHASEGENT_WORKTREE_CACHE_DIR`
/// so an `acquire_lease(..., None)` call stays entirely inside temp dirs.
pub(crate) fn open_temp_db_and_cache(label: &str) -> (TempDir, TempDir, EnvGuard, EnvGuard) {
    let temp = TempDir::new(&format!("{label}-db"));
    let db = temp.path().join("phasegent.sqlite3");
    let db_env = EnvGuard::set(
        "PHASEGENT_DB_PATH",
        db.as_os_str().to_string_lossy().as_ref(),
    );
    let cache = TempDir::new(&format!("{label}-cache"));
    let cache_env = EnvGuard::set(
        "PHASEGENT_WORKTREE_CACHE_DIR",
        cache.path().as_os_str().to_string_lossy().as_ref(),
    );
    (temp, cache, db_env, cache_env)
}

/// Assert the serialised CLI acquire envelope carries exactly the six
/// documented fields (no `warnings` key) with the given decision values.
pub(crate) fn assert_cli_envelope(
    outcome: AcquireOutcome,
    expected_created: bool,
    expected_reason: &str,
) -> serde_json::Value {
    let json = serde_json::to_value(AcquireJson::from(outcome)).expect("envelope serialise");
    let object = json.as_object().expect("envelope must be an object");
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "branch",
            "created",
            "lease_id",
            "path",
            "reason",
            "repo_identity"
        ],
        "CLI envelope fields must stay stable"
    );
    assert_eq!(json["created"], serde_json::json!(expected_created));
    assert_eq!(json["reason"], serde_json::json!(expected_reason));
    json
}
