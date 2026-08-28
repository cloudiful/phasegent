//! Pipeline / job DTOs and status mappings.

use serde::Deserialize;

/// JSON payload returned by `GET /projects/:id/pipelines` and the
/// single-pipeline endpoint.
#[derive(Debug, Deserialize)]
pub(crate) struct ApiPipeline {
    pub id: u64,
    #[serde(default)]
    pub iid: u64,
    #[serde(default)]
    pub status: String,
    /// GitLab returns the branch / tag name as the JSON key `ref`.
    /// The orchestrator uses `ref_name` internally to avoid the
    /// Rust reserved word.
    #[serde(default, rename = "ref")]
    pub ref_name: Option<String>,
    #[serde(default)]
    pub sha: Option<String>,
    #[serde(default)]
    pub before_sha: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub web_url: Option<String>,
}

/// JSON payload returned by `GET /projects/:id/pipelines/:pipeline_id/jobs`.
#[derive(Debug, Deserialize)]
pub(crate) struct ApiJob {
    pub id: u64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub pipeline: Option<ApiJobPipelineRef>,
    #[serde(default)]
    pub queued_duration: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiJobPipelineRef {
    #[serde(default)]
    pub id: Option<u64>,
}

/// Map a GitLab pipeline `status` value to the orchestrator's shared
/// `CiRunSummary::status` vocabulary. The Forgejo mapping is the
/// reference; we keep the same lowercase string here so downstream
/// consumers can compare them without special-casing GitLab.
///
/// GitLab exposes `created`, `waiting_for_resource`, `preparing`,
/// `pending`, `running`, `success`, `failed`, `canceled`, `skipped`,
/// `manual`, and `scheduled`. The shared vocabulary keeps
/// `running` / `pending` / `success` / `failure` semantics and adds
/// `canceled`, `skipped`, and `manual` because those GitLab states
/// do not map cleanly to either `running` or `failure`.
pub(crate) fn pipeline_status_from_gitlab(status: &str) -> String {
    let normalised = status.to_ascii_lowercase();
    match normalised.as_str() {
        "created" | "waiting_for_resource" | "preparing" | "pending" | "scheduled" => {
            "pending".to_owned()
        }
        "running" => "running".to_owned(),
        "success" => "success".to_owned(),
        "failed" => "failure".to_owned(),
        "canceled" | "cancelled" => "cancelled".to_owned(),
        "skipped" => "skipped".to_owned(),
        "manual" => "manual".to_owned(),
        // Unknown future values: keep them visible rather than silently
        // remapping to "unknown".
        other => other.to_owned(),
    }
}

/// Resolve the optional `conclusion` field that GitLab exposes for
/// finished pipelines / jobs. The shared model uses `None` while the
/// pipeline is still running and the literal conclusion string once
/// the pipeline finishes. GitLab returns the same string as `status`
/// for finished pipelines, so the status is the authoritative source.
pub(crate) fn pipeline_conclusion_from_gitlab(
    status: &str,
    conclusion: Option<&str>,
) -> Option<String> {
    let normalised = status.to_ascii_lowercase();
    match normalised.as_str() {
        "success" | "failed" | "canceled" | "cancelled" | "skipped" => Some(normalised),
        "running"
        | "pending"
        | "created"
        | "waiting_for_resource"
        | "preparing"
        | "scheduled"
        | "manual" => None,
        _ => conclusion
            .filter(|value| !value.trim().is_empty())
            .map(|value| value.to_ascii_lowercase()),
    }
}

#[cfg(test)]
mod tests {
    use super::{pipeline_conclusion_from_gitlab, pipeline_status_from_gitlab};

    #[test]
    fn pipeline_status_from_gitlab_maps_known_states() {
        assert_eq!(pipeline_status_from_gitlab("running"), "running");
        assert_eq!(pipeline_status_from_gitlab("success"), "success");
        assert_eq!(pipeline_status_from_gitlab("failed"), "failure");
        assert_eq!(pipeline_status_from_gitlab("canceled"), "cancelled");
        assert_eq!(pipeline_status_from_gitlab("cancelled"), "cancelled");
        assert_eq!(pipeline_status_from_gitlab("skipped"), "skipped");
        assert_eq!(pipeline_status_from_gitlab("manual"), "manual");
        assert_eq!(pipeline_status_from_gitlab("pending"), "pending");
        assert_eq!(pipeline_status_from_gitlab("created"), "pending");
        assert_eq!(
            pipeline_status_from_gitlab("waiting_for_resource"),
            "pending"
        );
        assert_eq!(pipeline_status_from_gitlab("preparing"), "pending");
        assert_eq!(pipeline_status_from_gitlab("scheduled"), "pending");
    }

    #[test]
    fn pipeline_status_from_gitlab_preserves_unknown_states() {
        // Unknown GitLab statuses must surface unchanged so an
        // operator can spot them rather than seeing a silent
        // remapping to "unknown".
        assert_eq!(
            pipeline_status_from_gitlab("brand-new-state"),
            "brand-new-state"
        );
    }

    #[test]
    fn pipeline_conclusion_uses_status_for_terminal_states() {
        assert_eq!(
            pipeline_conclusion_from_gitlab("success", Some("ignored")),
            Some("success".to_owned())
        );
        assert_eq!(
            pipeline_conclusion_from_gitlab("failed", Some("ignored")),
            Some("failed".to_owned())
        );
        assert_eq!(
            pipeline_conclusion_from_gitlab("cancelled", None),
            Some("cancelled".to_owned())
        );
        assert_eq!(
            pipeline_conclusion_from_gitlab("running", Some("ignored")),
            None
        );
        assert_eq!(
            pipeline_conclusion_from_gitlab("pending", Some("ignored")),
            None
        );
    }
}
