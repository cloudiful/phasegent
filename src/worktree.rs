//! Local Git worktree leasing for seamless, repo-isolated execution.
//!
//! Phase 1 of issue #239 (worktree-seamless) provides a Rust core that
//! hands out an isolated checkout per `(repo, issue, session)` triple
//! without exposing any new CLI surface, policy change, or plugin JS.
//! The module owns three responsibilities:
//!
//! * **Repo identity**: resolve the canonical `git-common-dir` of a
//!   checkout so the same physical repository is recognised whether the
//!   caller passed the main worktree, a linked worktree, or a path
//!   inside a worktree whose `.git` is a file pointer.
//! * **Lease table**: a small `worktree_leases` row that records which
//!   checkout/dir/branch belongs to a session, with `status` in
//!   `{active, retained, released}`. Schema is created lazily via
//!   `CREATE TABLE IF NOT EXISTS` so opening storage is still
//!   non-destructive against pre-Phase-1 databases.
//! * **Git wrapper helpers**: thin `git worktree list/add/remove`
//!   wrappers in `branch_context`-style so the same
//!   `ProcessWorktreeRunner` / `FakeWorktreeRunner` testability
//!   pattern covers worktree flows. `add` and `remove` are exercised
//!   only by tests in Phase 1; the production call site is Phase 2's
//!   CLI.
//!
//! ## Phase boundaries
//!
//! * `acquire_lease` may create a fresh worktree under
//!   `~/.cache/phasegent/worktrees/<fingerprint>/<slug>` (or reuse the
//!   current checkout when no other lease is active for the repo).
//!   Creation is gated by issue #247: it only happens when `--isolate`
//!   or the resolved `worktree-auto` switch is on, so the default path
//!   reuses the current checkout and warns on a conflict. It never
//!   deletes a worktree or branch.
//! * `release_lease` flips the row to `retained` (default) or
//!   `released`. Directory and branch pruning is a Phase 2 concern and
//!   is intentionally not implemented here.
//! * `is_clean` is the worktree-dirty probe Phase 2 will gate prune on.
//!   Phase 1 ships the helper and a test so the contract is locked
//!   before prune depends on it.
//!
//! ## .env / secrets
//!
//! The worktree module never reads, copies, or writes `.env` files,
//! shell history, or credential material; per the issue Decisions, the
//! AI / user is expected to copy `.env` by hand when needed. This is
//! documented in [`acquire_lease`] so a future operator searching for
//! ".env support" lands on the right place.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::infra::storage::Storage;

mod acquire;
pub(crate) mod git;
pub(crate) mod leases;
mod naming;

// The re-exports below are part of the public surface of the
// `worktree` module and are consumed by the integration tests
// (and the future Phase 2 CLI). They look unused to clippy when
// the test target is not being analysed, so silence the false
// positive here.
#[allow(unused_imports)]
pub use acquire::{
    WORKTREE_AUTO_SETTING, acquire_lease, release_lease, release_lease_forced,
    resolve_worktree_auto,
};
#[allow(unused_imports)]
pub use git::{is_clean, parse_worktree_list, worktree_add, worktree_remove};
#[allow(unused_imports)]
pub use leases::{ensure_schema, list_for_issue, list_for_repo};
#[allow(unused_imports)]
pub use naming::{
    cache_root_in, compute_fingerprint, generate_branch, slug_from_branch, validate_ref_format,
};

/// Status of a single lease row. `active` rows are the ones
/// `acquire_lease` returns; `retained` / `released` are terminal
/// states used by `release_lease`.
pub const LEASE_STATUS_ACTIVE: &str = "active";
pub const LEASE_STATUS_RETAINED: &str = "retained";
pub const LEASE_STATUS_RELEASED: &str = "released";

/// Max length of any structured branch / slug string the module echoes
/// back to callers. Bounded so a misbehaving `git` invocation cannot
/// flood logs with arbitrary content.
const MAX_ECHO_CHARS: usize = 200;
/// Cap on `git` output echoed back into structured messages. Matches
/// the `MAX_ECHO_CHARS` bound used elsewhere in the codebase.
#[allow(dead_code)]
const ECHO_LIMIT: usize = 200;

#[derive(Debug, PartialEq, Eq)]
pub struct LeaseRow {
    pub lease_id: String,
    pub repo_identity: String,
    pub issue: u64,
    pub session: String,
    pub checkout_path: String,
    pub worktree_path: String,
    pub branch: String,
    pub status: String,
    pub created_at: i64,
    pub heartbeat_at: i64,
    /// Operator justification for a forced release; `None` for
    /// ordinary releases. Lease rows are never deleted.
    pub release_reason: Option<String>,
}

/// Outcome of a successful `acquire_lease`. `created == false` means
/// the caller is reusing the current checkout because no other lease
/// is active for the repo; `created == true` means a fresh worktree
/// was added.
#[derive(Debug, PartialEq, Eq)]
pub struct AcquireOutcome {
    pub lease_id: String,
    pub path: String,
    pub branch: String,
    pub repo_identity: String,
    pub created: bool,
    /// `no_conflict` when reusing the current checkout; `new_worktree`
    /// when a fresh dir / branch was created; `idempotent` when a
    /// pre-existing `(repo, issue, session)` lease was reused.
    pub reason: String,
    /// Best-effort, stderr-bound warnings collected while deciding the
    /// outcome (e.g. a dirty checkout reused because it is not bound
    /// to a task, or the trigger detail behind a `new_worktree`
    /// decision). Never changes `created` / `reason`; the CLI JSON
    /// envelope deliberately does not carry this field, so consumers
    /// that key on `created` need no changes.
    pub warnings: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ReleaseOutcome {
    pub lease_id: String,
    pub status: String,
    /// True when the transition went through the forced path with a
    /// recorded reason. A no-op on an already-terminal lease reports
    /// `forced: false` because nothing was overridden.
    pub forced: bool,
    pub reason: Option<String>,
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

    #[allow(dead_code)]
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({ "kind": self.kind, "message": self.message })
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

/// Abstraction over Git invocation so acquire / is_clean can be
/// tested without spawning processes. Takes the working directory per
/// call because the worktree flow runs `git` in the user's repo, in
/// the new worktree, and in `cache_root` directories — none of which
/// share a single workdir.
pub trait WorktreeRunner {
    fn run(&self, args: &[&str], workdir: &Path) -> Result<GitOutput, WorktreeError>;
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

impl WorktreeRunner for ProcessWorktreeRunner {
    fn run(&self, args: &[&str], workdir: &Path) -> Result<GitOutput, WorktreeError> {
        let mut command = Command::new("git");
        command.args(args).current_dir(workdir);
        let output = command
            .output()
            .map_err(|error| WorktreeError::new("git", format!("could not run git: {error}")))?;
        let status = output.status.code().unwrap_or(-1);
        Ok(GitOutput {
            status,
            stdout: sanitized_git_output(&output.stdout),
        })
    }
}

pub(crate) fn bounded(text: &str) -> String {
    text.chars().take(MAX_ECHO_CHARS).collect()
}

#[allow(dead_code)]
fn sanitized_git_output(raw: &[u8]) -> String {
    let lossy = String::from_utf8_lossy(raw);
    let cleaned: String = lossy.chars().filter(|c| !c.is_control()).collect();
    cleaned.trim().chars().take(ECHO_LIMIT).collect()
}

/// Wall-clock seconds since UNIX_EPOCH, with a defensive `0` fallback
/// for the unlikely case where the system clock predates 1970. Used
/// for `created_at` / `heartbeat_at`.
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
    let output = runner.run(&["rev-parse", "--git-common-dir"], repo_path)?;
    if output.status != 0 {
        return Err(WorktreeError::new(
            "git",
            format!("git rev-parse failed with exit status {}", output.status),
        ));
    }
    let raw = output.stdout.trim();
    if raw.is_empty() {
        return Err(WorktreeError::new(
            "git",
            "git rev-parse returned an empty common dir",
        ));
    }
    // Common-dir may be a relative path (e.g. when run from the repo
    // root it prints `.`); resolve it against the working directory
    // we asked git about, then canonicalise so `.` and the absolute
    // version of the same dir produce the same identity.
    let resolved = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        repo_path.join(raw)
    };
    let canonical = resolved.canonicalize().unwrap_or(resolved);
    let identity = canonical.to_string_lossy().to_string();
    if identity.is_empty() {
        return Err(WorktreeError::new(
            "git",
            "could not resolve canonical repository identity",
        ));
    }
    Ok(bounded(&identity))
}

/// Read every active lease row for the given `issue`. Empty when
/// none; bounded by the storage layer.
#[allow(dead_code)]
pub fn leases_for_issue(issue: u64) -> Result<Vec<LeaseRow>, WorktreeError> {
    let storage = Storage::open().map_err(|error| WorktreeError::new("storage", error))?;
    ensure_schema(&storage).map_err(|error| WorktreeError::new("storage", error))?;
    list_for_issue(&storage, issue)
}

/// Read every lease row (active + terminal) for the given
/// `repo_identity`. Empty when none; bounded by the storage layer.
#[allow(dead_code)]
pub fn leases_for_repo(repo_identity: &str) -> Result<Vec<LeaseRow>, WorktreeError> {
    let storage = Storage::open().map_err(|error| WorktreeError::new("storage", error))?;
    ensure_schema(&storage).map_err(|error| WorktreeError::new("storage", error))?;
    list_for_repo(&storage, repo_identity)
}
