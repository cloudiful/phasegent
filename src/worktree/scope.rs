//! The exact ownership identity of one worktree association.
//!
//! A scope is either a legacy `(provider, project, external_id)` triple or a
//! durable task scope that also carries the task id and the canonical instance
//! the task lives on. The task fields keep same-number tasks on different
//! instances apart. This is identity evidence, not occupancy state.

use std::path::Path;

use crate::worktree::WorktreeError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeScope {
    provider: String,
    project: String,
    external_id: u64,
    task_id: Option<i64>,
    instance: Option<String>,
}

impl WorktreeScope {
    pub fn new(provider: &str, project: &str, external_id: u64) -> Result<Self, WorktreeError> {
        Self::validated(provider, project, external_id)
    }

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
                "worktree scope task id must be greater than zero",
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
                "worktree scope task instance must be a non-empty endpoint",
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
                "worktree scope requires provider, project and a positive external id",
            ));
        }
        if !provider
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(WorktreeError::new(
                "argument",
                "worktree scope provider must be canonical",
            ));
        }
        if provider == "forgejo"
            && crate::providers::forgejo::ForgejoConfig::new("http://localhost", project).is_err()
        {
            return Err(WorktreeError::new(
                "argument",
                "Forgejo worktree project must be canonical owner/repo",
            ));
        }
        if provider == "gitlab"
            && crate::providers::gitlab::GitlabConfig::new("http://localhost", project).is_err()
        {
            return Err(WorktreeError::new(
                "argument",
                "GitLab worktree project must be a canonical namespace/project path",
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
    pub fn task_id(&self) -> Option<i64> {
        self.task_id
    }
    pub fn instance(&self) -> Option<&str> {
        self.instance.as_deref()
    }
}

/// The durable repository key of a checkout, proven from git. The key names
/// the physical repository the durable task memberships are recorded under.
pub(crate) fn verified_repo_key(path: &Path) -> Option<String> {
    crate::branch_links::resolve_checkout_repo_key(
        &crate::git_runner::ProcessGitRunner::in_directory(path),
    )
    .ok()
}

/// The durable repository key of the physical repository a common-dir
/// identity names. The identity is the repository's git common directory, so
/// its parent is the checkout that owns it.
pub(crate) fn verified_repo_key_for_identity(identity: &str) -> Option<String> {
    let checkout = Path::new(identity).parent()?;
    verified_repo_key(checkout)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn checkout(label: &str) -> PathBuf {
        let root = crate::test_scratch::root().join(format!(
            "phasegent-worktree-scope-{label}-{}-{}",
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
        let missing = PathBuf::from("/nonexistent/phasegent-worktree-scope-checkout");
        assert!(verified_repo_key(&missing).is_none());
        assert!(
            verified_repo_key_for_identity(missing.join(".git").to_str().unwrap()).is_none(),
            "a missing checkout yields no key, so every keyed reader fails closed"
        );
    }
}
