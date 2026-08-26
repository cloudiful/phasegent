use serde::Serialize;
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const DEFAULT_PAGE: usize = 1;
pub(crate) const DEFAULT_LIMIT: usize = 50;
pub(crate) const DEFAULT_LOG_TAIL: usize = 100;
pub(crate) const MAX_LOG_LINES: usize = 1_000;
pub(crate) const MAX_LOG_BYTES: usize = 16 * 1024;
pub(crate) const DEFAULT_INSPECT_TIMEOUT: u64 = 900;
pub(crate) const DEFAULT_INSPECT_POLL: u64 = 5;

#[derive(Clone, Debug, Serialize)]
pub struct CiRunSummary {
    pub id: u64,
    pub run_number: u64,
    pub status: String,
    pub conclusion: Option<String>,
    pub head_sha: Option<String>,
    pub commit_sha: Option<String>,
    #[serde(rename = "ref")]
    pub ref_name: Option<String>,
    #[serde(rename = "prettyref")]
    pub pretty_ref: Option<String>,
    pub workflow_id: Option<Value>,
    pub html_url: Option<String>,
    pub created: Option<String>,
    pub started: Option<String>,
    pub stopped: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CiRunsOutput {
    pub workflow_runs: Vec<CiRunSummary>,
    pub total_count: Option<usize>,
    pub page: usize,
    pub limit: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct CiJobSummary {
    pub id: u64,
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub run_id: Option<u64>,
    pub attempt: Option<Value>,
    pub task_id: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct CiJobsOutput {
    pub run_id: u64,
    pub jobs: Vec<CiJobSummary>,
}

#[derive(Debug, Serialize)]
pub struct CiJobLogsOutput {
    pub job_id: u64,
    pub log: String,
    pub truncated: bool,
    pub bytes: usize,
}

#[derive(Clone, Debug)]
pub struct CiRunsFilter {
    pub sha: Option<String>,
    pub ref_name: Option<String>,
    pub status: Option<String>,
    pub workflow: Option<String>,
    pub page: usize,
    pub limit: usize,
}

#[derive(Clone, Debug)]
pub struct CiInspectRequest {
    pub sha: String,
    pub ref_name: Option<String>,
    pub wait: bool,
    pub timeout: u64,
    pub poll: u64,
}

#[derive(Debug, Serialize)]
pub struct CiLogExcerpt {
    pub job_id: u64,
    pub name: String,
    pub log: String,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct CiInspectOutput {
    pub state: String,
    pub selected_run: Option<CiRunSummary>,
    pub sha: String,
    #[serde(rename = "ref")]
    pub ref_name: Option<String>,
    pub url: Option<String>,
    pub failed_jobs: Vec<CiJobSummary>,
    pub log_excerpts: Vec<CiLogExcerpt>,
    pub checked_at: String,
    pub poll_count: usize,
}

pub(crate) fn checked_at() -> String {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or_else(
        |_| "0".to_owned(),
        |duration| duration.as_secs().to_string(),
    )
}

pub(crate) fn bound_log(raw: &str, requested_tail: usize) -> (String, bool, usize) {
    let tail = requested_tail.min(MAX_LOG_LINES);
    let lines = raw.lines().collect::<Vec<_>>();
    let start = lines.len().saturating_sub(tail);
    let selected = lines[start..].join("\n");
    let mut truncated = start > 0;
    let bounded = if selected.len() > MAX_LOG_BYTES {
        truncated = true;
        let start = selected.len() - MAX_LOG_BYTES;
        let start = selected
            .char_indices()
            .find(|(index, _)| *index >= start)
            .map_or(0, |(index, _)| index);
        selected[start..].to_owned()
    } else {
        selected
    };
    let bytes = bounded.len();
    (bounded, truncated, bytes)
}

pub(crate) fn pretty_ref(value: &str) -> Option<String> {
    let value = value
        .strip_prefix("refs/heads/")
        .or_else(|| value.strip_prefix("refs/tags/"))
        .unwrap_or(value);
    (!value.is_empty()).then(|| value.to_owned())
}
