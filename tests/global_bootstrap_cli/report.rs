use super::fixtures::{
    ALL_USERS, membership_list, parse_stdout, project_list, roles_list, user_list,
};
use super::support::{MockResponse, make_test_db, run_cli, start_mock_server, stderr_text};

#[test]
fn dry_run_prints_complete_report_without_writes() {
    let responses = vec![
        MockResponse::ok(project_list(&[(44, "Workflow", "workflow")])),
        MockResponse::ok(user_list(&ALL_USERS)),
        MockResponse::ok(user_list(&ALL_USERS)),
        MockResponse::ok(user_list(&ALL_USERS)),
        MockResponse::ok(user_list(&ALL_USERS)),
        MockResponse::ok(roles_list(&[
            (3, "Maintainer"),
            (4, "Developer"),
            (5, "Reporter"),
        ])),
        MockResponse::ok(membership_list(None)),
    ];
    let server = start_mock_server(responses);
    let db = make_test_db(&server.base_url);
    let output = run_cli(
        &db.path,
        &server.base_url,
        Some("admin"),
        &[
            "admin",
            "workflow",
            "bootstrap",
            "--all-projects",
            "--dry-run",
        ],
    );

    assert_eq!(output.status.code(), Some(0), "{}", stderr_text(&output));
    let report = parse_stdout(&output);
    assert_eq!(report["mode"], "all-projects");
    assert_eq!(report["dry_run"], true);
    assert_eq!(report["ready"], true);
    assert_eq!(report["roles"].as_array().unwrap().len(), 4);
    assert_eq!(report["projects"][0]["identifier"], "workflow");
    assert_eq!(report["projects"][0]["memberships"][0]["status"], "added");
    assert_eq!(report["summary"]["added"], 4);
    assert!(
        !super::support::stdout_text(&output).contains("phasegent-orchestrator-key"),
        "reports must never contain credentials"
    );
    let requests = server.requests();
    assert!(
        !requests
            .iter()
            .any(|request| request.starts_with("POST") || request.starts_with("PUT")),
        "dry-run must never write: {requests:?}"
    );
    assert!(
        !requests
            .iter()
            .any(|request| request.starts_with("GET /users/")),
        "dry-run must never fetch an individual user's API key: {requests:?}"
    );
}

#[test]
fn apply_reports_provisioned_roles_without_creating_projects_or_mirrors() {
    let mut responses = vec![MockResponse::ok(project_list(&[(
        44, "Workflow", "workflow",
    )]))];
    for (id, login) in ALL_USERS {
        responses.push(MockResponse::ok(user_list(&[])));
        responses.push(MockResponse::ok(
            serde_json::json!({
                "user": {"id": id, "login": login, "firstname": "Phasegent", "lastname": login,
                         "mail": format!("{login}@phasegent.local")}
            })
            .to_string(),
        ));
        responses.push(MockResponse::ok(
            serde_json::json!({
                "user": {"id": id, "login": login, "firstname": "Phasegent", "lastname": login,
                         "mail": format!("{login}@phasegent.local"), "api_key": format!("{login}-key")}
            })
            .to_string(),
        ));
    }
    for _ in 0..4 {
        responses.push(MockResponse::ok(roles_list(&[
            (3, "Maintainer"),
            (4, "Developer"),
            (5, "Reporter"),
        ])));
        responses.push(MockResponse::ok(membership_list(None)));
        responses.push(MockResponse::ok("{}"));
    }
    let server = start_mock_server(responses);
    let db = make_test_db(&server.base_url);
    let output = run_cli(
        &db.path,
        &server.base_url,
        Some("admin"),
        &["admin", "workflow", "bootstrap", "--all-projects"],
    );

    assert_eq!(output.status.code(), Some(0), "{}", stderr_text(&output));
    let report = parse_stdout(&output);
    assert_eq!(report["dry_run"], false);
    assert_eq!(report["ready"], true);
    assert!(
        report["roles"]
            .as_array()
            .unwrap()
            .iter()
            .all(|role| role["status"] == "provisioned")
    );
    assert_eq!(report["summary"]["added"], 4);
    let requests = server.requests();
    assert!(
        !requests
            .iter()
            .any(|request| request.starts_with("POST /projects.json")
                || request.contains("/sys/redmine_git_mirror")),
        "global apply must not create projects or mirrors: {requests:?}"
    );
    assert!(
        !super::support::stdout_text(&output).contains("phasegent-orchestrator-key"),
        "reports must never contain credentials"
    );
}

#[test]
fn partial_failure_prints_the_complete_report_and_exits_nonzero() {
    let responses = vec![
        MockResponse::ok(project_list(&[(44, "Workflow", "workflow")])),
        MockResponse::ok(user_list(&ALL_USERS)),
        MockResponse::ok(user_list(&ALL_USERS)),
        MockResponse::ok(user_list(&ALL_USERS)),
        MockResponse::ok(user_list(&ALL_USERS)),
        MockResponse {
            status: 403,
            body: "{}".to_owned(),
        },
    ];
    let server = start_mock_server(responses);
    let db = make_test_db(&server.base_url);
    let output = run_cli(
        &db.path,
        &server.base_url,
        Some("admin"),
        &[
            "admin",
            "workflow",
            "bootstrap",
            "--all-projects",
            "--dry-run",
        ],
    );

    assert_eq!(output.status.code(), Some(1), "{}", stderr_text(&output));
    let report = parse_stdout(&output);
    assert_eq!(report["ready"], false);
    assert_eq!(report["projects"].as_array().unwrap().len(), 1);
    assert_eq!(report["projects"][0]["error"]["kind"], "http");
    assert_eq!(report["summary"]["errors"], 1);
    assert!(
        report["projects"][0]["memberships"]
            .as_array()
            .unwrap()
            .iter()
            .all(|membership| membership["status"] == "blocked")
    );
}
