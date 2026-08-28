//! Namespace resolution and private repository creation.

use crate::providers::api::{ForgejoError, RepoSummary};
use crate::providers::gitlab::model::{ApiNamespace, ApiProject, NewProject};

use super::core::GitlabProvider;

/// Parsed result of [`GitlabProvider::resolve_namespace_target`].
/// Either a numeric namespace id (preferred for personal or explicit
/// group targets) or a string path is returned so the caller can
/// POST `/projects` with the right pair of fields.
#[derive(Debug)]
pub(crate) struct ResolvedNamespace {
    pub namespace_id: Option<u64>,
    pub path: String,
}

impl ApiProject {
    /// Convert the GitLab project payload into the shared
    /// `RepoSummary`. `full_name` is derived from the project's
    /// `path_with_namespace` when GitLab provides it; otherwise the
    /// namespace `path` and the project `path` are joined so the
    /// summary is still meaningful.
    pub(crate) fn into_summary(self) -> RepoSummary {
        let full_name = self
            .path_with_namespace
            .clone()
            .or_else(|| {
                self.namespace
                    .as_ref()
                    .and_then(|namespace| namespace.full_path.clone().or(namespace.path.clone()))
                    .map(|namespace_path| format!("{namespace_path}/{}", self.path))
            })
            .unwrap_or_else(|| self.path.clone());
        let owner = self
            .namespace
            .as_ref()
            .and_then(|namespace| namespace.full_path.clone().or(namespace.path.clone()))
            .or_else(|| {
                self.path_with_namespace
                    .as_ref()
                    .and_then(|value| value.rsplit_once('/').map(|(owner, _)| owner.to_owned()))
            })
            .unwrap_or_default();
        let private = matches!(
            self.visibility.as_deref(),
            Some("private") | Some("internal") | None
        ) && self.visibility.as_deref() != Some("public");
        RepoSummary {
            full_name,
            owner,
            name: self.path,
            private,
            clone_url: self.http_url_to_repo,
            ssh_url: self.ssh_url_to_repo,
            html_url: self.web_url,
        }
    }
}

impl GitlabProvider {
    /// Resolve an authenticated-user namespace id from GitLab. Phase 3
    /// deliberately resolves namespaces lazily: when the orchestrator
    /// passes a bare `REPOSITORY` (no `OWNER/` prefix), this method
    /// fetches the current user via `/user` and returns its numeric
    /// id so the project lands in the caller's personal namespace,
    /// matching the Forgejo behaviour.
    pub(crate) fn current_user_id(&self) -> Result<u64, ForgejoError> {
        #[derive(serde::Deserialize)]
        struct CurrentUser {
            id: u64,
        }
        let user: CurrentUser = self.http.get("user", &[], "repo create")?;
        Ok(user.id)
    }

    /// Resolve an `OWNER` path to a numeric GitLab namespace id by
    /// searching `/namespaces?search=OWNER`. The endpoint returns
    /// namespaces whose path contains the search term; the resolver
    /// filters to exact matches and distinguishes `user` from
    /// `group` namespaces so the operator never lands a project
    /// under the wrong namespace:
    ///
    ///   * zero matches → structured config error (the operator
    ///     must pick a different OWNER or pass an explicit id).
    ///   * exactly one matching `group` → use that group id
    ///     (groups are the typical meaning of `OWNER/REPO`).
    ///   * exactly one matching `user` → use that user id (handles
    ///     cross-account repos that target another user's namespace).
    ///   * any other combination (multiple groups, multiple users,
    ///     or a mix of both kinds) → structured config error
    ///     instructing the operator to disambiguate.
    pub(crate) fn resolve_owner_namespace_id(&self, owner: &str) -> Result<u64, ForgejoError> {
        if owner.is_empty() {
            return Err(ForgejoError::config(
                "GitLab repo create requires a non-empty OWNER",
            ));
        }
        let candidates: Vec<ApiNamespace> = self.http.paginate("repo create", |http, page| {
            http.get_page::<ApiNamespace>(
                "namespaces",
                &[("search", owner.to_owned()), ("page", page.to_string())],
                "repo create",
            )
        })?;
        let exact: Vec<&ApiNamespace> = candidates
            .iter()
            .filter(|namespace| namespace.path.as_deref() == Some(owner))
            .collect();
        if exact.is_empty() {
            return Err(ForgejoError::config(format!(
                "GitLab namespace '{owner}' was not found; pass a different OWNER \
                  or supply an explicit namespace id"
            )));
        }
        let groups: Vec<u64> = exact
            .iter()
            .filter(|namespace| namespace.kind.as_deref() == Some("group"))
            .map(|namespace| namespace.id)
            .collect();
        let users: Vec<u64> = exact
            .iter()
            .filter(|namespace| namespace.kind.as_deref() == Some("user"))
            .map(|namespace| namespace.id)
            .collect();
        if !groups.is_empty() && users.is_empty() {
            if groups.len() == 1 {
                return Ok(groups[0]);
            }
            return Err(ForgejoError::config(format!(
                "GitLab group namespace '{owner}' is ambiguous \
                  (matched {} groups); pass an explicit namespace id",
                groups.len()
            )));
        }
        if groups.is_empty() && !users.is_empty() {
            if users.len() == 1 {
                return Ok(users[0]);
            }
            return Err(ForgejoError::config(format!(
                "GitLab user namespace '{owner}' is ambiguous \
                  (matched {} users); pass an explicit namespace id",
                users.len()
            )));
        }
        Err(ForgejoError::config(format!(
            "GitLab namespace '{owner}' is ambiguous (matched {} group(s) and {} user(s)); \
              pass an explicit namespace id to disambiguate",
            groups.len(),
            users.len()
        )))
    }

    /// Map the orchestrator's `OWNER/REPOSITORY` target to either an
    /// explicit `namespace_id` (when the owner resolves to a known
    /// group or user) or the authenticated user's namespace id. The
    /// function never returns a guessed namespace id; when the
    /// caller passes a bare `REPOSITORY` (no slash) the function
    /// returns the user's namespace id so a project lands in the
    /// caller's personal namespace, matching Forgejo's behaviour.
    ///
    /// The `(Some(namespace), None)` arm leaves `namespace_id`
    /// unset so the caller (currently [`GitlabProvider::create_repo`])
    /// resolves the owner via [`Self::resolve_owner_namespace_id`]
    /// before issuing POST `/projects`. The pure helper has no
    /// network access of its own.
    pub(crate) fn resolve_namespace_target(
        target: &str,
        explicit_namespace_id: Option<u64>,
        current_user_id: u64,
    ) -> Result<ResolvedNamespace, ForgejoError> {
        if target.is_empty() {
            return Err(ForgejoError::config(
                "GitLab repo create requires a non-empty target",
            ));
        }
        let (namespace, path) = match target.split_once('/') {
            Some((owner, name)) if !owner.is_empty() && !name.is_empty() => {
                (Some(owner.to_owned()), name.to_owned())
            }
            Some((owner, name)) if owner.is_empty() && !name.is_empty() => (None, name.to_owned()),
            _ => (None, target.to_owned()),
        };
        let namespace_id = match (namespace, explicit_namespace_id) {
            (Some(_), Some(id)) => Some(id),
            (None, Some(id)) => Some(id),
            (None, None) => Some(current_user_id),
            (Some(_), None) => None,
        };
        Ok(ResolvedNamespace { namespace_id, path })
    }

    /// `POST /projects` with the orchestrator's repo create payload.
    /// Private-only by contract; the caller must already have
    /// validated `--private` so a non-private call surfaces a
    /// structured error before any network traffic.
    pub(crate) fn create_repo(
        &self,
        target: &str,
        private: bool,
        description: &str,
        auto_init: bool,
    ) -> Result<RepoSummary, ForgejoError> {
        if !private {
            return Err(ForgejoError::config(
                "repo create requires a private repository",
            ));
        }
        let current_user_id = self.current_user_id()?;
        let mut resolved = Self::resolve_namespace_target(target, None, current_user_id)?;
        if resolved.namespace_id.is_none() {
            // The caller passed OWNER/REPO without an explicit
            // namespace id; resolve OWNER to its numeric id via the
            // authenticated namespaces endpoint so POST /projects
            // cannot silently fall back to the personal namespace.
            let owner = target.split_once('/').map(|(owner, _)| owner).unwrap_or("");
            resolved.namespace_id = Some(self.resolve_owner_namespace_id(owner)?);
        }
        let payload = NewProject {
            name: &resolved.path,
            path: Some(&resolved.path),
            namespace_id: resolved.namespace_id,
            namespace: None,
            visibility: "private",
            description,
            initialize_with_readme: auto_init,
        };
        let project: ApiProject = self
            .http
            .post(&self.projects_path(), &payload, "repo create")?;
        Ok(project.into_summary())
    }

    pub(crate) fn list_projects(
        &self,
    ) -> Result<Vec<crate::providers::redmine::model::RedmineProject>, ForgejoError> {
        // GitLab project enumeration is part of Phase 3; surface the
        // structured not-supported error so callers do not silently
        // see a Redmine-shaped result.
        let _ = self;
        Err(ForgejoError::not_supported("gitlab", "project list"))
    }
}

#[cfg(test)]
mod tests {
    use crate::providers::gitlab::GitlabProvider;

    #[test]
    fn resolve_namespace_target_personal_path() {
        let resolved = GitlabProvider::resolve_namespace_target("widget", None, 7).unwrap();
        assert_eq!(resolved.path, "widget");
        assert_eq!(resolved.namespace_id, Some(7));
    }

    #[test]
    fn resolve_namespace_target_owner_repo_keeps_owner_without_explicit_id() {
        let resolved = GitlabProvider::resolve_namespace_target("acme/widgets", None, 7).unwrap();
        assert_eq!(resolved.path, "widgets");
        // Owner supplied without an explicit id: leave it None so a
        // caller that wants strict namespace resolution must ask for it.
        assert_eq!(resolved.namespace_id, None);
    }

    #[test]
    fn resolve_namespace_target_owner_repo_with_explicit_id() {
        let resolved =
            GitlabProvider::resolve_namespace_target("acme/widgets", Some(99), 7).unwrap();
        assert_eq!(resolved.path, "widgets");
        assert_eq!(resolved.namespace_id, Some(99));
    }

    #[test]
    fn resolve_namespace_target_empty_string_errors() {
        let error = GitlabProvider::resolve_namespace_target("", None, 7).unwrap_err();
        assert!(error.to_string().contains("non-empty"));
    }
}
