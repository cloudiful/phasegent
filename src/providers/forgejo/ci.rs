use crate::ci_model::{
    CiJobLogsOutput, CiJobSummary, CiJobsOutput, CiRunSummary, CiRunsFilter, CiRunsOutput,
    bound_log, pretty_ref,
};
use crate::providers::api::ForgejoError;
use crate::providers::forgejo::ForgejoProvider;
use reqwest::blocking::RequestBuilder;
use reqwest::header::ACCEPT;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

#[derive(Debug, Deserialize)]
struct ApiActionRun {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    run_number: u64,
    #[serde(default)]
    status: String,
    conclusion: Option<String>,
    head_sha: Option<String>,
    commit_sha: Option<String>,
    head_commit: Option<ApiHeadCommit>,
    #[serde(rename = "ref")]
    ref_name: Option<String>,
    head_branch: Option<String>,
    #[serde(alias = "prettyref")]
    pretty_ref: Option<String>,
    workflow_id: Option<Value>,
    #[serde(alias = "url")]
    html_url: Option<String>,
    #[serde(alias = "created")]
    created_at: Option<String>,
    #[serde(alias = "run_started_at", alias = "started")]
    started_at: Option<String>,
    #[serde(alias = "stopped")]
    stopped_at: Option<String>,
    #[serde(alias = "completed")]
    completed_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiHeadCommit {
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiActionJob {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    status: String,
    conclusion: Option<String>,
    run_id: Option<u64>,
    attempt: Option<Value>,
    #[serde(alias = "run_attempt")]
    workflow_run_attempt: Option<Value>,
    task_id: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct ApiRunListObject {
    #[serde(default)]
    workflow_runs: Vec<ApiActionRun>,
    total_count: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ApiRunList {
    Object(ApiRunListObject),
    Array(Vec<ApiActionRun>),
}

#[derive(Debug, Deserialize)]
struct ApiJobListObject {
    #[serde(default)]
    jobs: Vec<ApiActionJob>,
    #[allow(dead_code)]
    total_count: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ApiJobList {
    Object(ApiJobListObject),
    Array(Vec<ApiActionJob>),
}

impl ForgejoProvider {
    pub(crate) fn ci_runs(&self, filter: &CiRunsFilter) -> Result<CiRunsOutput, ForgejoError> {
        let mut query = vec![
            ("page", filter.page.to_string()),
            ("limit", filter.limit.to_string()),
        ];
        if let Some(sha) = &filter.sha {
            query.push(("head_sha", sha.clone()));
        }
        if let Some(ref_name) = &filter.ref_name {
            query.push(("ref", ref_name.clone()));
        }
        if let Some(status) = &filter.status {
            query.push(("status", status.clone()));
        }
        if let Some(workflow) = &filter.workflow {
            query.push(("workflow_id", workflow.clone()));
        }
        let response: ApiRunList = self.ci_json(
            &format!("{}/actions/runs", self.repository_path()),
            &query,
            "ci runs",
        )?;
        let (runs, total_count) = match response {
            ApiRunList::Object(response) => (response.workflow_runs, response.total_count),
            ApiRunList::Array(runs) => (runs, None),
        };
        Ok(CiRunsOutput {
            workflow_runs: runs.into_iter().map(Into::into).collect(),
            total_count,
            page: filter.page,
            limit: filter.limit,
        })
    }

    pub(crate) fn ci_run_get(&self, run_id: u64) -> Result<CiRunSummary, ForgejoError> {
        let run: ApiActionRun = self.ci_json(
            &format!("{}/actions/runs/{run_id}", self.repository_path()),
            &[],
            "ci run get",
        )?;
        Ok(run.into())
    }

    pub(crate) fn ci_run_jobs(&self, run_id: u64) -> Result<CiJobsOutput, ForgejoError> {
        let response: ApiJobList = self.ci_json(
            &format!("{}/actions/runs/{run_id}/jobs", self.repository_path()),
            &[],
            "ci run jobs",
        )?;
        let jobs = match response {
            ApiJobList::Object(response) => response.jobs,
            ApiJobList::Array(jobs) => jobs,
        };
        Ok(CiJobsOutput {
            run_id,
            jobs: jobs.into_iter().map(Into::into).collect(),
        })
    }

    pub(crate) fn ci_job_logs(
        &self,
        job_id: u64,
        tail: usize,
    ) -> Result<CiJobLogsOutput, ForgejoError> {
        let log = self.ci_text(
            &format!("{}/actions/jobs/{job_id}/logs", self.repository_path()),
            "ci job logs",
        )?;
        let (log, truncated, bytes) = bound_log(&log, tail);
        Ok(CiJobLogsOutput {
            job_id,
            log,
            truncated,
            bytes,
        })
    }

    fn ci_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        operation: &str,
    ) -> Result<T, ForgejoError> {
        // CI JSON reads are safe GETs; retry on transient 429/502/503/504 and
        // transport timeouts with bounded backoff.
        let request = self.client.get(path).query(query);
        let (status, _headers, text) = crate::infra::http_client::fetch_with_retry(
            self.authorized(request),
            operation,
            |message| message.to_owned(),
        )?;
        crate::providers::forgejo::http::decode_from_parts(status, &text, operation)
    }

    fn ci_text(&self, path: &str, operation: &str) -> Result<String, ForgejoError> {
        // CI text reads (job logs) are safe GETs; retry with the same policy.
        let request = self.client.get(path).query(&[] as &[(&str, String)]);
        let (status, _headers, text) = crate::infra::http_client::fetch_with_retry(
            self.authorized(request),
            operation,
            |message| message.to_owned(),
        )?;
        crate::providers::forgejo::http::decode_text_from_parts(status, text, operation)
    }

    fn authorized(&self, request: RequestBuilder) -> RequestBuilder {
        request
            .header(ACCEPT, "application/json")
            .bearer_auth(&self.token)
    }
}

impl From<ApiActionRun> for CiRunSummary {
    fn from(run: ApiActionRun) -> Self {
        let ref_name = run.ref_name.clone().or(run.head_branch);
        Self {
            id: run.id,
            run_number: run.run_number,
            status: run.status,
            conclusion: run.conclusion,
            head_sha: run.head_sha,
            commit_sha: run
                .commit_sha
                .or_else(|| run.head_commit.and_then(|commit| commit.id)),
            pretty_ref: run
                .pretty_ref
                .or_else(|| ref_name.as_deref().and_then(pretty_ref)),
            ref_name,
            workflow_id: run.workflow_id,
            html_url: run.html_url,
            created: run.created_at,
            started: run.started_at,
            stopped: run.stopped_at.or(run.completed_at),
        }
    }
}

impl From<ApiActionJob> for CiJobSummary {
    fn from(job: ApiActionJob) -> Self {
        Self {
            id: job.id,
            name: job.name,
            status: job.status,
            conclusion: job.conclusion,
            run_id: job.run_id,
            attempt: job.attempt.or(job.workflow_run_attempt),
            task_id: job.task_id,
        }
    }
}
