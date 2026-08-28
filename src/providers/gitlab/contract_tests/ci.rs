#![allow(unused_imports)]
use super::support::*;
use crate::providers::config::GitlabConfig;
use crate::providers::gitlab::GitlabProvider;
use crate::providers::{CiProvider, IssueProvider, ProviderDispatcher, RepoProvider};

#[test]
fn ci_runs_hits_pipelines_endpoint_with_filters() {
    use crate::ci_model::CiRunsFilter;
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(format!(
            "[{}]",
            pipeline_payload(11, 1, "success", "refs/heads/main", "abc123")
        ))
        .with_header("x-next-page", ""),
    ]);
    let provider = provider(base);
    let filter = CiRunsFilter {
        sha: Some("abc123".to_owned()),
        ref_name: Some("refs/heads/main".to_owned()),
        status: Some("success".to_owned()),
        workflow: Some("ci.yml".to_owned()),
        page: 1,
        limit: 50,
    };
    let output = provider.ci_runs(&filter).unwrap();
    assert_eq!(output.workflow_runs.len(), 1);
    let run = &output.workflow_runs[0];
    assert_eq!(run.id, 11);
    assert_eq!(run.run_number, 1);
    assert_eq!(run.status, "success");
    assert_eq!(run.ref_name.as_deref(), Some("refs/heads/main"));
    assert_eq!(run.commit_sha.as_deref(), Some("abc123"));
    let requests = requests.recv().unwrap();
    let request = &requests[0];
    assert!(
        request.starts_with("GET /api/v4/projects/42/pipelines?"),
        "{request}",
    );
    assert!(request.contains("sha=abc123"));
    assert!(request.contains("ref=refs%2Fheads%2Fmain"));
    assert!(request.contains("status=success"));
    // page is always emitted by the helper.
    assert!(request.contains("page=1"));
    server.join().unwrap();
}

#[test]
fn ci_runs_paginates_until_x_next_page_is_empty() {
    use crate::ci_model::CiRunsFilter;
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(format!(
            "[{}]",
            pipeline_payload(11, 1, "success", "main", "aaa")
        ))
        .with_header("x-next-page", "2"),
        MockResponse::ok(format!(
            "[{}]",
            pipeline_payload(12, 2, "failed", "main", "bbb")
        ))
        .with_header("x-next-page", ""),
    ]);
    let provider = provider(base);
    let filter = CiRunsFilter {
        sha: None,
        ref_name: None,
        status: None,
        workflow: None,
        page: 1,
        limit: 50,
    };
    let output = provider.ci_runs(&filter).unwrap();
    assert_eq!(output.workflow_runs.len(), 2);
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("page=1"));
    assert!(requests[1].contains("page=2"));
    server.join().unwrap();
}

#[test]
fn ci_runs_maps_status_through_shared_vocabulary() {
    use crate::ci_model::CiRunsFilter;
    let (base, _requests, server) = sequence(vec![
        MockResponse::ok(format!(
            "[{},{},{},{}]",
            pipeline_payload(1, 1, "running", "main", "a"),
            pipeline_payload(2, 2, "failed", "main", "b"),
            pipeline_payload(3, 3, "canceled", "main", "c"),
            pipeline_payload(4, 4, "skipped", "main", "d"),
        ))
        .with_header("x-next-page", ""),
    ]);
    let provider = provider(base);
    let filter = CiRunsFilter {
        sha: None,
        ref_name: None,
        status: None,
        workflow: None,
        page: 1,
        limit: 50,
    };
    let output = provider.ci_runs(&filter).unwrap();
    assert_eq!(output.workflow_runs.len(), 4);
    assert_eq!(output.workflow_runs[0].status, "running");
    assert_eq!(output.workflow_runs[1].status, "failure");
    assert_eq!(output.workflow_runs[2].status, "cancelled");
    assert_eq!(output.workflow_runs[3].status, "skipped");
    // The conclusion field is sourced from the terminal status only;
    // non-terminal statuses keep `conclusion: None`.
    assert_eq!(output.workflow_runs[0].conclusion, None);
    assert_eq!(
        output.workflow_runs[1].conclusion.as_deref(),
        Some("failed")
    );
    assert_eq!(
        output.workflow_runs[2].conclusion.as_deref(),
        Some("canceled")
    );
    assert_eq!(
        output.workflow_runs[3].conclusion.as_deref(),
        Some("skipped")
    );
    server.join().unwrap();
}

#[test]
fn ci_runs_preserves_unrecognised_statuses_unchanged() {
    // Future GitLab status values must surface in the shared JSON
    // contract rather than be silently remapped to "unknown".
    use crate::ci_model::CiRunsFilter;
    let (base, _requests, server) = sequence(vec![
        MockResponse::ok(format!(
            "[{}]",
            pipeline_payload(1, 1, "future-state", "main", "a")
        ))
        .with_header("x-next-page", ""),
    ]);
    let provider = provider(base);
    let filter = CiRunsFilter {
        sha: None,
        ref_name: None,
        status: None,
        workflow: None,
        page: 1,
        limit: 50,
    };
    let output = provider.ci_runs(&filter).unwrap();
    assert_eq!(output.workflow_runs[0].status, "future-state");
    server.join().unwrap();
}

#[test]
fn ci_run_get_hits_single_pipeline_endpoint() {
    let (result, request) = one(
        MockResponse::ok(pipeline_payload(11, 7, "running", "main", "abc")),
        |provider| provider.ci_run_get(11),
    );
    let run = result.unwrap();
    assert_eq!(run.id, 11);
    assert_eq!(run.run_number, 7);
    assert_eq!(run.status, "running");
    assert_request(&request, "GET", "/api/v4/projects/42/pipelines/11", None);
}

#[test]
fn ci_run_jobs_hits_pipelines_pipeline_id_jobs_endpoint() {
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(format!(
            "[{},{}]",
            job_payload(101, "lint", "success", Some("success")),
            job_payload(102, "test", "failed", Some("failed")),
        ))
        .with_header("x-next-page", ""),
    ]);
    let provider = provider(base);
    let output = provider.ci_run_jobs(11).unwrap();
    assert_eq!(output.run_id, 11);
    assert_eq!(output.jobs.len(), 2);
    assert_eq!(output.jobs[0].name, "lint");
    assert_eq!(output.jobs[1].status, "failure");
    let requests = requests.recv().unwrap();
    let request = &requests[0];
    assert!(
        request.starts_with("GET /api/v4/projects/42/pipelines/11/jobs?"),
        "{request}",
    );
    server.join().unwrap();
}

#[test]
fn ci_job_logs_hits_jobs_job_id_trace_endpoint_with_bounded_tail() {
    use crate::ci_model::DEFAULT_LOG_TAIL;
    let trace = (0..2000)
        .map(|index| format!("line-{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let (base, requests, server) = sequence(vec![MockResponse::ok(trace.clone())]);
    let provider = provider(base);
    let output = provider.ci_job_logs(101, 10).unwrap();
    assert_eq!(output.job_id, 101);
    assert!(output.truncated);
    assert!(output.bytes <= output.log.len());
    let tail_lines = output.log.lines().collect::<Vec<_>>();
    assert_eq!(tail_lines.len(), 10);
    assert_eq!(tail_lines.last().copied(), Some("line-1999"));
    let request = &requests.recv().unwrap()[0];
    assert!(
        request.starts_with("GET /api/v4/projects/42/jobs/101/trace"),
        "{request}",
    );
    // Tail must not leak the synthetic token when the trace mentions it.
    let _ = DEFAULT_LOG_TAIL;
    server.join().unwrap();
}

#[test]
fn ci_job_logs_redacts_token_from_raw_trace() {
    // Use the synthetic TEST_TOKEN as the secret so the redaction
    // path actually fires. The trace must surface `[redacted]`
    // instead of the raw token.
    let trace = format!("first line\n{TEST_TOKEN}\nlast line");
    let (result, _request) = one(MockResponse::ok(trace), |provider| {
        provider.ci_job_logs(1, 100)
    });
    let output = result.unwrap();
    assert!(
        !output.log.contains(TEST_TOKEN),
        "raw trace must not leak the token: {}",
        output.log,
    );
    assert!(output.log.contains("[redacted]"));
}

#[test]
fn ci_job_logs_returns_404_as_structured_error() {
    let (result, _request) = one(
        MockResponse::status(404, r#"{"message":"404 Not found"}"#),
        |provider| provider.ci_job_logs(99, 100),
    );
    let error = result.unwrap_err();
    let rendered = error.json();
    assert_eq!(rendered["kind"], "http");
    assert_eq!(rendered["status"], 404);
    assert_eq!(rendered["operation"], "ci job logs");
}
