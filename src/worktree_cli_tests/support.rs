use super::*;

pub(crate) fn strings<const N: usize>(values: [&str; N]) -> Vec<String> {
    values.into_iter().map(str::to_owned).collect()
}

pub(crate) struct TempDir(pub(crate) PathBuf);

impl TempDir {
    pub(crate) fn new(label: &str) -> Self {
        let dir = crate::test_scratch::root().join(format!(
            "phasegent-wtcli-{label}-{}-{}",
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

pub(crate) fn open_temp_db(label: &str) -> (TempDir, EnvGuard) {
    let temp = TempDir::new(label);
    let db = temp.path().join("phasegent.sqlite3");
    let env = EnvGuard::set(
        "PHASEGENT_DB_PATH",
        db.as_os_str().to_string_lossy().as_ref(),
    );
    (temp, env)
}

pub(crate) fn temp_cache(label: &str) -> TempDir {
    TempDir::new(&format!("cache-{label}"))
}

pub(crate) fn open_records(identity: &str) -> Vec<crate::worktree::DirectoryRecord> {
    let storage = Storage::open().expect("temp storage opens");
    crate::worktree::registry::load_records(&storage, Some(identity)).expect("load records")
}

pub(crate) fn record_directory(
    identity: &str,
    checkout: &std::path::Path,
    worktree: &std::path::Path,
    branch: &str,
    issue: u64,
    session: &str,
) -> crate::worktree::DirectoryRecord {
    let storage = Storage::open().expect("temp storage opens");
    crate::worktree::registry::ensure_schema(&storage).expect("registry schema");
    let canonical = crate::worktree::canonical_or_self(worktree);
    let directory_id =
        crate::worktree::registry::stable_directory_id(identity, &canonical.to_string_lossy());
    crate::worktree::registry::insert_directory(
        &storage,
        &crate::worktree::registry::NewDirectory {
            directory_id: &directory_id,
            repo_identity: identity,
            checkout_path: &checkout.to_string_lossy(),
            worktree_path: &canonical.to_string_lossy(),
            branch,
            created_at: crate::worktree::now_unix_secs().max(1),
            session,
            associations: &[crate::worktree::registry::NewAssociation::Unscoped { issue }],
        },
    )
    .expect("insert directory");
    crate::worktree::registry::load_records(&storage, Some(identity))
        .expect("load")
        .into_iter()
        .find(|record| record.directory_id == directory_id)
        .expect("record")
}

pub(crate) fn acquire(
    repo: &TempRepo,
    cache: &TempDir,
    issue: u64,
    session: &str,
    isolate: bool,
) -> Result<crate::worktree::AcquireOutcome, crate::worktree::WorktreeError> {
    crate::worktree::acquire_directory_with(
        &ProcessWorktreeRunner::new(),
        repo.dir.path(),
        issue,
        session,
        crate::worktree::AcquireOptions {
            cache_base: Some(cache.path()),
            isolate,
            base: None,
            reuse: false,
        },
    )
}
