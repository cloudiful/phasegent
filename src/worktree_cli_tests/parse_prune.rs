use super::support::*;
use super::*;

#[test]
fn parse_prune_defaults_to_removal_and_text_output() {
    let invocation =
        crate::command::parse_with_role_env(&strings(["worktree", "prune"]), Some("orchestrator"))
            .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Prune {
            repo,
            remove,
            dry_run,
            format,
            verbose,
            no_sync,
        }) => {
            assert_eq!(repo, None);
            assert!(!remove);
            assert!(!dry_run, "prune defaults to removal, not preview");
            assert_eq!(format, "text", "default output is concise text");
            assert!(!verbose, "the concise summary is the default");
            assert!(!no_sync);
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn parse_prune_verbose_selects_the_detailed_text() {
    let invocation = crate::command::parse_with_role_env(
        &strings(["worktree", "prune", "--verbose"]),
        Some("orchestrator"),
    )
    .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Prune {
            verbose, format, ..
        }) => {
            assert!(verbose, "--verbose selects the detailed text");
            assert_eq!(
                format, "text",
                "--verbose never selects the machine document"
            );
        }
        other => panic!("unexpected command {other:?}"),
    }
    let machine = crate::command::parse_with_role_env(
        &strings(["worktree", "prune", "--verbose", "--json"]),
        Some("orchestrator"),
    )
    .unwrap();
    match machine.command {
        Command::Worktree(WorktreeCommand::Prune {
            verbose, format, ..
        }) => {
            assert!(verbose);
            assert_eq!(format, "json");
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn parse_prune_dryrun_and_alias_are_recognised() {
    for flag in ["--dryrun", "--dry-run"] {
        let invocation = crate::command::parse_with_role_env(
            &strings(["worktree", "prune", flag]),
            Some("orchestrator"),
        )
        .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::Prune { dry_run, .. }) => {
                assert!(dry_run, "{flag} must request a preview");
            }
            other => panic!("unexpected command {other:?}"),
        }
    }
}

#[test]
fn parse_prune_remove_is_an_accepted_compatibility_noop() {
    let invocation = crate::command::parse_with_role_env(
        &strings(["worktree", "prune", "--remove"]),
        Some("orchestrator"),
    )
    .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Prune {
            remove, dry_run, ..
        }) => {
            assert!(remove, "--remove is still parseable");
            assert!(!dry_run, "--remove does not imply a preview");
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn parse_prune_json_selects_machine_output() {
    for args in [
        strings(["worktree", "prune", "--json"]),
        strings(["worktree", "prune", "--format", "json"]),
    ] {
        let invocation = crate::command::parse_with_role_env(&args, Some("orchestrator")).unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::Prune { format, .. }) => {
                assert_eq!(format, "json");
            }
            other => panic!("unexpected command {other:?}"),
        }
    }
    let explicit_text = crate::command::parse_with_role_env(
        &strings(["worktree", "prune", "--format", "text"]),
        Some("orchestrator"),
    )
    .unwrap();
    match explicit_text.command {
        Command::Worktree(WorktreeCommand::Prune { format, .. }) => assert_eq!(format, "text"),
        other => panic!("unexpected command {other:?}"),
    }
    let bad = crate::command::parse_with_role_env(
        &strings(["worktree", "prune", "--format", "yaml"]),
        Some("orchestrator"),
    )
    .unwrap_err();
    assert!(bad.contains("--format"), "unexpected error: {bad}");
    let contradictory = crate::command::parse_with_role_env(
        &strings(["worktree", "prune", "--json", "--format", "text"]),
        Some("orchestrator"),
    )
    .unwrap_err();
    assert!(
        contradictory.contains("mutually exclusive"),
        "unexpected error: {contradictory}"
    );
}

#[test]
fn parse_prune_rejects_the_removed_lease_flags_and_subcommand() {
    for args in [
        strings(["worktree", "prune", "--release-stale"]),
        strings(["worktree", "prune", "--stale-days", "7"]),
        strings(["worktree", "prune", "--reason", "cleanup"]),
    ] {
        let err = crate::command::parse_with_role_env(&args, Some("orchestrator")).unwrap_err();
        assert!(
            err.contains("unknown option"),
            "a removed lease flag must be rejected, got {err}"
        );
    }
    for command in ["release", "heartbeat", "release-stale"] {
        let err = crate::command::parse_with_role_env(
            &strings(["worktree", command]),
            Some("orchestrator"),
        )
        .unwrap_err();
        assert!(
            err.contains("unknown worktree command"),
            "{command} must be removed, got {err}"
        );
    }
}
