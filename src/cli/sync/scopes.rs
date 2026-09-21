//! Repository scoping for one reconciliation pass (issue 552 Phase 2).
//!
//! Without `--all` the pass scans exactly the checkout it runs in. With
//! `--all` it walks every repository identity the lease table records —
//! the local checkouts of the operator's mirrors — and resolves each one
//! through its main checkout (the identity's parent directory), skipping
//! identities whose checkout can no longer be verified with a reason
//! instead of guessing.

use std::path::{Path, PathBuf};

use crate::infra::storage::Storage;
use crate::worktree::leases::{ensure_schema, list_for_repo};
use crate::worktree::{LeaseRow, WorktreeError, WorktreeRunner};

use super::SyncSkippedRepo;

/// One repository the pass scans, with the lease rows it holds.
pub(super) struct RepoScope {
    pub(super) repo_identity: String,
    pub(super) repo_path: PathBuf,
    pub(super) rows: Vec<LeaseRow>,
}

/// Resolve the repository scopes of one pass. Without `--all` this is the
/// current checkout (an unresolvable checkout is a hard error for the
/// caller); with `--all` it is every identity recorded in the lease table,
/// resolved through its main checkout and skipped with a reason when that
/// checkout is gone.
pub(super) fn resolve_scopes(
    runner: &dyn WorktreeRunner,
    storage: &Storage,
    all: bool,
    cwd: &Path,
) -> Result<(Vec<RepoScope>, Vec<SyncSkippedRepo>), WorktreeError> {
    if !all {
        let identity = crate::worktree::repo_identity(runner, cwd)?;
        let rows = list_for_repo(storage, &identity)?;
        return Ok((
            vec![RepoScope {
                repo_identity: identity,
                repo_path: cwd.to_path_buf(),
                rows,
            }],
            Vec::new(),
        ));
    }
    let mut scopes = Vec::new();
    let mut skipped = Vec::new();
    for identity in repo_identities(storage)? {
        let Some(repo_path) = Path::new(&identity).parent().map(Path::to_path_buf) else {
            skipped.push(SyncSkippedRepo {
                repo_identity: identity,
                reason: "repository identity has no checkout directory".to_owned(),
            });
            continue;
        };
        if !repo_path.exists() {
            skipped.push(SyncSkippedRepo {
                repo_identity: identity,
                reason: format!("checkout directory is missing: {}", repo_path.display()),
            });
            continue;
        }
        match crate::worktree::repo_identity(runner, &repo_path) {
            Ok(resolved) if resolved == identity => {}
            Ok(resolved) => {
                skipped.push(SyncSkippedRepo {
                    repo_identity: identity,
                    reason: format!(
                        "checkout {} resolves to a different repository identity: {resolved}",
                        repo_path.display()
                    ),
                });
                continue;
            }
            Err(error) => {
                skipped.push(SyncSkippedRepo {
                    repo_identity: identity,
                    reason: format!(
                        "checkout {} cannot resolve its repository identity: {}",
                        repo_path.display(),
                        error.message
                    ),
                });
                continue;
            }
        }
        let rows = list_for_repo(storage, &identity)?;
        scopes.push(RepoScope {
            repo_identity: identity,
            repo_path,
            rows,
        });
    }
    Ok((scopes, skipped))
}

/// Every repository identity the lease table knows about. The query reads
/// the same table `list_for_repo` reads, scoped to the distinct identities
/// so `--all` never walks the operator's filesystem looking for checkouts.
fn repo_identities(storage: &Storage) -> Result<Vec<String>, WorktreeError> {
    let mut statement = storage
        .connection
        .prepare("SELECT DISTINCT repo_identity FROM worktree_leases ORDER BY repo_identity")
        .map_err(|error| WorktreeError::new("storage", format!("prepare repo scan: {error}")))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| WorktreeError::new("storage", format!("repo scan: {error}")))?;
    let mut identities = Vec::new();
    for row in rows {
        identities.push(
            row.map_err(|error| WorktreeError::new("storage", format!("repo row: {error}")))?,
        );
    }
    Ok(identities)
}

/// Open the lease store and make sure the schema exists, so a pass on a
/// pre-Phase-1 database behaves like every other worktree entry point.
pub(super) fn open_lease_storage() -> Result<Storage, String> {
    let storage = Storage::open()?;
    ensure_schema(&storage)?;
    Ok(storage)
}

/// The reconciliation pass runs for `worktree acquire|list|prune`, which
/// take no provider flag, so it resolves the configured default provider
/// exactly like any other provider-backed command without `--provider`.
pub(super) fn default_provider(
    role: crate::policy::Role,
) -> Result<crate::providers::ProviderDispatcher, crate::providers::api::ForgejoError> {
    let kind = crate::providers::config::resolve_kind(role, None)?;
    crate::cli::provider_for(role, Some(kind), None, None, None, None)
}

/// Small adapter so a [`WorktreeError`] can travel through the pass's
/// single error channel.
pub(super) trait IntoProviderError {
    fn into_provider_error(self) -> crate::providers::api::ForgejoError;
}

impl IntoProviderError for WorktreeError {
    fn into_provider_error(self) -> crate::providers::api::ForgejoError {
        crate::providers::api::ForgejoError::request("issue sync", self.message)
    }
}
