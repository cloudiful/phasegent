use std::collections::HashMap;
use std::path::Path;

use crate::infra::storage::Storage;
use crate::worktree::WorktreeError;

/// The exact identity one scoped lease belongs to.
///
/// A scope is either the legacy `(provider, project, external_id)` triple or
/// a task scope that also carries the durable task id and the canonical
/// instance that task lives on. The task fields are what keep two
/// same-number tasks on different instances apart: they never share a
/// scope, so one task's close, release or reuse can never select the other
/// task's lease.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseScope {
    provider: String,
    project: String,
    external_id: u64,
    task_id: Option<i64>,
    instance: Option<String>,
}

impl LeaseScope {
    /// The legacy scope: a provider, a project and a positive external id.
    /// It carries no task identity, exactly as before.
    pub fn new(provider: &str, project: &str, external_id: u64) -> Result<Self, WorktreeError> {
        Self::validated(provider, project, external_id)
    }

    /// The task scope: the legacy triple plus the durable task id and the
    /// canonical instance the task is bound to.
    ///
    /// Both task fields are required, so a scope can never claim a task
    /// identity it cannot reproduce, and a scope that names a task never
    /// equals a scope that does not.
    pub fn for_task(
        provider: &str,
        project: &str,
        external_id: u64,
        task_id: i64,
        instance: &str,
    ) -> Result<Self, WorktreeError> {
        let mut scope = Self::validated(provider, project, external_id)?;
        if task_id <= 0 {
            return Err(WorktreeError::new(
                "argument",
                "lease scope task id must be greater than zero",
            ));
        }
        let instance = instance.trim();
        if instance.is_empty()
            || instance
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(WorktreeError::new(
                "argument",
                "lease scope task instance must be a non-empty endpoint",
            ));
        }
        scope.task_id = Some(task_id);
        scope.instance = Some(instance.to_owned());
        Ok(scope)
    }

    fn validated(provider: &str, project: &str, external_id: u64) -> Result<Self, WorktreeError> {
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
            task_id: None,
            instance: None,
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
    /// The durable task this scope belongs to, if any.
    pub fn task_id(&self) -> Option<i64> {
        self.task_id
    }
    /// The canonical instance of the durable task this scope belongs to.
    pub fn instance(&self) -> Option<&str> {
        self.instance.as_deref()
    }
    /// Whether the scope names a durable task identity.
    pub fn is_task_scoped(&self) -> bool {
        self.task_id.is_some()
    }
}

/// The no-key sidecar map of one repository.
///
/// It keeps the legacy NULL behaviour — a sidecar without a task id is read
/// exactly as before — and refuses every task-bearing row, because a task
/// can only be shown to belong to a physical repository through that
/// repository's own verified key. Production readers use the keyed variant;
/// this wrapper stays for the legacy fixtures.
#[cfg(test)]
pub(crate) fn scope_map_for_repo(
    storage: &Storage,
    identity: &str,
) -> Result<HashMap<String, LeaseScope>, WorktreeError> {
    scope_map_for_connection(&storage.connection, identity)
}

/// The durable repository key of a checkout, proven from git.
///
/// The key is derived from the checkout's own verified root and origin, so
/// it names the physical repository the durable task memberships are
/// recorded under. `None` means the key could not be proven: a checkout
/// without a verifiable root or origin has no key, and every keyed reader
/// then fails closed on a task-bearing row instead of assuming membership.
///
/// The git call happens outside every SQLite transaction, so a proof never
/// mixes filesystem work with a write boundary.
pub(crate) fn verified_repo_key(path: &Path) -> Option<String> {
    crate::branch_links::resolve_checkout_repo_key(
        &crate::git_runner::ProcessGitRunner::in_directory(path),
    )
    .ok()
}

/// The durable repository key of the physical repository a lease identity
/// names.
///
/// The identity is the repository's git common directory, so its parent is
/// the checkout that owns it. Proving the key from that checkout keeps every
/// reader of the same repository on one key, no matter which worktree the
/// caller happens to sit in. `None` means the checkout could not be proven —
/// a missing or foreign path yields no key at all, so a task-bearing row
/// fails closed instead of being assumed to belong here.
pub(crate) fn verified_repo_key_for_identity(identity: &str) -> Option<String> {
    let checkout = Path::new(identity).parent()?;
    verified_repo_key(checkout)
}

/// The sidecar map of one repository, checked against a verified physical
/// repository key.
///
/// A task-bearing sidecar is only accepted when the task exists, is
/// canonical, matches the sidecar's own provider, project and external
/// number, and is an explicit member of this physical repository. A task
/// whose membership lives in another repository is refused here, so no
/// caller can release or retain it through this repository's proof.
///
/// `repo_key` is the verified durable key of the physical repository the
/// caller is acting on. `None` means the key could not be proven, so a
/// task-bearing row cannot be shown to belong to this repository and the
/// read fails closed instead of assuming the membership.
pub(crate) fn scope_map_for_repo_with_key(
    storage: &Storage,
    identity: &str,
    repo_key: Option<&str>,
) -> Result<HashMap<String, LeaseScope>, WorktreeError> {
    scope_map_for_connection_with_key(&storage.connection, identity, repo_key)
}

/// The same complete sidecar map over an arbitrary connection, so a caller
/// can rebuild and compare the proof inside its own immediate transaction.
///
/// A sidecar that names a durable task is only accepted when that task
/// exists, is canonical, and matches the sidecar's own provider, project and
/// external number. An orphan, mismatched or corrupt reference fails the
/// whole read, so no caller can act on unproven ownership evidence. A
/// sidecar without a task id stays the legacy scope it always was.
#[cfg(test)]
pub(crate) fn scope_map_for_connection(
    connection: &rusqlite::Connection,
    identity: &str,
) -> Result<HashMap<String, LeaseScope>, WorktreeError> {
    scope_map_for_connection_with_key(connection, identity, None)
}

/// The sidecar map over an arbitrary connection, checked against a verified
/// physical repository key.
///
/// The key is the durable repository identity the memberships are recorded
/// under. Without it a task-bearing row cannot be proven at all, so the read
/// fails closed instead of accepting the row.
pub(crate) fn scope_map_for_connection_with_key(
    connection: &rusqlite::Connection,
    identity: &str,
    repo_key: Option<&str>,
) -> Result<HashMap<String, LeaseScope>, WorktreeError> {
    let task_id_column = scope_has_task_id(connection)?;
    let sql = if task_id_column {
        "SELECT s.lease_id, s.provider, s.project, s.external_id, l.issue, s.task_id \
           FROM worktree_lease_scopes s JOIN worktree_leases l ON l.lease_id = s.lease_id \
          WHERE l.repo_identity = ?1"
    } else {
        "SELECT s.lease_id, s.provider, s.project, s.external_id, l.issue, NULL \
           FROM worktree_lease_scopes s JOIN worktree_leases l ON l.lease_id = s.lease_id \
          WHERE l.repo_identity = ?1"
    };
    let mut statement = connection
        .prepare(sql)
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
        let task_id: Option<i64> = row
            .get(5)
            .map_err(|e| WorktreeError::new("storage", format!("lease scope task id: {e}")))?;
        if issue != external {
            return Err(WorktreeError::new(
                "storage",
                "lease scope does not match issue number",
            ));
        }
        let external_id = u64::try_from(external).unwrap_or(0);
        let scope = match task_id {
            Some(task_id) => {
                // A task-bearing scope is only proven with a verified
                // physical repository key: the membership is what ties the
                // task to this repository.
                let repo_key = repo_key.ok_or_else(|| {
                    WorktreeError::new(
                        "storage",
                        "lease scope references a durable task but the repository key is unknown",
                    )
                })?;
                let binding = crate::task_identity::binding_for_task_id_on(connection, task_id)
                    .map_err(|e| WorktreeError::new("storage", e))?
                    .ok_or_else(|| {
                        WorktreeError::new(
                            "storage",
                            "lease scope references an unknown durable task",
                        )
                    })?;
                let key = binding.key();
                if key.provider() != provider
                    || key.project() != project
                    || key.external_number() != Some(external_id)
                {
                    return Err(WorktreeError::new(
                        "storage",
                        "lease scope does not match its durable task",
                    ));
                }
                if !crate::task_identity::task_repo_membership_exists_on(
                    connection, repo_key, task_id,
                )
                .map_err(|e| WorktreeError::new("storage", e))?
                {
                    return Err(WorktreeError::new(
                        "storage",
                        "lease scope references a durable task that is not a member of this repository",
                    ));
                }
                LeaseScope::for_task(&provider, &project, external_id, task_id, key.instance())?
            }
            None => LeaseScope::new(&provider, &project, external_id)?,
        };
        result.insert(id, scope);
    }
    Ok(result)
}

/// Whether the lease sidecar table carries the durable task id column. An
/// older store simply has no task-bearing rows, so it stays legacy.
pub(crate) fn scope_has_task_id(connection: &rusqlite::Connection) -> Result<bool, WorktreeError> {
    let mut statement = connection
        .prepare(
            "SELECT name FROM pragma_table_info('worktree_lease_scopes') WHERE name = 'task_id'",
        )
        .map_err(|e| WorktreeError::new("storage", format!("read lease scope columns: {e}")))?;
    let mut rows = statement
        .query([])
        .map_err(|e| WorktreeError::new("storage", format!("read lease scope columns: {e}")))?;
    Ok(rows
        .next()
        .map_err(|e| WorktreeError::new("storage", format!("read lease scope columns: {e}")))?
        .is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A throwaway git checkout with one origin, so the durable key can be
    /// proven from git exactly like a real repository.
    fn checkout(label: &str) -> PathBuf {
        let root = crate::test_scratch::root().join(format!(
            "phasegent-lease-scope-{label}-{}-{}",
            std::process::id(),
            crate::worktree::now_unix_secs()
        ));
        std::fs::create_dir_all(&root).unwrap();
        for args in [
            ["init", "--quiet"].as_slice(),
            [
                "remote",
                "add",
                "origin",
                "https://forgejo.test/owner/repo.git",
            ]
            .as_slice(),
        ] {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} must succeed");
        }
        root
    }

    #[test]
    fn verified_repo_key_follows_the_origin_of_the_named_repository() {
        let root = checkout("origin");
        let identity = root.join(".git");
        assert_eq!(
            verified_repo_key_for_identity(identity.to_str().unwrap()).as_deref(),
            Some("forgejo.test/owner/repo")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn verified_repo_key_is_absent_for_an_unprovable_checkout() {
        let missing = PathBuf::from("/nonexistent/phasegent-lease-scope-checkout");
        assert!(verified_repo_key(&missing).is_none());
        assert!(
            verified_repo_key_for_identity(missing.join(".git").to_str().unwrap()).is_none(),
            "a missing checkout yields no key, so every keyed reader fails closed"
        );
    }
}
