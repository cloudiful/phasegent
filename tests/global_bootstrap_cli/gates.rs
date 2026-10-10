use super::fixtures::{no_network_db, project_list, run_cli_in};
use super::support::{MockResponse, make_test_db, run_cli, start_mock_server, stderr_text};

#[test]
fn dry_run_without_all_projects_is_an_argument_error() {
    let db = no_network_db();
    let output = run_cli(
        &db.path,
        "http://127.0.0.1:1",
        Some("admin"),
        &["admin", "workflow", "bootstrap", "--dry-run"],
    );
    assert_eq!(output.status.code(), Some(2), "{}", stderr_text(&output));
}

#[test]
fn all_projects_conflicts_with_global_project_id_before_network() {
    let db = no_network_db();
    let output = run_cli(
        &db.path,
        "http://127.0.0.1:1",
        Some("admin"),
        &[
            "--project-id",
            "5",
            "admin",
            "workflow",
            "bootstrap",
            "--all-projects",
        ],
    );
    assert_eq!(output.status.code(), Some(2), "{}", stderr_text(&output));
}

#[test]
fn all_projects_rejects_repository_before_network() {
    let db = no_network_db();
    let output = run_cli(
        &db.path,
        "http://127.0.0.1:1",
        Some("admin"),
        &[
            "admin",
            "workflow",
            "bootstrap",
            "--all-projects",
            "--repository",
            "owner/repo",
        ],
    );
    assert_eq!(output.status.code(), Some(2), "{}", stderr_text(&output));
}

#[test]
fn all_projects_is_admin_only() {
    let db = no_network_db();
    for role in ["executor", "reviewer"] {
        let output = run_cli(
            &db.path,
            "http://127.0.0.1:1",
            Some(role),
            &["admin", "workflow", "bootstrap", "--all-projects"],
        );
        assert_eq!(
            output.status.code(),
            Some(3),
            "role {role} must hit the permission gate"
        );
    }
}

#[test]
fn non_redmine_provider_is_rejected_before_network() {
    let db = no_network_db();
    let output = run_cli(
        &db.path,
        "http://127.0.0.1:1",
        Some("admin"),
        &[
            "--provider",
            "local",
            "admin",
            "workflow",
            "bootstrap",
            "--all-projects",
        ],
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr_text(&output));
    assert!(stderr_text(&output).contains("not_supported"));
}

#[test]
fn all_projects_runs_from_a_directory_without_git_metadata() {
    let server = start_mock_server(vec![MockResponse::ok(project_list(&[]))]);
    let db = make_test_db(&server.base_url);
    let working_dir = db.path.parent().expect("database temp directory");
    assert!(!working_dir.join(".git").exists());
    let output = run_cli_in(
        working_dir,
        &db.path,
        &server.base_url,
        "admin",
        &["admin", "workflow", "bootstrap", "--all-projects"],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr_text(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ready"], true);
    assert_eq!(report["projects"].as_array().unwrap().len(), 0);
    assert_eq!(server.requests().len(), 1);
}
