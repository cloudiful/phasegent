//! Shared fixture for the OpenCode-aware cleanup regression suites
//! (issue 747 P1).
//!
//! Each fixture builds a real repository with a real linked worktree and
//! a real lease row, so the production entry point runs against real Git
//! state. The only double is the host: `FakeOpenCodeApi` reports where
//! sessions are, so no test reaches a real OpenCode instance and no test
//! can move a real session.

use super::support::*;
use super::*;

use crate::lifecycle::{AutoCleanupOutcome, CleanupMode, cleanup_closed_issue_worktrees_with};
use crate::worktree::leases::{NewLease, ensure_schema, insert_lease};
use crate::worktree::opencode::test_support::FakeOpenCodeApi;

pub(crate) const ISSUE: u64 = 7;

pub(crate) struct Fixture {
    /// The repository's main checkout; the only verified move target.
    pub(crate) main: PathBuf,
    /// A real linked worktree standing in for the closed issue's
    /// candidate directory.
    pub(crate) candidate: PathBuf,
    pub(crate) identity: String,
    _repo: TempRepo,
    _db: TempDir,
    _env: EnvGuard,
}

impl Fixture {
    /// Build the repository, the linked candidate worktree, and the
    /// temporary lease store. Returns `None` when the host cannot create
    /// a Git repository or a linked worktree, which is the project's
    /// established way of skipping rather than failing such a test.
    /// Callers must already hold `lock_workflow_tests()`.
    pub(crate) fn new(label: &str) -> Option<Self> {
        let repo = TempRepo::init(label)?;
        let main = repo.dir.path().to_path_buf();
        let candidate = crate::test_scratch::root().join(format!(
            "phasegent-p1-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let runner = ProcessWorktreeRunner::new();
        worktree_add(&runner, &main, &candidate, "feat/7-aaaaaa").ok()?;
        let identity = repo_identity(&runner, &main).expect("repo identity");
        let (db, storage, env) = open_temp_db(label);
        ensure_schema(&storage).expect("lease schema");
        Some(Self {
            main,
            candidate,
            identity,
            _repo: repo,
            _db: db,
            _env: env,
        })
    }

    /// Record a lease for the candidate, already converged to `retained`
    /// exactly as the close chain leaves it before the cleanup runs.
    pub(crate) fn lease(&self, session: &str) {
        self.lease_at(&self.candidate, session);
    }

    /// Record a lease pointing at an arbitrary directory, for the guards
    /// that are about the path rather than about the session.
    pub(crate) fn lease_at(&self, worktree_path: &Path, session: &str) {
        let storage = Storage::open().expect("temp storage opens");
        let lease_id = format!("lease-{}", worktree_path.display());
        insert_lease(
            &storage,
            NewLease {
                lease_id: lease_id.as_str(),
                identity: &self.identity,
                issue: ISSUE,
                session,
                checkout_path: &self.main.to_string_lossy(),
                worktree_path: &worktree_path.to_string_lossy(),
                branch: "feat/7-aaaaaa",
                status: LEASE_STATUS_RETAINED,
                created_at: 1,
                heartbeat_at: 1,
            },
        )
        .expect("lease row inserts");
    }

    /// Drive the production cleanup for this fixture's issue.
    pub(crate) fn clean(
        &self,
        api: &FakeOpenCodeApi,
        mode: CleanupMode,
        session: Option<&str>,
    ) -> AutoCleanupOutcome {
        cleanup_closed_issue_worktrees_with(
            &ProcessWorktreeRunner::new(),
            api,
            mode,
            &self.main,
            ISSUE,
            session,
        )
    }
}

/// Unwrap a classified pass: the cleanup only returns `Noop` or
/// `Warning` when it never reached a candidate, which no fixture here
/// arranges.
pub(crate) fn classified(outcome: AutoCleanupOutcome) -> (u64, Vec<String>) {
    match outcome {
        AutoCleanupOutcome::Cleaned { removed, kept } => (removed, kept),
        other => panic!("expected a classified pass, got {other:?}"),
    }
}
