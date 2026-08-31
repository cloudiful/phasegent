use crate::providers::api::ForgejoError;
use crate::providers::redmine::model::{
    RedmineGitMirrorOutcome, RedmineGitMirrorRequest, RedmineGitMirrorResponse,
};

/// Register the current repository's Git URL with the `redmine_git_mirror`
/// plugin, returning the resulting `RedmineGitMirrorOutcome`.
///
/// The helper expects the Redmine **base URL** (no `/api/v1` suffix) plus a
/// deployment-level bearer key read from
/// `PHASEGENT_REDMINE_GIT_MIRROR_API_KEY`. The plugin's canonical mirror
/// identifier is `mirror_<project_id>_<owner>_<repo>` (lowercased); we
/// `GET` it first so a previously-queued or already-ready mirror is
/// reported as `existing`/`pending`/`cloning`/`ready` without a duplicate
/// POST. A missing entry triggers a `POST
/// /sys/redmine_git_mirror/projects/<id>/repository`, and an entry whose
/// plugin-reported status is `failed` triggers the same POST once to
/// requeue the mirror (a failed job never converges on its own); the
/// queued URL always comes from this function's arguments, never from the
/// plugin response.
pub fn register_git_mirror(
    redmine_base_url: &str,
    project_id: u64,
    owner: &str,
    repo: &str,
    remote_url: &str,
) -> Result<RedmineGitMirrorOutcome, ForgejoError> {
    if project_id == 0 {
        return Err(ForgejoError::config(
            "Redmine project id must be greater than zero to register a git mirror",
        ));
    }
    if remote_url.trim().is_empty() {
        return Err(ForgejoError::config(
            "git mirror remote URL must not be empty",
        ));
    }
    let storage = crate::infra::storage::Storage::open().map_err(ForgejoError::config)?;
    let bearer_key =
        crate::auth::redmine_git_mirror_api_key(&storage).map_err(ForgejoError::config)?;
    let Some(bearer_key) = bearer_key else {
        return Err(ForgejoError::config(
            "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY is not set; \
             set the Redmine git mirror plugin key in the environment to queue mirrors",
        ));
    };
    let http = crate::providers::redmine::http::RedmineGitMirrorHttp::new(
        redmine_base_url.to_owned(),
        bearer_key,
    )?;
    let identifier = mirror_identifier(project_id, owner, repo);
    let get_path = format!("/sys/redmine_git_mirror/projects/{project_id}/repository/{identifier}");
    let post_path = format!("/sys/redmine_git_mirror/projects/{project_id}/repository");
    let response = match http.get::<RedmineGitMirrorResponse>(&get_path, "mirror get")? {
        crate::providers::redmine::http::RedmineGitMirrorLookup::Found(response) => {
            // Only a `failed` existing mirror is requeued; `pending`,
            // `cloning`, and `ready` stay idempotent with a single GET.
            if response.status.trim().eq_ignore_ascii_case("failed") {
                let body = RedmineGitMirrorRequest::new(remote_url);
                http.post::<RedmineGitMirrorResponse, _>(&post_path, &body, "mirror post")?
            } else {
                response
            }
        }
        crate::providers::redmine::http::RedmineGitMirrorLookup::Missing => {
            let body = RedmineGitMirrorRequest::new(remote_url);
            http.post::<RedmineGitMirrorResponse, _>(&post_path, &body, "mirror post")?
        }
    };
    outcome_from_response(response)
}

pub(crate) fn mirror_identifier(project_id: u64, owner: &str, repo: &str) -> String {
    let owner = owner.trim().to_ascii_lowercase();
    let repo = repo.trim().to_ascii_lowercase();
    format!("mirror_{project_id}_{owner}_{repo}")
}

/// Derive the plugin base URL from the Redmine `api_base`. The Redmine REST
/// client may be configured with or without an `/api/v1` suffix; the plugin
/// lives at the site root (`/sys/...`), so that suffix is stripped when
/// present. The helper keeps the result normalized without a trailing slash
/// so `RedmineGitMirrorHttp::new` receives a stable base.
fn mirror_base_url(api_base: &str) -> String {
    let trimmed = api_base.trim_end_matches('/');
    if let Some(stripped) = trimmed.strip_suffix("/api/v1") {
        stripped.to_owned()
    } else {
        trimmed.to_owned()
    }
}

impl crate::providers::config::RedmineProvider {
    /// Read-only discovery of Redmine projects whose `redmine_git_mirror`
    /// record matches the current Git origin. Uses existing
    /// `list_projects()` pagination and the existing
    /// `RedmineGitMirrorHttp::get` semantics: a `404` is a normal non-match,
    /// while any other HTTP/auth/decode error is propagated as an actionable
    /// error. Never `POST`s and never touches SQLite beyond the read-only
    /// bearer-key resolver.
    ///
    /// The `remote` argument is the already-parsed, credential-free origin
    /// (`repository` is `OWNER/REPOSITORY`, `repository_url` is the
    /// credential-free URL used for mirror registration). Empty or missing
    /// plugin `remote_url` is treated as a non-match. Returned projects
    /// carry only `id`/`name`/`identifier` so Phase 3 can report zero,
    /// single, or ambiguous matches without leaking credentials.
    pub fn discover_matching_projects(
        &self,
        remote: &crate::remote::RemoteRepository,
    ) -> Result<crate::providers::redmine::RedmineDiscovery, crate::providers::api::ForgejoError>
    {
        self.discover_matching_projects_for_urls(&remote.repository, &remote.repository_url)
    }

    /// Variant of [`Self::discover_matching_projects`] that accepts raw
    /// repository and URL strings. Useful for tests and for callers that
    /// already split the origin.
    pub fn discover_matching_projects_for_urls(
        &self,
        repository: &str,
        repository_url: &str,
    ) -> Result<crate::providers::redmine::RedmineDiscovery, crate::providers::api::ForgejoError>
    {
        use crate::providers::api::ForgejoError;
        use crate::providers::redmine::{RedmineDiscoveredProject, RedmineDiscovery};

        let repository = repository.trim();
        if repository.is_empty() {
            return Err(ForgejoError::config(
                "repository must use OWNER/REPOSITORY form",
            ));
        }
        let mut parts = repository.split('/');
        let owner = parts
            .next()
            .ok_or_else(|| ForgejoError::config("repository must use OWNER/REPOSITORY form"))?
            .trim();
        let repo = parts
            .next()
            .ok_or_else(|| ForgejoError::config("repository must use OWNER/REPOSITORY form"))?
            .trim();
        if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
            return Err(ForgejoError::config(
                "repository must use OWNER/REPOSITORY form",
            ));
        }
        if repository_url.trim().is_empty() {
            return Err(ForgejoError::config(
                "git mirror remote URL must not be empty",
            ));
        }
        let canonical_local =
            crate::remote::canonical_git_url(repository_url).map_err(ForgejoError::config)?;

        let projects = self.list_projects()?;
        if projects.is_empty() {
            return Ok(RedmineDiscovery::NoMatch);
        }

        let storage = crate::infra::storage::Storage::open().map_err(ForgejoError::config)?;
        let bearer_key =
            crate::auth::redmine_git_mirror_api_key(&storage).map_err(ForgejoError::config)?;
        let Some(bearer_key) = bearer_key else {
            return Err(ForgejoError::config(
                "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY is not set; \
                 set the Redmine git mirror plugin key in the environment to queue mirrors",
            ));
        };
        let base_url = mirror_base_url(&self.config.api_base);
        let http =
            crate::providers::redmine::http::RedmineGitMirrorHttp::new(base_url, bearer_key)?;

        let mut matches = Vec::new();
        for project in projects {
            let identifier = mirror_identifier(project.id, owner, repo);
            let path = format!(
                "/sys/redmine_git_mirror/projects/{}/repository/{identifier}",
                project.id
            );
            let lookup = http.get::<crate::providers::redmine::model::RedmineGitMirrorResponse>(
                &path,
                "mirror get",
            )?;
            let response = match lookup {
                crate::providers::redmine::http::RedmineGitMirrorLookup::Missing => continue,
                crate::providers::redmine::http::RedmineGitMirrorLookup::Found(resp) => resp,
            };
            let remote_url = match response.remote_url {
                Some(ref url) if !url.trim().is_empty() => url.trim().to_owned(),
                _ => continue,
            };
            let canonical_remote = match crate::remote::canonical_git_url(&remote_url) {
                Ok(value) => value,
                Err(_) => continue,
            };
            if canonical_local == canonical_remote {
                matches.push(RedmineDiscoveredProject {
                    id: project.id,
                    name: project.name.clone(),
                    identifier: project.identifier.clone(),
                });
            }
        }

        match matches.len() {
            0 => Ok(RedmineDiscovery::NoMatch),
            1 => Ok(RedmineDiscovery::Single(
                matches.into_iter().next().unwrap(),
            )),
            _ => Ok(RedmineDiscovery::Multiple(matches)),
        }
    }

    /// Narrowly scoped plugin lookup for a single project. Mirrors the
    /// discovery helper's `GET` semantics: `404` is `None`, empty/missing
    /// `remote_url` is `None`, and any other HTTP/auth/decode error is
    /// propagated. Does not `POST` and does not persist anything.
    pub(crate) fn lookup_mirror_for_project(
        &self,
        project_id: u64,
        owner: &str,
        repo: &str,
    ) -> Result<
        Option<crate::providers::redmine::model::RedmineGitMirrorResponse>,
        crate::providers::api::ForgejoError,
    > {
        use crate::providers::api::ForgejoError;

        if project_id == 0 {
            return Err(ForgejoError::config(
                "Redmine project id must be greater than zero to query a git mirror",
            ));
        }
        let storage = crate::infra::storage::Storage::open().map_err(ForgejoError::config)?;
        let bearer_key =
            crate::auth::redmine_git_mirror_api_key(&storage).map_err(ForgejoError::config)?;
        let Some(bearer_key) = bearer_key else {
            return Err(ForgejoError::config(
                "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY is not set; \
                 set the Redmine git mirror plugin key in the environment to queue mirrors",
            ));
        };
        let base_url = mirror_base_url(&self.config.api_base);
        let http =
            crate::providers::redmine::http::RedmineGitMirrorHttp::new(base_url, bearer_key)?;
        let identifier = mirror_identifier(project_id, owner, repo);
        let path = format!("/sys/redmine_git_mirror/projects/{project_id}/repository/{identifier}");
        let lookup = http.get::<crate::providers::redmine::model::RedmineGitMirrorResponse>(
            &path,
            "mirror get",
        )?;
        match lookup {
            crate::providers::redmine::http::RedmineGitMirrorLookup::Missing => Ok(None),
            crate::providers::redmine::http::RedmineGitMirrorLookup::Found(response) => {
                match response.remote_url.as_deref().map(str::trim) {
                    Some(url) if !url.is_empty() => Ok(Some(response)),
                    _ => Ok(None),
                }
            }
        }
    }
}

fn outcome_from_response(
    response: RedmineGitMirrorResponse,
) -> Result<RedmineGitMirrorOutcome, ForgejoError> {
    let status = response.status.trim().to_ascii_lowercase();
    if status == "failed" {
        let detail = response
            .error
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "the plugin reported a failed mirror status".to_owned());
        return Err(ForgejoError::config(format!(
            "Redmine git mirror plugin reported a failed status for {}: {}",
            response.identifier, detail
        )));
    }
    Ok(RedmineGitMirrorOutcome {
        id: response.id,
        project_id: response.project_id,
        identifier: response.identifier,
        status: normalise_status(response.status.as_str()),
        remote_url: response.remote_url.unwrap_or_default(),
        local_path: response.local_path.unwrap_or_default(),
        error: response.error,
    })
}

fn normalise_status(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "pending" | "cloning" | "ready" | "failed" => raw.trim().to_ascii_lowercase(),
        other => other.to_owned(),
    }
}
