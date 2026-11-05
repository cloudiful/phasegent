use std::collections::HashMap;

use crate::infra::storage::Storage;
use crate::worktree::WorktreeError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseScope {
    provider: String,
    project: String,
    external_id: u64,
}

impl LeaseScope {
    pub fn new(provider: &str, project: &str, external_id: u64) -> Result<Self, WorktreeError> {
        if provider.is_empty()
            || project.is_empty()
            || external_id == 0
            || external_id > i64::MAX as u64
            || provider
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
            || project.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(WorktreeError::new(
                "argument",
                "lease scope requires provider, project and a positive external id",
            ));
        }
        if !provider
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(WorktreeError::new(
                "argument",
                "lease scope provider must be canonical",
            ));
        }
        if provider == "forgejo"
            && crate::providers::forgejo::ForgejoConfig::new("http://localhost", project).is_err()
        {
            return Err(WorktreeError::new(
                "argument",
                "Forgejo lease project must be canonical owner/repo",
            ));
        }
        Ok(Self {
            provider: provider.to_owned(),
            project: project.to_owned(),
            external_id,
        })
    }
    pub fn provider(&self) -> &str {
        &self.provider
    }
    pub fn project(&self) -> &str {
        &self.project
    }
    pub fn external_id(&self) -> u64 {
        self.external_id
    }
}

pub(crate) fn scope_map_for_repo(
    storage: &Storage,
    identity: &str,
) -> Result<HashMap<String, LeaseScope>, WorktreeError> {
    scope_map_for_connection(&storage.connection, identity)
}

/// The same complete sidecar map over an arbitrary connection, so a caller
/// can rebuild and compare the proof inside its own immediate transaction.
pub(crate) fn scope_map_for_connection(
    connection: &rusqlite::Connection,
    identity: &str,
) -> Result<HashMap<String, LeaseScope>, WorktreeError> {
    let mut statement = connection.prepare("SELECT s.lease_id, s.provider, s.project, s.external_id, l.issue FROM worktree_lease_scopes s JOIN worktree_leases l ON l.lease_id = s.lease_id WHERE l.repo_identity = ?1")
        .map_err(|e| WorktreeError::new("storage", format!("prepare lease scopes: {e}")))?;
    let mut rows = statement
        .query([identity])
        .map_err(|e| WorktreeError::new("storage", format!("read lease scopes: {e}")))?;
    let mut result = HashMap::new();
    while let Some(row) = rows
        .next()
        .map_err(|e| WorktreeError::new("storage", format!("read lease scope: {e}")))?
    {
        let id: String = row
            .get(0)
            .map_err(|e| WorktreeError::new("storage", format!("lease scope id: {e}")))?;
        let provider: String = row
            .get(1)
            .map_err(|e| WorktreeError::new("storage", format!("lease scope provider: {e}")))?;
        let project: String = row
            .get(2)
            .map_err(|e| WorktreeError::new("storage", format!("lease scope project: {e}")))?;
        let external: i64 = row
            .get(3)
            .map_err(|e| WorktreeError::new("storage", format!("lease scope external id: {e}")))?;
        let issue: i64 = row
            .get(4)
            .map_err(|e| WorktreeError::new("storage", format!("lease scope issue: {e}")))?;
        if issue != external {
            return Err(WorktreeError::new(
                "storage",
                "lease scope does not match issue number",
            ));
        }
        let scope = LeaseScope::new(&provider, &project, u64::try_from(external).unwrap_or(0))?;
        result.insert(id, scope);
    }
    Ok(result)
}
