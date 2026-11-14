//! Local Git worktrees over a persistent directory registry.
//!
//! The module hands out an isolated checkout per `(repo, issue, session)`
//! and records the physical directory, its task or legacy issue
//! associations, and the sessions observed there. It keeps no occupancy
//! state of its own: live ownership comes from the connected service and
//! from whether an associated task is still open.
//!
//! * **Repo identity**: resolve the canonical `git-common-dir` of a
//!   checkout so the same physical repository is recognised whether the
//!   caller passed the main worktree, a linked worktree, or a path inside
//!   a worktree whose `.git` is a file pointer.
//! * **Directory registry**: `worktree_directories` rows and their task,
//!   legacy-issue, and session associations. A directory can reference
//!   many tasks and sessions; a confirmed closed historical task does not
//!   veto reuse when no fresh occupancy protects the path.
//! * **Git wrapper helpers**: thin `git worktree list/add/remove` wrappers
//!   so the same `ProcessWorktreeRunner` / test-runner pattern covers
//!   worktree flows.
//!
//! ## .env / secrets
//!
//! The worktree module never reads, copies, or writes `.env` files,
//! shell history, or credential material; per the issue Decisions, the
//! AI / user is expected to copy `.env` by hand when needed.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::infra::storage::Storage;

mod acquire;
pub(crate) mod cache;
mod create_hook;
pub(crate) mod git;
mod naming;
pub(crate) mod occupancy;
pub(crate) mod opencode;
pub(crate) mod operation_lock;
pub mod probe;
mod raw_output;
pub(crate) mod registry;
pub(super) mod registry_schema;
pub(crate) mod scope;
pub use scope::WorktreeScope;
mod session;

// The re-exports below are part of the public surface of the
// `worktree` module and are consumed by the integration tests
// (and the future Phase 2 CLI). They look unused to clippy when
// the test target is not being analysed, so silence the false
// positive here.
#[allow(unused_imports)]
pub use acquire::{AcquireOptions, acquire_directory, acquire_directory_with};
#[allow(unused_imports)]
pub(crate) use acquire::{
    acquire_directory_scoped, acquire_directory_seeded, acquire_directory_with_foreign_state,
};
#[allow(unused_imports)]
pub use git::{
    checkout_git_dirs, head_rev, is_clean, is_inside_work_tree, parse_worktree_list,
    ref_resolves_to_commit, worktree_add, worktree_add_from, worktree_remove,
};
#[allow(unused_imports)]
pub use naming::{
    cache_root, cache_root_in, compute_fingerprint, generate_branch, slug_from_branch,
    validate_ref_format,
};
#[allow(unused_imports)]
pub use probe::{MAX_PROBE_ERRORS, ProbeError, ProbeFacts, probe_path};
#[allow(unused_imports)]
pub use raw_output::RawOutputRunner;
#[allow(unused_imports)]
pub use registry::{DirectoryLegacyIssue, DirectoryRecord, DirectoryTask};
#[allow(unused_imports)]
pub use registry::{ensure_schema, record_for_path, records_for_issue, repo_identities};
#[allow(unused_imports)]
pub(crate) use session::{
    MAX_SESSION_CHARS, SESSION_ENV, SessionContext, SessionSource, resolve_session,
    resolve_session_optional, resolve_session_with,
};

/// Best-effort worktree acquisition after `issue create` (issue 18).
/// Exposed crate-internally so the create arm can run it.
#[allow(unused_imports)]
pub(crate) use create_hook::{auto_acquire_after_create, auto_acquire_after_create_scoped};

/// Max length of any structured branch / slug string the module echoes
/// back to callers. Bounded so a misbehaving `git` invocation cannot
/// flood logs with arbitrary content.
const MAX_ECHO_CHARS: usize = 200;

/// Outcome of a successful directory acquisition. `created == false`
/// means the caller is reusing the current checkout because no other
/// unfinished task or current session occupies its path; `created ==
/// true` means a fresh worktree was added.
#[derive(Debug, PartialEq, Eq)]
pub struct AcquireOutcome {
    pub directory_id: String,
    pub path: String,
    pub branch: String,
    pub repo_identity: String,
    pub created: bool,
    /// `no_conflict` when reusing the current checkout; `new_worktree`
    /// when a fresh dir / branch was created; `idempotent` when a
    /// pre-existing `(repo, issue, session)` directory was reused.
    pub reason: String,
    /// Best-effort, stderr-bound warnings collected while deciding the
    /// outcome (e.g. an advisory dirty checkout that was reused, or the
    /// trigger detail behind a `new_worktree` decision). Never changes
    /// `created` / `reason`; the CLI JSON envelope deliberately does not
    /// carry this field, so consumers that key on `created` need no changes.
    pub warnings: Vec<String>,
}

/// One parsed entry from `git worktree list --porcelain`. The
/// porcelain format emits blank-line-separated blocks with a leading
/// `worktree <path>`, optional `HEAD <sha>`, optional `branch <ref>`,
/// and a trailing blank line; the parser collects the fields the
/// module actually needs and discards the rest.
#[derive(Debug, PartialEq, Eq)]
pub struct WorktreeListEntry {
    pub worktree: String,
    pub head: Option<String>,
    pub branch: Option<String>,
}

/// Structured error type. The `kind` field is one of `argument`,
/// `git`, `storage`, `state`, `branch`. Messages are bounded and
/// never echo secrets, environment variables, or full git output.
#[derive(Debug, PartialEq, Eq)]
pub struct WorktreeError {
    pub kind: &'static str,
    pub message: String,
}

impl WorktreeError {
    pub(crate) fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: bounded(&message.into()),
        }
    }
}

impl fmt::Display for WorktreeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for WorktreeError {}

impl From<rusqlite::Error> for WorktreeError {
    fn from(error: rusqlite::Error) -> Self {
        Self::new("storage", format!("sqlite: {error}"))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct GitOutput {
    pub status: i32,
    pub stdout: String,
}

/// Ceiling on a machine path answer handed back by the production runner's
/// `run_path`. Well above any supported checkout path, and a hard stop so the
/// read cannot grow without bound.
const MAX_PATH_OUTPUT_BYTES: usize = 64 * 1024;

/// Abstraction over Git invocation so acquire / is_clean can be
/// tested without spawning processes. Takes the working directory per
/// call because the worktree flow runs `git` in the user's repo, in
/// the new worktree, and in `cache_root` directories — none of which
/// share a single workdir.
pub trait WorktreeRunner {
    fn run(&self, args: &[&str], workdir: &Path) -> Result<GitOutput, WorktreeError>;

    /// Run one Git command whose stdout is a machine path answer, preserving
    /// the exact bytes apart from the line ending. The default reuses [`run`]
    /// so existing test runners keep working; the production runner overrides
    /// it with a bounded byte-faithful channel. Callers must still reject an
    /// empty, multi-line, relative, or unresolvable answer.
    fn run_path(&self, args: &[&str], workdir: &Path) -> Result<GitOutput, WorktreeError> {
        self.run(args, workdir)
    }
}

/// Production implementation. Builds a fresh `git` process each call
/// so the per-call workdir is honoured. The helper never invokes a
/// shell and never concatenates user input into a command line.
#[allow(dead_code)]
pub struct ProcessWorktreeRunner;

impl ProcessWorktreeRunner {
    #[allow(dead_code)]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for ProcessWorktreeRunner {
    fn default() -> Self {
        Self::new()
    }
}

fn git_output(args: &[&str], workdir: &Path) -> Result<std::process::Output, WorktreeError> {
    let mut command = Command::new("git");
    command.args(args).current_dir(workdir);
    command
        .output()
        .map_err(|error| WorktreeError::new("git", format!("could not run git: {error}")))
}

impl WorktreeRunner for ProcessWorktreeRunner {
    fn run(&self, args: &[&str], workdir: &Path) -> Result<GitOutput, WorktreeError> {
        let output = git_output(args, workdir)?;
        let status = output.status.code().unwrap_or(-1);
        Ok(GitOutput {
            status,
            stdout: sanitized_git_output(&output.stdout),
        })
    }

    fn run_path(&self, args: &[&str], workdir: &Path) -> Result<GitOutput, WorktreeError> {
        let output = git_output(args, workdir)?;
        let status = output.status.code().unwrap_or(-1);
        if output.stdout.len() > MAX_PATH_OUTPUT_BYTES {
            return Err(WorktreeError::new(
                "git",
                "git path output exceeded its bound",
            ));
        }
        let stdout = String::from_utf8(output.stdout)
            .map_err(|_| WorktreeError::new("git", "git path output was not valid UTF-8"))?;
        Ok(GitOutput { status, stdout })
    }
}

/// Canonicalise `path`, falling back to the path itself when it no
/// longer exists so a just-removed directory still compares equal to its
/// recorded directory path.
pub(crate) fn canonical_or_self(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Canonical path answers for one read-only scan or one fresh recheck.
#[derive(Default)]
pub(crate) struct CanonicalPaths {
    paths: std::cell::RefCell<std::collections::HashMap<PathBuf, PathBuf>>,
}

impl CanonicalPaths {
    pub(crate) fn resolve(&self, path: &Path) -> PathBuf {
        if let Some(resolved) = self.paths.borrow().get(path) {
            return resolved.clone();
        }
        let resolved = canonical_or_self(path);
        self.paths
            .borrow_mut()
            .insert(path.to_path_buf(), resolved.clone());
        resolved
    }

    pub(crate) fn same_directory(&self, left: &Path, right: &Path) -> bool {
        self.resolve(left) == self.resolve(right)
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn resolutions(&self) -> usize {
        self.paths.borrow().len()
    }
}

/// True when both paths name the same directory. A registry row's recorded
/// path and a host-reported location can spell one directory differently,
/// so every such comparison in the crate goes through here.
pub(crate) fn same_directory(left: &Path, right: &Path) -> bool {
    canonical_or_self(left) == canonical_or_self(right)
}

pub(crate) fn bounded(text: &str) -> String {
    text.chars().take(MAX_ECHO_CHARS).collect()
}

#[allow(dead_code)]
fn sanitized_git_output(raw: &[u8]) -> String {
    let lossy = String::from_utf8_lossy(raw);
    let cleaned: String = lossy.chars().filter(|c| !c.is_control()).collect();
    cleaned.trim().chars().take(MAX_ECHO_CHARS).collect()
}

/// Wall-clock seconds since UNIX_EPOCH, with a defensive `0` fallback
/// for the unlikely case where the system clock predates 1970. Used for
/// `created_at`.
pub fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// Canonicalise the repository identity of `repo_path` by resolving
/// `git rev-parse --git-common-dir` and following it to an absolute
/// path. The same physical repository yields the same identity from
/// the main checkout, a linked worktree, or a subdirectory inside
/// either, because Git stores a single `.git`-common-dir pointer per
/// repository and writes a `gitdir:` file for linked worktrees.
pub fn repo_identity(
    runner: &dyn WorktreeRunner,
    repo_path: &Path,
) -> Result<String, WorktreeError> {
    let output = runner.run_path(&["rev-parse", "--git-common-dir"], repo_path)?;
    if output.status != 0 {
        return Err(WorktreeError::new(
            "git",
            format!("git rev-parse failed with exit status {}", output.status),
        ));
    }
    let raw = git::path_line(&output.stdout)?;
    // Common-dir may be a relative path (e.g. when run from the repo
    // root it prints `.`); resolve it against the working directory
    // we asked git about, then canonicalise so `.` and the absolute
    // version of the same dir produce the same identity.
    let resolved = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        repo_path.join(raw)
    };
    let canonical = resolved.canonicalize().map_err(|error| {
        WorktreeError::new(
            "git",
            format!("could not resolve canonical repository identity: {error}"),
        )
    })?;
    let identity = canonical.to_string_lossy().to_string();
    if identity.is_empty() {
        return Err(WorktreeError::new(
            "git",
            "could not resolve canonical repository identity",
        ));
    }
    Ok(identity)
}

#[allow(dead_code)]
pub fn directories_for_issue(issue: u64) -> Result<Vec<DirectoryRecord>, WorktreeError> {
    let storage = Storage::open().map_err(|error| WorktreeError::new("storage", error))?;
    registry::records_for_issue(&storage, None, issue)
}

#[allow(dead_code)]
pub fn directories_for_repo(repo_identity: &str) -> Result<Vec<DirectoryRecord>, WorktreeError> {
    let storage = Storage::open().map_err(|error| WorktreeError::new("storage", error))?;
    registry::load_records(&storage, Some(repo_identity))
}
