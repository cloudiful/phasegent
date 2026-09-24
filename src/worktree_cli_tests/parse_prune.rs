use super::support::*;
use super::*;

#[test]
fn parse_prune_defaults_to_read_only_and_stale_days_7() {
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
            no_sync: _,
        }) => {
            assert_eq!(repo, None);
            assert_eq!(stale_days, 7);
            assert!(!release_stale);
            assert!(!remove);
            assert_eq!(reason, None);
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn parse_prune_remove_flag_round_trip() {
    let invocation = crate::command::parse_with_role_env(
        &strings(["worktree", "prune", "--remove", "--stale-days", "7"]),
        Some("orchestrator"),
    )
    .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Prune {
            stale_days, remove, ..
        }) => {
            assert_eq!(stale_days, 7);
            assert!(remove);
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn parse_prune_rejects_removed_dry_run_flag() {
    let err = crate::command::parse_with_role_env(
        &strings(["worktree", "prune", "--dry-run"]),
        Some("orchestrator"),
    )
    .unwrap_err();
    assert!(err.contains("--dry-run"), "unexpected error: {err}");
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
fn parse_prune_defaults_to_read_only_scan_and_7_days() {
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
            no_sync: _,
        }) => {
            assert_eq!(repo, None);
            assert_eq!(stale_days, 7);
            assert!(!release_stale);
            assert!(!remove);
            assert_eq!(reason, None);
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn parse_prune_release_stale_requires_reason_and_reason_requires_release_stale() {
    // `--release-stale` is only accepted together with a non-empty `--reason`,
    // and a bare `--reason` is rejected because it would record a dry-run
    // reason; the combined invocation stays parseable.
    let missing_reason = crate::command::parse_with_role_env(
        &strings(["worktree", "prune", "--release-stale"]),
        Some("orchestrator"),
    )
    .unwrap_err();
    assert!(
        missing_reason.contains("--release-stale requires a non-empty --reason"),
        "unexpected error: {missing_reason}"
    );
    let dangling_reason = crate::command::parse_with_role_env(
        &strings(["worktree", "prune", "--reason", "cleanup"]),
        Some("orchestrator"),
    )
    .unwrap_err();
    assert!(
        dangling_reason.contains("--reason requires --release-stale"),
        "unexpected error: {dangling_reason}"
    );
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
            no_sync: _,
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
