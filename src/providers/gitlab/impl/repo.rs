//! Namespace resolution and private repository creation, plus the
//! Phase 2 read-only `GET /projects` enumeration that maps onto the
//! shared `RedmineProject` shape.

use crate::providers::api::{ForgejoError, RepoSummary};
use crate::providers::gitlab::model::{ApiNamespace, ApiProject, NewProject};
use crate::providers::redmine::model::RedmineProject;

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
    /// Resolve an authenticated-user namespace id from GitLab. Namespaces
    /// resolve lazily: when the orchestrator passes a bare `REPOSITORY`
    /// (no `OWNER/` prefix), this method fetches the current user via
    /// `/user` and returns its numeric id so the project lands in the
    /// caller's personal namespace, matching the Forgejo behaviour.
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

    /// `GET /projects` paginated across all pages, mapped onto the
    /// shared `RedmineProject` shape so the existing `project list`
    /// CLI command (and any downstream planning flow that consumes
    /// `RedmineProject`) works against GitLab without a separate
    /// code path.
    ///
    /// GitLab returns the project list as a top-level JSON array (no
    /// wrapper); the shared `paginate` helper walks every page,
    /// repeats the `x-next-page`/`x-total-pages` heuristics, and stops
    /// before the safety cap. Each page is mapped onto the shared
    /// `RedmineProject` shape; the mapping is intentionally
    /// conservative — `id` and `path` are required, `name` falls back
    /// to `path` when the API omitted it, and `description` defaults
    /// to an empty string (GitLab returns `null` when the project has
    /// no description).
    pub(crate) fn list_projects(&self) -> Result<Vec<RedmineProject>, ForgejoError> {
        let path = self.projects_path();
        let projects: Vec<ApiProject> = self.http.paginate("project list", |http, page| {
            http.get_page::<ApiProject>(&path, &[("page", page.to_string())], "project list")
        })?;
        Ok(projects.into_iter().map(Into::into).collect())
    }
}

impl From<ApiProject> for RedmineProject {
    fn from(project: ApiProject) -> Self {
        // GitLab's `visibility` maps onto Redmine's `is_public`
        // boolean: only `public` projects are surfaced as public;
        // `private`, `internal`, and the (legacy) absent value all
        // land as not-public so the audit output stays aligned with
        // the Redmine vocabulary. `id` is required by `RedmineProject`
        // so the `Default::default()` fallback covers the rare case
        // where a GitLab instance omits it (older mocked fixtures).
        let is_public = matches!(project.visibility.as_deref(), Some("public"));
        RedmineProject {
            id: project.id.unwrap_or_default(),
            // Redmine `name` is the human-readable label; GitLab's
            // `name` is exactly that, with `path` as the URL slug.
            // Falls back to `path` so the field is never empty.
            name: project
                .name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| project.path.clone()),
            // Redmine uses `identifier` as the URL slug; GitLab's
            // `path` is the equivalent. Falls back to the bare `id`
            // when `path` is somehow missing (it is a required field
            // on the API, so this branch is a defensive fallback).
            identifier: if project.path.trim().is_empty() {
                format!("{}", project.id.unwrap_or_default())
            } else {
                project.path.clone()
            },
            description: project.description.unwrap_or_default(),
            status: None,
            is_public: Some(is_public),
            inherit_members: None,
            created_on: project.created_at,
            updated_on: project.updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::gitlab::GitlabProvider;
    use crate::providers::gitlab::model::{ApiProject, ApiProjectNamespace};

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

    #[test]
    fn api_project_maps_to_redmine_project_for_public_visibility() {
        let api = ApiProject {
            path: "widget".to_owned(),
            path_with_namespace: Some("acme/widget".to_owned()),
            web_url: Some("https://gitlab.example/acme/widget".to_owned()),
            visibility: Some("public".to_owned()),
            namespace: Some(ApiProjectNamespace {
                path: Some("acme".to_owned()),
                full_path: Some("acme".to_owned()),
            }),
            http_url_to_repo: Some("https://gitlab.example/acme/widget.git".to_owned()),
            ssh_url_to_repo: Some("ssh://git@gitlab.example/acme/widget.git".to_owned()),
            id: Some(42),
            name: Some("Widget".to_owned()),
            description: Some("A widget project".to_owned()),
            created_at: Some("2026-09-01T00:00:00.000Z".to_owned()),
            updated_at: Some("2026-09-10T00:00:00.000Z".to_owned()),
            default_branch: Some("main".to_owned()),
            archived: Some(false),
        };
        let project: RedmineProject = api.into();
        assert_eq!(project.id, 42);
        assert_eq!(project.name, "Widget");
        assert_eq!(project.identifier, "widget");
        assert_eq!(project.description, "A widget project");
        assert_eq!(project.is_public, Some(true));
        assert_eq!(
            project.created_on.as_deref(),
            Some("2026-09-01T00:00:00.000Z")
        );
    }

    #[test]
    fn api_project_maps_private_visibility_as_not_public() {
        let api = ApiProject {
            path: "secret".to_owned(),
            path_with_namespace: Some("acme/secret".to_owned()),
            web_url: None,
            visibility: Some("private".to_owned()),
            namespace: None,
            http_url_to_repo: None,
            ssh_url_to_repo: None,
            id: Some(7),
            name: None,
            description: None,
            created_at: None,
            updated_at: None,
            default_branch: None,
            archived: None,
        };
        let project: RedmineProject = api.into();
        assert_eq!(project.id, 7);
        // Missing `name` falls back to `path`.
        assert_eq!(project.name, "secret");
        assert_eq!(project.identifier, "secret");
        // Missing `description` defaults to empty string.
        assert_eq!(project.description, "");
        assert_eq!(project.is_public, Some(false));
    }
}
