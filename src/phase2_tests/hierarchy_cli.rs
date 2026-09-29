//! Focused CLI surface tests for the native hierarchy commands (issue 641
//! P4a): parser shapes, help topics, and role gates. Moved out of
//! `issue_commands.rs` so each hierarchy file stays under the size budget.
//! Execution exit codes and dispatcher routing live in `hierarchy_lifecycle`.

use super::owned_args;
use super::*;

#[test]
fn hierarchy_commands_parse_shapes_and_reject_bad_input() {
    // `hierarchy get <id>`, `hierarchy set --parent/--child`, and
    // `hierarchy unset --child` parse to the typed command; bad ids and
    // missing flags fail locally with no provider access.
    match command::parse_with_role_env(
        &owned_args(&["hierarchy", "get", "641"]),
        Some("orchestrator"),
    )
    .unwrap()
    .command
    {
        command::Command::Hierarchy(command::HierarchyCommand::Get { id }) => {
            assert_eq!(id, 641);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    match command::parse_with_role_env(
        &owned_args(&["hierarchy", "set", "--parent", "640", "--child", "641"]),
        Some("orchestrator"),
    )
    .unwrap()
    .command
    {
        command::Command::Hierarchy(command::HierarchyCommand::Set { parent, child }) => {
            assert_eq!(parent, 640);
            assert_eq!(child, 641);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // Inline `--option=value` forms parse identically.
    match command::parse_with_role_env(
        &owned_args(&["hierarchy", "set", "--parent=640", "--child=641"]),
        Some("orchestrator"),
    )
    .unwrap()
    .command
    {
        command::Command::Hierarchy(command::HierarchyCommand::Set { parent, child }) => {
            assert_eq!((parent, child), (640, 641));
        }
        other => panic!("unexpected command: {other:?}"),
    }

    match command::parse_with_role_env(
        &owned_args(&["hierarchy", "unset", "--child", "641"]),
        Some("orchestrator"),
    )
    .unwrap()
    .command
    {
        command::Command::Hierarchy(command::HierarchyCommand::Unset { child }) => {
            assert_eq!(child, 641);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // Missing flags, zero ids, non-numeric ids, positionals on set/unset,
    // and unknown subcommands are all usage errors before any role, provider,
    // or network access.
    for argv in [
        vec!["hierarchy", "set", "--child", "641"],
        vec!["hierarchy", "set", "--parent", "640"],
        vec!["hierarchy", "unset"],
        vec!["hierarchy", "set", "--parent", "0", "--child", "641"],
        vec!["hierarchy", "unset", "--child", "0"],
        vec!["hierarchy", "get", "abc"],
        vec![
            "hierarchy",
            "set",
            "--parent",
            "640",
            "--child",
            "641",
            "extra",
        ],
        vec!["hierarchy", "reparent", "641"],
    ] {
        let args = owned_args(&argv);
        assert!(
            command::parse_with_role_env(&args, Some("orchestrator")).is_err(),
            "hierarchy {args:?} must be rejected"
        );
    }

    // Bare `hierarchy` and trailing `--help` resolve to help topics.
    for (argv, expected) in [
        (vec!["hierarchy"], "group"),
        (vec!["hierarchy", "get", "--help"], "get"),
        (vec!["hierarchy", "set", "--help"], "set"),
        (vec!["hierarchy", "unset", "--help"], "unset"),
    ] {
        let args = owned_args(&argv);
        match command::parse_with_role_env(&args, Some("orchestrator"))
            .unwrap()
            .command
        {
            command::Command::Help(command::HelpTopic::Hierarchy) => {
                assert_eq!(expected, "group");
            }
            command::Command::Help(command::HelpTopic::HierarchyCommand(name)) => {
                assert_eq!(name, expected);
            }
            other => panic!("unexpected command for {args:?}: {other:?}"),
        }
    }
}

#[test]
fn hierarchy_role_gates_keep_reads_open_and_writes_orchestrator_only() {
    // Reads ride the issue-read permission: every non-admin role parses.
    for role in ["orchestrator", "executor", "reviewer", "tester"] {
        let args = owned_args(&["hierarchy", "get", "641"]);
        assert!(
            matches!(
                command::parse_with_role_env(&args, Some(role))
                    .unwrap()
                    .command,
                command::Command::Hierarchy(command::HierarchyCommand::Get { id: 641 })
            ),
            "hierarchy get must parse for {role}"
        );
    }
    // Writes ride the issue-update permission: only the orchestrator parses;
    // every other role gets the stable permission denial naming that
    // operation.
    for argv in [
        vec!["hierarchy", "set", "--parent", "640", "--child", "641"],
        vec!["hierarchy", "unset", "--child", "641"],
    ] {
        let args = owned_args(&argv);
        assert!(matches!(
            command::parse_with_role_env(&args, Some("orchestrator"))
                .unwrap()
                .command,
            command::Command::Hierarchy(_)
        ));
        for role in ["executor", "reviewer", "tester", "admin"] {
            match command::parse_outcome_with_role_env(&args, Some(role)).unwrap() {
                command::ParseOutcome::Permission { operation, .. } => {
                    assert_eq!(operation, "issue update", "denial for {role}");
                }
                command::ParseOutcome::Invocation(invocation) => {
                    panic!("hierarchy write must deny {role}, got {invocation:?}")
                }
            }
        }
    }
    // Admin is denied even the read; a missing role keeps the historical
    // role-required error.
    let get = owned_args(&["hierarchy", "get", "641"]);
    match command::parse_outcome_with_role_env(&get, Some("admin")).unwrap() {
        command::ParseOutcome::Permission { operation, .. } => {
            assert_eq!(operation, "issue read");
        }
        command::ParseOutcome::Invocation(invocation) => {
            panic!("hierarchy get must deny admin, got {invocation:?}")
        }
    }
    assert!(command::parse_with_role_env(&get, None).is_err());

    // The registry resolves separate read/write capabilities for the two
    // hierarchy operations, distinct from every relation path.
    let get_path = command::command_registry_path(&command::Command::Hierarchy(
        command::HierarchyCommand::Get { id: 641 },
    ))
    .expect("hierarchy get must resolve a registry path");
    assert_eq!(get_path, vec!["hierarchy", "get"]);
    assert_eq!(
        command::registry_capability(&get_path),
        Some(Capability::IssueRead)
    );
    let set_path = command::command_registry_path(&command::Command::Hierarchy(
        command::HierarchyCommand::Set {
            parent: 640,
            child: 641,
        },
    ))
    .expect("hierarchy set must resolve a registry path");
    assert_eq!(set_path, vec!["hierarchy", "set"]);
    assert_eq!(
        command::registry_capability(&set_path),
        Some(Capability::IssueUpdateBody)
    );
}
