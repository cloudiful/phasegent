//! Repository scoping for one reconciliation pass.
//!
//! Without `--all` the pass scans exactly the checkout it runs in. With
//! `--all` it walks every repository identity the directory registry records
//! and resolves each one through its main checkout, skipping identities whose
//! checkout can no longer be verified with a reason instead of guessing.

use std::path::{Path, PathBuf};

use crate::infra::storage::Storage;
use crate::worktree::registry::{self, DirectoryRecord};
use crate::worktree::{WorktreeError, WorktreeRunner};

use super::SyncSkippedRepo;

pub(crate) struct RepoScope {
    pub(crate) repo_identity: String,
    pub(crate) repo_path: PathBuf,
    pub(crate) records: Vec<DirectoryRecord>,
}

/// Resolve the repository scopes of one pass.
pub(super) fn resolve_scopes(
    runner: &dyn WorktreeRunner,
    storage: &Storage,
    all: bool,
    cwd: &Path,
) -> Result<(Vec<RepoScope>, Vec<SyncSkippedRepo>), WorktreeError> {
    if !all {
        let identity = crate::worktree::repo_identity(runner, cwd)?;
        let records = registry::load_records(storage, Some(&identity))?;
        return Ok((
            vec![RepoScope {
                repo_identity: identity,
                repo_path: cwd.to_path_buf(),
                records,
            }],
            Vec::new(),
        ));
    }
    let mut scopes = Vec::new();
    let mut skipped = Vec::new();
    for identity in registry::repo_identities(storage)? {
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
        let records = match registry::load_records(storage, Some(&identity)) {
            Ok(records) => records,
            Err(error) => {
                skipped.push(SyncSkippedRepo {
                    repo_identity: identity,
                    reason: format!("could not read the directory registry: {}", error.message),
                });
                continue;
            }
        };
        scopes.push(RepoScope {
            repo_identity: identity,
            repo_path,
            records,
        });
    }
    Ok((scopes, skipped))
}

pub(super) trait IntoProviderError {
    fn into_provider_error(self) -> crate::providers::api::PhasegentError;
}

impl IntoProviderError for WorktreeError {
    fn into_provider_error(self) -> crate::providers::api::PhasegentError {
        crate::providers::api::PhasegentError::request("issue sync", self.message)
    }
}
