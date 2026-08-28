use crate::ci_command;
use crate::ci_model::{CiInspectRequest, CiRunsFilter, MAX_LOG_BYTES};
use crate::command;
use crate::policy::{Capability, Role};
use crate::providers::forgejo::{ForgejoConfig, ForgejoProvider};
use crate::providers::{CiProvider, ProviderDispatcher};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

struct MockResponse {
    status: u16,
    content_type: &'static str,
    body: String,
}

impl MockResponse {
    fn json(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            content_type: "application/json",
            body: body.into(),
        }
    }

    fn text(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            content_type: "text/plain",
            body: body.into(),
        }
    }
}

#[test]
fn ci_parser_covers_read_commands_and_filters() {
    let args = strings([
        "--role",
        "executor",
        "ci",
        "runs",
        "--sha",
        "abc",
        "--ref",
        "refs/heads/main",
        "--status",
        "completed",
        "--workflow",
        "build.yml",
        "--page",
        "2",
        "--limit",
        "10",
    ]);
    match command::parse(&args).unwrap().command {
        command::Command::Ci(ci_command::CiCommand::Runs {
            sha,
            ref_name,
            status,
            workflow,
            page,
            limit,
        }) => {
            assert_eq!(sha.as_deref(), Some("abc"));
            assert_eq!(ref_name.as_deref(), Some("refs/heads/main"));
            assert_eq!(status.as_deref(), Some("completed"));
            assert_eq!(workflow.as_deref(), Some("build.yml"));
            assert_eq!((page, limit), (2, 10));
        }
        other => panic!("unexpected command: {other:?}"),
    }

    for (args, expected) in [
        (
            vec!["--role", "reviewer", "ci", "run", "get", "7"],
            "run get",
        ),
        (
            vec!["--role", "reviewer", "ci", "run", "jobs", "7"],
            "run jobs",
        ),
        (
            vec![
                "--role", "reviewer", "ci", "job", "logs", "8", "--tail", "3",
            ],
            "job logs",
        ),
        (
            vec![
                "--role",
                "orchestrator",
                "ci",
                "inspect",
                "--sha=abc",
                "--wait",
                "--timeout",
                "0",
                "--poll",
                "0",
            ],
            "inspect",
        ),
    ] {
        let parsed = command::parse(&strings_vec(args)).unwrap();
        assert!(
            matches!(parsed.command, command::Command::Ci(_)),
            "{expected}"
        );
    }
}

#[test]
fn ci_role_policy_is_read_only_for_all_roles() {
    for role in [Role::Orchestrator, Role::Executor, Role::Reviewer] {
        assert!(role.allows(Capability::CiRead));
    }
    assert!(!Role::Admin.allows(Capability::CiRead));
    let error = command::parse(&strings(["--role", "executor", "ci", "cancel", "7"])).unwrap_err();
    assert!(error.contains("unknown ci command"));
}

#[test]
fn ci_runs_maps_response_and_query_filters() {
    let (base, requests, server) = sequence(vec![MockResponse::json(
        r#"{"total_count":2,"workflow_runs":[{"id":11,"run_number":4,"status":"completed","conclusion":"success","head_sha":"abc","ref":"refs/heads/main","workflow_id":9,"html_url":"https://forgejo/run/11","created_at":"created","run_started_at":"started","completed_at":"stopped","head_commit":{"id":"commit-abc"}}]}"#,
    )]);
    let provider = dispatcher(base);
    let output = provider
        .ci_runs(&CiRunsFilter {
            sha: Some("abc".to_owned()),
            ref_name: Some("refs/heads/main".to_owned()),
            status: Some("completed".to_owned()),
            workflow: Some("9".to_owned()),
            page: 2,
            limit: 10,
        })
        .unwrap();
    assert_eq!(output.total_count, Some(2));
    assert_eq!(output.workflow_runs[0].id, 11);
    assert_eq!(
        output.workflow_runs[0].commit_sha.as_deref(),
        Some("commit-abc")
    );
    assert_eq!(output.workflow_runs[0].pretty_ref.as_deref(), Some("main"));
    assert_eq!(
        output.workflow_runs[0].workflow_id.as_ref().unwrap(),
        &serde_json::json!(9)
    );
    let request = requests.recv().unwrap().remove(0);
    assert!(request.starts_with("GET /api/v1/repos/owner/repo/actions/runs?"));
    for query in [
        "page=2",
        "limit=10",
        "head_sha=abc",
        "ref=refs%2Fheads%2Fmain",
        "status=completed",
        "workflow_id=9",
    ] {
        assert!(request.contains(query), "missing {query} in {request}");
    }
    server.join().unwrap();
}

#[test]
fn ci_run_and_jobs_paths_and_optional_job_fields_map() {
    let (base, requests, server) = sequence(vec![
        MockResponse::json(
            r#"{"id":7,"run_number":2,"status":"queued","head_sha":"abc","head_branch":"main","workflow_id":"build.yml"}"#,
        ),
        MockResponse::json(
            r#"{"jobs":[{"id":8,"name":"test","status":"completed","conclusion":"failure","run_id":7,"run_attempt":2,"task_id":99}]}"#,
        ),
    ]);
    let provider = dispatcher(base);
    let run = provider.ci_run_get(7).unwrap();
    assert_eq!(run.pretty_ref.as_deref(), Some("main"));
    assert_eq!(
        run.workflow_id.as_ref().unwrap(),
        &serde_json::json!("build.yml")
    );
    let jobs = provider.ci_run_jobs(7).unwrap();
    assert_eq!(jobs.jobs[0].conclusion.as_deref(), Some("failure"));
    assert_eq!(jobs.jobs[0].attempt, Some(serde_json::json!(2)));
    assert_eq!(jobs.jobs[0].task_id, Some(serde_json::json!(99)));
    let requests = requests.recv().unwrap();
    assert!(requests[0].starts_with("GET /api/v1/repos/owner/repo/actions/runs/7 "));
    assert!(requests[1].starts_with("GET /api/v1/repos/owner/repo/actions/runs/7/jobs "));
    server.join().unwrap();
}

#[test]
fn ci_job_logs_returns_only_bounded_tail_and_bytes() {
    let raw = (0..2_000)
        .map(|index| format!("line-{index:04}"))
        .collect::<Vec<_>>()
        .join("\n");
    let (base, requests, server) = sequence(vec![MockResponse::text(raw)]);
    let provider = dispatcher(base);
    let output = provider.ci_job_logs(8, 3).unwrap();
    assert!(output.truncated);
    assert!(output.bytes <= MAX_LOG_BYTES);
    assert!(output.log.contains("line-1999"));
    assert!(!output.log.contains("line-1996"));
    assert!(
        requests.recv().unwrap()[0]
            .starts_with("GET /api/v1/repos/owner/repo/actions/jobs/8/logs ")
    );
    server.join().unwrap();
}

#[test]
fn ci_inspect_distinguishes_no_run_and_failure_with_bounded_excerpt() {
    let (base, _, server) = sequence(vec![MockResponse::json(
        r#"{"total_count":0,"workflow_runs":[]}"#,
    )]);
    let no_run = dispatcher(base)
        .ci_inspect(&CiInspectRequest {
            sha: "missing".to_owned(),
            ref_name: None,
            wait: false,
            timeout: 0,
            poll: 0,
        })
        .unwrap();
    assert_eq!(no_run.state, "no_run");
    assert!(no_run.selected_run.is_none());
    server.join().unwrap();

    let (base, _, server) = sequence(vec![
        MockResponse::json(
            r#"{"workflow_runs":[{"id":12,"run_number":3,"status":"completed","conclusion":"failure","head_sha":"abc","ref":"refs/heads/main","html_url":"https://forgejo/run/12"}]}"#,
        ),
        MockResponse::json(
            r#"[{"id":21,"name":"test","status":"completed","conclusion":"failure","run_id":12}]"#,
        ),
        MockResponse::text("first\nsecond\nthird\n"),
    ]);
    let failure = dispatcher(base)
        .ci_inspect(&CiInspectRequest {
            sha: "abc".to_owned(),
            ref_name: Some("main".to_owned()),
            wait: false,
            timeout: 0,
            poll: 0,
        })
        .unwrap();
    assert_eq!(failure.state, "failure");
    assert_eq!(failure.selected_run.as_ref().unwrap().id, 12);
    assert_eq!(failure.failed_jobs.len(), 1);
    assert_eq!(failure.log_excerpts[0].log, "first\nsecond\nthird");
    assert_eq!(failure.url.as_deref(), Some("https://forgejo/run/12"));
    server.join().unwrap();
}

#[test]
fn ci_inspect_timeout_does_not_sleep_for_zero_timeout() {
    let (base, _, server) = sequence(vec![MockResponse::json(
        r#"{"workflow_runs":[{"id":12,"run_number":3,"status":"in_progress","head_sha":"abc"}]}"#,
    )]);
    let result = dispatcher(base)
        .ci_inspect(&CiInspectRequest {
            sha: "abc".to_owned(),
            ref_name: None,
            wait: true,
            timeout: 0,
            poll: 0,
        })
        .unwrap();
    assert_eq!(result.state, "timeout");
    assert_eq!(result.poll_count, 1);
    server.join().unwrap();
}

fn strings<const N: usize>(values: [&str; N]) -> Vec<String> {
    values.into_iter().map(str::to_owned).collect()
}

fn strings_vec(values: Vec<&str>) -> Vec<String> {
    values.into_iter().map(str::to_owned).collect()
}

fn dispatcher(base: String) -> ProviderDispatcher {
    ProviderDispatcher::Forgejo(
        ForgejoProvider::new(
            ForgejoConfig::new(base, "owner", "repo"),
            "token".to_owned(),
        )
        .unwrap(),
    )
}

fn sequence(responses: Vec<MockResponse>) -> (String, Receiver<Vec<String>>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        for response in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            requests.push(request);
            write_response(&mut stream, response);
        }
        let _ = sender.send(requests);
    });
    (format!("http://{address}/api/v1"), receiver, server)
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let size = stream.read(&mut chunk).unwrap();
        if size == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..size]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn write_response(stream: &mut TcpStream, response: MockResponse) {
    let status_text = if response.status == 200 {
        "OK"
    } else {
        "Error"
    };
    let headers = format!(
        "HTTP/1.1 {} {status_text}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        response.content_type,
        response.body.len()
    );
    stream.write_all(headers.as_bytes()).unwrap();
    stream.write_all(response.body.as_bytes()).unwrap();
}
