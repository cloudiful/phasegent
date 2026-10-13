use super::fixtures::{
    ALL_USERS, membership_list, parse_stdout, project_list, roles_list, run_cli_in_with_role,
    user_list,
};
use super::support::{
    MockResponse, make_test_db, run_cli, start_mock_server, stderr_text, stdout_text,
};

#[test]
fn membership_failure_emits_structured_project_error_and_error_count() {
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
    responses.extend([
        MockResponse::ok(roles_list(&[
            (3, "Maintainer"),
            (4, "Developer"),
            (5, "Reporter"),
        ])),
        MockResponse {
            status: 403,
            body: r#"{"errors":["denied"]}"#.to_owned(),
        },
    ]);
    for _ in 0..3 {
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

    assert_eq!(output.status.code(), Some(1), "{}", stderr_text(&output));
    let report = parse_stdout(&output);
    assert_eq!(report["ready"], false);
    assert_eq!(report["projects"].as_array().unwrap().len(), 1);
    assert_eq!(report["projects"][0]["error"]["kind"], "http");
    assert_eq!(report["projects"][0]["error"]["status"], 403);
    assert_eq!(report["projects"][0]["error"]["message"], "denied");
    assert_eq!(report["projects"][0]["memberships"][0]["status"], "blocked");
    assert_eq!(report["summary"]["added"], 3);
    assert_eq!(report["summary"]["warnings"], 1);
    assert_eq!(report["summary"]["errors"], 1);
    assert!(!stdout_text(&output).contains("phasegent-orchestrator-key"));
    assert!(
        server
            .requests()
            .iter()
            .all(|request| !request.starts_with("POST /projects.json"))
    );
}

/// Operator reaches the same membership-failure surface through the central
/// admin credential with identical structured errors and no credential leak.
#[test]
fn operator_membership_failure_matches_admin_surface() {
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
    responses.extend([
        MockResponse::ok(roles_list(&[
            (3, "Maintainer"),
            (4, "Developer"),
            (5, "Reporter"),
        ])),
        MockResponse {
            status: 403,
            body: r#"{"errors":["denied"]}"#.to_owned(),
        },
    ]);
    for _ in 0..3 {
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
    let working_dir = db.path.parent().expect("database temp directory");
    let output = run_cli_in_with_role(
        working_dir,
        &db.path,
        &server.base_url,
        None,
        None,
        &["admin", "workflow", "bootstrap", "--all-projects"],
    );

    assert_eq!(output.status.code(), Some(1), "{}", stderr_text(&output));
    let report = parse_stdout(&output);
    assert_eq!(report["ready"], false);
    assert_eq!(report["projects"][0]["error"]["kind"], "http");
    assert_eq!(report["projects"][0]["error"]["status"], 403);
    assert_eq!(report["summary"]["errors"], 1);
    assert!(!stdout_text(&output).contains("phasegent-orchestrator-key"));
}
