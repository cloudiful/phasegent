use super::parse_workflow;
use crate::command::{Command, WorkflowCommand};

fn bootstrap(values: &[&str]) -> Result<WorkflowCommand, String> {
    let mut full = vec!["bootstrap".to_owned()];
    full.extend(values.iter().map(|value| (*value).to_owned()));
    match parse_workflow(&full) {
        Ok(Command::Workflow(command)) => Ok(command),
        Ok(other) => panic!("expected a workflow command, got {other:?}"),
        Err(error) => Err(error),
    }
}

#[test]
fn default_bootstrap_keeps_repository_mode() {
    let command = bootstrap(&["--repository", "owner/repo"]).unwrap();
    assert!(matches!(
        command,
        WorkflowCommand::Bootstrap {
            all_projects: false,
            dry_run: false,
            ref repository,
            ..
        } if repository.as_deref() == Some("owner/repo")
    ));
}

#[test]
fn all_projects_parses_with_and_without_dry_run() {
    let command = bootstrap(&["--all-projects"]).unwrap();
    assert!(matches!(
        command,
        WorkflowCommand::Bootstrap {
            all_projects: true,
            dry_run: false,
            ref repository,
            ref close_status_id,
            ref close_status_name,
        } if repository.is_none() && close_status_id.is_none() && close_status_name.is_none()
    ));

    let command = bootstrap(&["--all-projects", "--dry-run"]).unwrap();
    assert!(matches!(
        command,
        WorkflowCommand::Bootstrap {
            all_projects: true,
            dry_run: true,
            ..
        }
    ));
}

#[test]
fn dry_run_requires_all_projects() {
    let error = bootstrap(&["--dry-run"]).unwrap_err();
    assert!(
        error.contains("--dry-run requires --all-projects"),
        "{error}"
    );
}

#[test]
fn duplicate_boolean_flags_are_rejected() {
    for values in [
        &["--all-projects", "--all-projects"][..],
        &["--all-projects", "--dry-run", "--dry-run"][..],
    ] {
        let error = bootstrap(values).unwrap_err();
        assert!(error.contains("duplicate"), "unexpected error {error}");
    }
}

#[test]
fn all_projects_rejects_repository_and_status_and_value_forms() {
    for values in [
        &["--all-projects", "--repository", "owner/repo"][..],
        &["--all-projects", "--close-status-id", "5"][..],
        &["--all-projects", "--close-status-name", "Closed"][..],
        &["--all-projects=1"][..],
        &["--dry-run=1"][..],
        &["--all-projects", "--dry-run", "extra"][..],
    ] {
        assert!(
            bootstrap(values).is_err(),
            "expected rejection for {values:?}"
        );
    }
}

#[test]
fn legacy_group_flags_still_rejected() {
    let error = bootstrap(&["--group-name", "AI Agents"]).unwrap_err();
    assert!(error.contains("is no longer supported"), "{error}");
}
