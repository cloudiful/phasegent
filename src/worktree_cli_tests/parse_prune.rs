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
            stale_days,
            release_stale,
            remove,
            reason,
            dry_run,
            format,
            no_sync: _,
        }) => {
            assert_eq!(repo, None);
            assert_eq!(stale_days, 7);
            assert!(!release_stale);
            assert!(!remove);
            assert_eq!(reason, None);
            assert!(!dry_run, "prune defaults to removal, not preview");
            assert_eq!(format, "text", "default output is concise text");
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
        &strings(["worktree", "prune", "--remove", "--stale-days", "7"]),
        Some("orchestrator"),
    )
    .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Prune {
            stale_days,
            remove,
            dry_run,
            ..
        }) => {
            assert_eq!(stale_days, 7);
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
fn parse_prune_rejects_removed_release_stale_subcommand() {
    let err = crate::command::parse_with_role_env(
        &strings(["worktree", "release-stale"]),
        Some("orchestrator"),
    )
    .unwrap_err();
    assert!(
        err.contains("unknown worktree command 'release-stale'"),
        "unexpected error: {err}"
    );
}

#[test]
fn parse_prune_rejects_negative_stale_days() {
    let err = crate::command::parse_with_role_env(
        &strings(["worktree", "prune", "--stale-days", "-1"]),
        Some("orchestrator"),
    )
    .unwrap_err();
    assert!(
        err.contains("--stale-days"),
        "expected --stale-days error, got {err}"
    );
}

#[test]
fn parse_prune_release_stale_requires_reason_unless_previewing() {
    // A non-preview recovery records the reason, so it must be present.
    let missing_reason = crate::command::parse_with_role_env(
        &strings(["worktree", "prune", "--release-stale"]),
        Some("orchestrator"),
    )
    .unwrap_err();
    assert!(
        missing_reason.contains("--release-stale requires a non-empty --reason"),
        "unexpected error: {missing_reason}"
    );
    // A preview records nothing, so the reason may be omitted.
    let preview = crate::command::parse_with_role_env(
        &strings(["worktree", "prune", "--release-stale", "--dryrun"]),
        Some("orchestrator"),
    )
    .unwrap();
    match preview.command {
        Command::Worktree(WorktreeCommand::Prune {
            release_stale,
            reason,
            dry_run,
            ..
        }) => {
            assert!(release_stale);
            assert_eq!(reason, None);
            assert!(dry_run);
        }
        other => panic!("unexpected command {other:?}"),
    }
    let dangling_reason = crate::command::parse_with_role_env(
        &strings(["worktree", "prune", "--reason", "cleanup"]),
        Some("orchestrator"),
    )
    .unwrap_err();
    assert!(
        dangling_reason.contains("--reason requires --release-stale"),
        "unexpected error: {dangling_reason}"
    );
}

#[test]
fn parse_prune_release_stale_and_remove_round_trip() {
    let invocation = crate::command::parse_with_role_env(
        &strings([
            "worktree",
            "prune",
            "--repo",
            "/tmp/repo",
            "--stale-days",
            "3",
            "--release-stale",
            "--remove",
            "--reason",
            "cleanup",
        ]),
        Some("orchestrator"),
    )
    .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Prune {
            repo,
            stale_days,
            release_stale,
            remove,
            reason,
            ..
        }) => {
            assert_eq!(repo.as_deref(), Some("/tmp/repo"));
            assert_eq!(stale_days, 3);
            assert!(release_stale);
            assert!(remove);
            assert_eq!(reason.as_deref(), Some("cleanup"));
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn parse_prune_rejects_non_numeric_stale_days() {
    let error = crate::command::parse_with_role_env(
        &strings(["worktree", "prune", "--stale-days", "soon"]),
        Some("orchestrator"),
    )
    .unwrap_err();
    assert!(error.contains("--stale-days"), "unexpected error: {error}");
}
