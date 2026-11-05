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
use crate::worktree::leases::{ensure_schema, list_all_for_repo, repo_identities};
use crate::worktree::{LeaseRow, WorktreeError, WorktreeRunner};

use super::SyncSkippedRepo;

pub(crate) struct RepoScope {
    pub(crate) repo_identity: String,
    pub(crate) repo_path: PathBuf,
    pub(crate) candidates: Vec<(u64, Vec<LeaseRow>)>,
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
        let rows = list_all_for_repo(storage, &identity)?;
        let (candidates, skipped) = scoped_candidates(storage, &identity, rows)?;
        return Ok((
            vec![RepoScope {
                repo_identity: identity,
                repo_path: cwd.to_path_buf(),
                candidates,
            }],
            skipped,
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
        let rows = list_all_for_repo(storage, &identity)?;
        let (candidates, mut scope_skipped) = scoped_candidates(storage, &identity, rows)?;
        skipped.append(&mut scope_skipped);
        scopes.push(RepoScope {
            repo_identity: identity,
            repo_path,
            candidates,
        });
    }
    Ok((scopes, skipped))
}

/// Candidates of one repository plus the bounded skip reasons for the
/// scoped groups the pass conservatively keeps.
type ScopedCandidates = (Vec<(u64, Vec<LeaseRow>)>, Vec<SyncSkippedRepo>);

fn scoped_candidates(
    storage: &Storage,
    identity: &str,
    rows: Vec<LeaseRow>,
) -> Result<ScopedCandidates, WorktreeError> {
    let _ = storage;
    let _ = identity;
    // Scoped groups stay in the candidate list and are classified against
    // the pass's provider context in the engine, where a matching Forgejo
    // group can be reconciled; the legacy path never dispatches them.
    Ok((super::engine::candidate_issues(rows), Vec::new()))
}

/// Open the lease store and make sure the schema exists, so a pass on a
/// pre-Phase-1 database behaves like every other worktree entry point.
pub(super) fn open_lease_storage() -> Result<Storage, String> {
    let storage = Storage::open()?;
    ensure_schema(&storage)?;
    Ok(storage)
}

/// Small adapter so a [`WorktreeError`] can travel through the pass's
/// single error channel.
pub(super) trait IntoProviderError {
    fn into_provider_error(self) -> crate::providers::api::PhasegentError;
}

impl IntoProviderError for WorktreeError {
    fn into_provider_error(self) -> crate::providers::api::PhasegentError {
        crate::providers::api::PhasegentError::request("issue sync", self.message)
    }
}
