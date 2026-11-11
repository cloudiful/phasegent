//! Shared fixture for the OpenCode-aware cleanup regression suites.
//!
//! Each fixture builds a real repository with a real linked worktree and a real
//! registry directory, so the production entry point runs against real Git
//! state. The only double is the host: `FakeOpenCodeApi` reports where sessions
//! are, so no test reaches a real OpenCode instance and no test can move a real
//! session.

use super::support::*;
use super::*;

use crate::lifecycle::cleanup::closure::{DenyOtherIssues, DirectoryIssueClosure};
use crate::lifecycle::{AutoCleanupOutcome, CleanupMode, cleanup_closed_issue_worktrees_with};
use crate::worktree::opencode::OpenCodeApi;
use crate::worktree::opencode::test_support::FakeOpenCodeApi;
use crate::worktree::registry::{self, NewAssociation, NewDirectory};

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
    /// temporary registry store. Returns `None` when the host cannot create
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
        registry::ensure_schema(&storage).expect("registry schema");
        Some(Self {
            main,
            candidate,
            identity,
            _repo: repo,
            _db: db,
            _env: env,
        })
    }

    /// Record the candidate directory and its session.
    pub(crate) fn associate(&self, session: &str) {
        self.associate_issue(ISSUE, session);
    }

    /// Record a directory association for another issue pointing at the
    /// candidate, so the closure guards can be exercised against a shared
    /// directory.
    pub(crate) fn associate_issue(&self, issue: u64, session: &str) {
        let storage = Storage::open().expect("temp storage opens");
        let record = registry::record_for_path(&storage, &self.identity, &self.candidate)
            .expect("read registry");
        let association = NewAssociation::Unscoped { issue };
        match record {
            Some(record) => registry::attach_association(
                &storage,
                &record.directory_id,
                &self.identity,
                1,
                session,
                &association,
            )
            .expect("attach association"),
            None => {
                let canonical = crate::worktree::canonical_or_self(&self.candidate);
                let directory_id =
                    registry::stable_directory_id(&self.identity, &canonical.to_string_lossy());
                registry::insert_directory(
                    &storage,
                    &NewDirectory {
                        directory_id: &directory_id,
                        repo_identity: &self.identity,
                        checkout_path: &self.main.to_string_lossy(),
                        worktree_path: &canonical.to_string_lossy(),
                        branch: "feat/7-aaaaaa",
                        created_at: 1,
                        session,
                        associations: &[association],
                    },
                )
                .expect("insert directory");
            }
        }
    }

    /// Drive the production cleanup for this fixture's issue.
    pub(crate) fn clean(
        &self,
        api: &FakeOpenCodeApi,
        mode: CleanupMode,
        session: Option<&str>,
    ) -> AutoCleanupOutcome {
        self.clean_with(api, mode, session, &DenyOtherIssues)
    }

    /// Drive the production cleanup with an explicit closure seam over an
    /// explicit host.
    pub(crate) fn clean_with_api(
        &self,
        api: &dyn OpenCodeApi,
        mode: CleanupMode,
        session: Option<&str>,
        closure: &dyn DirectoryIssueClosure,
    ) -> AutoCleanupOutcome {
        cleanup_closed_issue_worktrees_with(
            &ProcessWorktreeRunner::new(),
            api,
            mode,
            &self.main,
            ISSUE,
            session,
            closure,
        )
    }

    /// Drive the production cleanup with an explicit closure seam.
    pub(crate) fn clean_with(
        &self,
        api: &FakeOpenCodeApi,
        mode: CleanupMode,
        session: Option<&str>,
        closure: &dyn DirectoryIssueClosure,
    ) -> AutoCleanupOutcome {
        self.clean_with_api(api, mode, session, closure)
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
