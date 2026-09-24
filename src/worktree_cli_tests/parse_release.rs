use super::support::*;
use super::*;

#[test]
fn parse_release_default_retain_is_true() {
    let invocation = crate::command::parse_with_role_env(
        &strings(["worktree", "release", "--lease", "lease-1"]),
        Some("orchestrator"),
    )
    .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Release {
            lease,
            retain,
            force,
            reason,
        }) => {
            assert_eq!(lease, "lease-1");
            assert!(retain);
            assert!(!force);
            assert_eq!(reason, None);
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn parse_release_retain_false_round_trip() {
    let invocation = crate::command::parse_with_role_env(
        &strings([
            "worktree", "release", "--lease", "lease-1", "--retain", "false",
        ]),
        Some("orchestrator"),
    )
    .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Release {
            lease,
            retain,
            force,
            reason,
        }) => {
            assert_eq!(lease, "lease-1");
            assert!(!retain);
            assert!(!force);
            assert_eq!(reason, None);
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn parse_release_rejects_bad_retain() {
    let err = crate::command::parse_with_role_env(
        &strings([
            "worktree", "release", "--lease", "lease-1", "--retain", "maybe",
        ]),
        Some("orchestrator"),
    )
    .unwrap_err();
    assert!(
        err.contains("--retain"),
        "expected --retain error, got {err}"
    );
}
