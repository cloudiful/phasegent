use super::*;

#[test]
fn project_list_parses_without_project_id() {
    // Redmine project list must work without --project-id; it is the
    // discovery path for another checkout.
    let args = ["--provider", "redmine", "project", "list"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse_with_role_env(&args, Some("executor"))
        .expect("project list without --project-id must parse");
    match invocation.command {
        Command::Project(ProjectCommand::List) => {}
        other => panic!("expected Project List, got {other:?}"),
    }
    // Also without role? No, project list requires role via top-level parser, but not project-id.
    // With explicit project-id should also parse.
    let args = [
        "--provider",
        "redmine",
        "--project-id",
        "42",
        "project",
        "list",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let invocation = command::parse_with_role_env(&args, Some("executor"))
        .expect("project list with --project-id must parse");
    assert!(matches!(
        invocation.command,
        Command::Project(ProjectCommand::List)
    ));
    assert_eq!(invocation.project_id.as_deref(), Some("42"));

    // Ensure help mentions no project-id needed
    let help_args = ["--help", "project", "list"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let inv = command::parse_with_role_env(&help_args, Some("executor")).expect("help must parse");
    match inv.command {
        Command::Help(crate::command::HelpTopic::ProjectCommand(cmd)) => assert_eq!(cmd, "list"),
        other => panic!("expected help topic for project list, got {other:?}"),
    }
}
