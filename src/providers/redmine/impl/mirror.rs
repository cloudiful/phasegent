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
