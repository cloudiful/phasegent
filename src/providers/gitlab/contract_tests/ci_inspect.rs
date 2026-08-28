#![allow(unused_imports)]
use super::support::*;
use crate::providers::config::GitlabConfig;
use crate::providers::gitlab::GitlabProvider;
use crate::providers::{CiProvider, IssueProvider, ProviderDispatcher, RepoProvider};

#[test]
fn ci_inspect_returns_no_run_when_no_pipeline_matches() {
    use crate::ci_model::CiInspectRequest;
    let (base, requests, server) =
        sequence(vec![MockResponse::ok("[]").with_header("x-next-page", "")]);
    let provider = provider(base);
    let request = CiInspectRequest {
        sha: "deadbeef".to_owned(),
        ref_name: None,
        wait: false,
        timeout: 1,
        poll: 1,
    };
    let output = provider.ci_inspect(&request).unwrap();
    assert_eq!(output.state, "no_run");
    assert!(output.selected_run.is_none());
    assert_eq!(output.poll_count, 1);
    let requests = requests.recv().unwrap();
    assert!(
        requests[0].contains("sha=deadbeef"),
        "sha must be forwarded: {}",
        requests[0],
    );
    server.join().unwrap();
}

#[test]
fn ci_inspect_returns_failure_for_failed_pipeline() {
    use crate::ci_model::CiInspectRequest;
    let (base, requests, server) = sequence(vec![
        // 1. initial runs listing; returned run matches the requested
        // sha and is already in a failed state, so the inspector
        // does not poll.
        MockResponse::ok(format!(
            "[{}]",
            pipeline_payload(11, 7, "failed", "main", "abc123")
        ))
        .with_header("x-next-page", ""),
        // 2. job listing for the failed pipeline.
        MockResponse::ok(format!(
            "[{},{}]",
            job_payload(101, "lint", "success", Some("success")),
            job_payload(102, "test", "failed", Some("failed")),
        ))
        .with_header("x-next-page", ""),
        // 3. job trace for the failing job.
        MockResponse::ok("test failed because of assertion X".to_owned()),
    ]);
    let provider = provider(base);
    let request = CiInspectRequest {
        sha: "abc123".to_owned(),
        ref_name: Some("main".to_owned()),
        wait: false,
        timeout: 1,
        poll: 1,
    };
    let output = provider.ci_inspect(&request).unwrap();
    assert_eq!(output.state, "failure");
    assert_eq!(output.sha, "abc123");
    let selected = output.selected_run.expect("selected run");
    assert_eq!(selected.id, 11);
    assert_eq!(selected.status, "failure");
    assert_eq!(output.failed_jobs.len(), 1);
    assert_eq!(output.failed_jobs[0].id, 102);
    assert_eq!(output.log_excerpts.len(), 1);
    assert_eq!(output.log_excerpts[0].name, "test");
    assert!(output.log_excerpts[0].log.contains("test failed"));
    let requests = requests.recv().unwrap();
    assert!(
        requests[0].starts_with("GET /api/v4/projects/42/pipelines?"),
        "{}",
        requests[0],
    );
    assert!(
        requests[1].starts_with("GET /api/v4/projects/42/pipelines/11/jobs?"),
        "{}",
        requests[1],
    );
    assert!(
        requests[2].starts_with("GET /api/v4/projects/42/jobs/102/trace"),
        "{}",
        requests[2],
    );
    server.join().unwrap();
}

#[test]
fn ci_inspect_treats_skipped_pipeline_as_distinct_non_failure() {
    // GitLab exposes `skipped` as a terminal pipeline state (for
    // example `when: never` rules). The shared Forgejo inspect
    // logic treats `skipped` as a distinct state, never as a
    // failure, so the GitLab provider must mirror that. This test
    // proves a skipped pipeline:
    //   * reports `state = "skipped"` rather than `"failure"`.
    //   * does NOT collect failed jobs.
    //   * does NOT issue a trace request for any job.
    use crate::ci_model::CiInspectRequest;
    let (base, requests, server) = sequence(vec![
        // 1. runs listing; the only candidate pipeline is skipped.
        MockResponse::ok(format!(
            "[{}]",
            pipeline_payload(11, 7, "skipped", "main", "abc123")
        ))
        .with_header("x-next-page", ""),
    ]);
    let provider = provider(base);
    let request = CiInspectRequest {
        sha: "abc123".to_owned(),
        ref_name: Some("main".to_owned()),
        wait: false,
        timeout: 1,
        poll: 1,
    };
    let output = provider.ci_inspect(&request).unwrap();
    assert_eq!(
        output.state, "skipped",
        "skipped pipeline must report a distinct, non-failure state: {output:?}"
    );
    let selected = output.selected_run.expect("selected run");
    assert_eq!(selected.id, 11);
    assert_eq!(selected.status, "skipped");
    assert!(
        output.failed_jobs.is_empty(),
        "skipped pipeline must not surface any failed jobs: {failed:?}",
        failed = output.failed_jobs,
    );
    assert!(
        output.log_excerpts.is_empty(),
        "skipped pipeline must not collect any log excerpts",
    );
    let requests = requests.recv().unwrap();
    assert_eq!(
        requests.len(),
        1,
        "skipped pipeline must not trigger job or trace requests: {requests:?}",
    );
    assert!(
        requests[0].starts_with("GET /api/v4/projects/42/pipelines?"),
        "{}",
        requests[0],
    );
    server.join().unwrap();
}

#[test]
fn ci_inspect_treats_skipped_job_as_non_failure_within_failed_pipeline() {
    // A pipeline can be failed overall but include skipped jobs (for
    // example `allow_failure: true` siblings). The skipped jobs must
    // not be added to `failed_jobs` or have their traces fetched,
    // matching the shared Forgejo inspect logic.
    use crate::ci_model::CiInspectRequest;
    let (base, requests, server) = sequence(vec![
        // 1. runs listing; pipeline is failed.
        MockResponse::ok(format!(
            "[{}]",
            pipeline_payload(11, 7, "failed", "main", "abc123")
        ))
        .with_header("x-next-page", ""),
        // 2. job listing; one failed, one skipped.
        MockResponse::ok(format!(
            "[{},{}]",
            job_payload(102, "test", "failed", Some("failed")),
            job_payload(103, "lint", "skipped", Some("skipped")),
        ))
        .with_header("x-next-page", ""),
        // 3. trace for the failed job only.
        MockResponse::ok("assertion X failed".to_owned()),
    ]);
    let provider = provider(base);
    let request = CiInspectRequest {
        sha: "abc123".to_owned(),
        ref_name: Some("main".to_owned()),
        wait: false,
        timeout: 1,
        poll: 1,
    };
    let output = provider.ci_inspect(&request).unwrap();
    assert_eq!(output.state, "failure");
    assert_eq!(output.failed_jobs.len(), 1);
    assert_eq!(output.failed_jobs[0].id, 102);
    assert_eq!(output.log_excerpts.len(), 1);
    assert_eq!(output.log_excerpts[0].name, "test");
    let requests = requests.recv().unwrap();
    assert_eq!(
        requests.len(),
        3,
        "skipped jobs must not trigger a trace fetch: {requests:?}",
    );
    assert!(requests[1].starts_with("GET /api/v4/projects/42/pipelines/11/jobs?"));
    assert!(requests[2].starts_with("GET /api/v4/projects/42/jobs/102/trace"));
    assert!(
        !requests
            .iter()
            .any(|request| request.contains("/jobs/103/")),
        "skipped job 103 must not have its trace fetched: {requests:?}",
    );
    server.join().unwrap();
}

#[test]
fn dispatcher_repo_provider_arm_routes_gitlab_create_repo_to_real_method() {
    // Phase 3 wires the dispatcher straight through to the GitLab
    // provider implementation. Direct callers that bypass the CLI
    // guards must reach the real method without recursing.
    let (base, requests, server) = sequence(vec![
        // 1. /user to resolve the personal namespace id.
        MockResponse::ok(user_payload(7)),
        // 2. /namespaces?search=acme to resolve the OWNER namespace id.
        MockResponse::ok(
            r#"[{"id":42,"path":"acme","full_path":"acme","kind":"group","name":"Acme"}]"#,
        )
        .with_header("x-next-page", ""),
        // 3. POST /projects with the resolved namespace_id.
        MockResponse::ok(project_payload(99, "widgets", "acme", "private")),
    ]);
    let dispatcher = ProviderDispatcher::Gitlab(provider(base));
    let summary = dispatcher
        .create_repo("acme/widgets", true, "phase3", true)
        .unwrap();
    assert_eq!(summary.full_name, "acme/widgets");
    assert_eq!(requests.recv().unwrap().len(), 3);
    server.join().unwrap();
}

#[test]
fn dispatcher_ci_provider_arm_routes_gitlab_ci_runs_to_real_method() {
    use crate::ci_model::CiRunsFilter;
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(format!(
            "[{}]",
            pipeline_payload(11, 1, "success", "main", "abc")
        ))
        .with_header("x-next-page", ""),
    ]);
    let dispatcher = ProviderDispatcher::Gitlab(provider(base));
    let filter = CiRunsFilter {
        sha: None,
        ref_name: None,
        status: None,
        workflow: None,
        page: 1,
        limit: 50,
    };
    let output = dispatcher.ci_runs(&filter).unwrap();
    assert_eq!(output.workflow_runs.len(), 1);
    assert_eq!(requests.recv().unwrap().len(), 1);
    server.join().unwrap();
}

#[test]
fn gitlab_pipeline_request_includes_private_token_header() {
    use crate::ci_model::CiRunsFilter;
    let (base, requests, server) =
        sequence(vec![MockResponse::ok("[]").with_header("x-next-page", "")]);
    let provider = provider(base);
    let filter = CiRunsFilter {
        sha: None,
        ref_name: None,
        status: None,
        workflow: None,
        page: 1,
        limit: 50,
    };
    let _ = provider.ci_runs(&filter).unwrap();
    let request = &requests.recv().unwrap()[0];
    assert!(
        request
            .to_ascii_lowercase()
            .contains(&format!("private-token: {TEST_TOKEN}")),
        "missing PRIVATE-TOKEN header: {request}",
    );
    assert!(
        !request.to_ascii_lowercase().contains("authorization:"),
        "GitLab request leaked an Authorization header: {request}",
    );
    server.join().unwrap();
}
