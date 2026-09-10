//! Parser for the `worktree` command group.
//!
//! Phase 2 of issue #239 (worktree-seamless) exposes the Rust core to the
//! CLI. The parser follows the existing `command/<name>.rs` pattern used
//! by `comment`, `timer`, `notify`, and others:
//!
//! * `args.first()` is the subcommand name (`acquire` / `release` /
//!   `status` / `list` / `prune`); a missing or `--help` first token
//!   returns `Command::Help(HelpTopic::Worktree)`, and a trailing
//!   `--help` returns `Command::Help(HelpTopic::WorktreeCommand(name))`.
//! * Each subcommand uses the standard `validate_options` /
//!   `optional_option` / `required_nonempty_option` / `has_flag`
//!   helpers so the leading-dash escape hatch (`--option=VALUE`) works
//!   for values that begin with `-`.
//! * No provider / network is touched here; the role gate lives in
//!   `cli::worktree::execute_worktree`.
//!
//! ## Subcommands
//!
//! * `acquire --issue N [--session S] [--base REF] [--isolate] [--format json]`
//!   Idempotent. Returns `AcquireOutcome` JSON. The optional `--base`
//!   flag is accepted for future Phase 2 follow-up; the current
//!   implementation always bases on `HEAD` (matching the Phase 1
//!   contract). `--isolate` forces a fresh branch/worktree on a
//!   conflict; without it (and with `worktree-auto` off) acquire reuses
//!   the current checkout and warns.
//! * `release --lease ID [--retain=true]`
//!   Default `--retain` is `true`. `--retain=false` flips the lease to
//!   `released`; `--retain=true` (or omitted) flips to `retained`.
//! * `status --issue N`
//!   Lists active leases for the issue. Read-only.
//! * `list [--repo PATH]`
//!   Lists every lease for the resolved repo identity. `--repo` defaults
//!   to the current working directory.
//! * `prune [--stale-days N] [--dry-run]`
//!   Removes clean + expired + retained worktrees. Default
//!   `--stale-days 14`. Branches are never deleted; the only `git`
//!   invocation is `git worktree remove` (no `--force`).

use super::parse_helpers::{has_flag, optional_option, required_nonempty_option, validate_options};
use super::{Command, HelpTopic, WorktreeCommand};

pub(crate) fn parse_worktree(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || matches!(name, Some("--help" | "-h")) {
        return Ok(Command::Help(HelpTopic::Worktree));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::WorktreeCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "acquire" => parse_acquire(args),
        "release" => parse_release(args),
        "status" => parse_status(args),
        "list" => parse_list(args),
        "prune" => parse_prune(args),
        value => Err(format!("unknown worktree command '{value}'")),
    }
}

fn parse_acquire(args: &[String]) -> Result<Command, String> {
    validate_options(
        args,
        0,
        &["--issue", "--session", "--base", "--format"],
        &["--isolate"],
        "worktree acquire",
    )?;
    let issue_raw = required_nonempty_option(args, "--issue", "worktree acquire")?;
    let issue: u64 = issue_raw
        .parse()
        .map_err(|_| "worktree acquire --issue must be a positive integer".to_owned())?;
    if issue == 0 {
        return Err("worktree acquire --issue must be greater than zero".to_owned());
    }
    let session = optional_option(args, "--session")
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(default_session_label);
    let base = optional_option(args, "--base")
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let format = optional_option(args, "--format")
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "json".to_owned());
    if format != "json" {
        return Err("worktree acquire --format must be 'json'".to_owned());
    }
    let isolate = has_flag(args, "--isolate");
    Ok(Command::Worktree(WorktreeCommand::Acquire {
        issue,
        session,
        base,
        format,
        isolate,
    }))
}

fn parse_release(args: &[String]) -> Result<Command, String> {
    validate_options(args, 0, &["--lease", "--retain"], &[], "worktree release")?;
    let lease = required_nonempty_option(args, "--lease", "worktree release")?;
    let retain = match optional_option(args, "--retain") {
        Some(raw) => {
            let normalized = raw.trim().to_ascii_lowercase();
            match normalized.as_str() {
                "true" | "1" | "yes" | "on" => true,
                "false" | "0" | "no" | "off" => false,
                other => {
                    return Err(format!(
                        "worktree release --retain must be true or false, got {other:?}"
                    ));
                }
            }
        }
        None => true,
    };
    Ok(Command::Worktree(WorktreeCommand::Release {
        lease,
        retain,
    }))
}

fn parse_status(args: &[String]) -> Result<Command, String> {
    validate_options(args, 0, &["--issue"], &[], "worktree status")?;
    let issue_raw = required_nonempty_option(args, "--issue", "worktree status")?;
    let issue: u64 = issue_raw
        .parse()
        .map_err(|_| "worktree status --issue must be a positive integer".to_owned())?;
    if issue == 0 {
        return Err("worktree status --issue must be greater than zero".to_owned());
    }
    Ok(Command::Worktree(WorktreeCommand::Status { issue }))
}

fn parse_list(args: &[String]) -> Result<Command, String> {
    validate_options(args, 0, &["--repo"], &[], "worktree list")?;
    let repo = optional_option(args, "--repo")
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    Ok(Command::Worktree(WorktreeCommand::List { repo }))
}

fn parse_prune(args: &[String]) -> Result<Command, String> {
    validate_options(args, 0, &["--stale-days"], &["--dry-run"], "worktree prune")?;
    let stale_days_raw = match optional_option(args, "--stale-days") {
        Some(value) => value
            .parse::<u32>()
            .map_err(|_| "worktree prune --stale-days must be a non-negative integer".to_owned())?,
        None => 14,
    };
    let dry_run = has_flag(args, "--dry-run");
    Ok(Command::Worktree(WorktreeCommand::Prune {
        stale_days: stale_days_raw,
        dry_run,
    }))
}

/// Default session label when `--session` is omitted. `phasegent` is the
/// only consumer of the `acquire` CLI in Phase 2, so the marker keeps
/// the lease history readable in `worktree status` / `worktree list`
/// output without forcing every caller to invent a session id.
fn default_session_label() -> String {
    "phasegent".to_owned()
}

#[cfg(test)]
mod tests {
    use super::super::{Command, WorktreeCommand};
    use crate::command;

    fn strings<const N: usize>(values: [&str; N]) -> Vec<String> {
        values.into_iter().map(str::to_owned).collect()
    }

    #[test]
    fn acquire_parses_minimal_args() {
        let invocation = command::parse(&strings([
            "--role",
            "orchestrator",
            "worktree",
            "acquire",
            "--issue",
            "239",
        ]))
        .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::Acquire {
                issue,
                session,
                base,
                format,
                isolate,
            }) => {
                assert_eq!(issue, 239);
                assert_eq!(session, "phasegent");
                assert_eq!(base, None);
                assert_eq!(format, "json");
                assert!(!isolate, "--isolate must default off");
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn acquire_parses_isolate_flag() {
        let invocation = command::parse(&strings([
            "--role",
            "orchestrator",
            "worktree",
            "acquire",
            "--issue",
            "247",
            "--isolate",
        ]))
        .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::Acquire { isolate, .. }) => {
                assert!(isolate, "--isolate must round-trip to the command");
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn acquire_parses_session_base_and_format() {
        let invocation = command::parse(&strings([
            "--role",
            "orchestrator",
            "worktree",
            "acquire",
            "--issue",
            "239",
            "--session",
            "alpha",
            "--base",
            "main",
            "--format",
            "json",
        ]))
        .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::Acquire {
                issue,
                session,
                base,
                format,
                isolate,
            }) => {
                assert_eq!(issue, 239);
                assert_eq!(session, "alpha");
                assert_eq!(base.as_deref(), Some("main"));
                assert_eq!(format, "json");
                assert!(!isolate);
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn acquire_rejects_non_json_format() {
        let err = command::parse(&strings([
            "--role",
            "orchestrator",
            "worktree",
            "acquire",
            "--issue",
            "1",
            "--format",
            "yaml",
        ]))
        .unwrap_err();
        assert!(err.contains("--format"));
    }

    #[test]
    fn acquire_rejects_zero_issue() {
        let err = command::parse(&strings([
            "--role",
            "orchestrator",
            "worktree",
            "acquire",
            "--issue",
            "0",
        ]))
        .unwrap_err();
        assert!(err.contains("--issue"));
    }

    #[test]
    fn release_defaults_to_retain() {
        let invocation = command::parse(&strings([
            "--role",
            "orchestrator",
            "worktree",
            "release",
            "--lease",
            "lease-1",
        ]))
        .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::Release { lease, retain }) => {
                assert_eq!(lease, "lease-1");
                assert!(retain);
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn release_retain_false_is_recognised() {
        let invocation = command::parse(&strings([
            "--role",
            "orchestrator",
            "worktree",
            "release",
            "--lease",
            "lease-1",
            "--retain",
            "false",
        ]))
        .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::Release { lease, retain }) => {
                assert_eq!(lease, "lease-1");
                assert!(!retain);
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn release_rejects_invalid_retain_value() {
        let err = command::parse(&strings([
            "--role",
            "orchestrator",
            "worktree",
            "release",
            "--lease",
            "lease-1",
            "--retain",
            "maybe",
        ]))
        .unwrap_err();
        assert!(err.contains("--retain"));
    }

    #[test]
    fn status_parses_issue() {
        let invocation = command::parse(&strings([
            "--role",
            "orchestrator",
            "worktree",
            "status",
            "--issue",
            "42",
        ]))
        .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::Status { issue }) => assert_eq!(issue, 42),
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn list_parses_repo_optional() {
        let invocation = command::parse(&strings([
            "--role",
            "executor",
            "worktree",
            "list",
            "--repo",
            "/tmp/repo",
        ]))
        .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::List { repo }) => {
                assert_eq!(repo.as_deref(), Some("/tmp/repo"));
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn prune_defaults_to_14_days_dry_run_off() {
        let invocation =
            command::parse(&strings(["--role", "orchestrator", "worktree", "prune"])).unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::Prune {
                stale_days,
                dry_run,
            }) => {
                assert_eq!(stale_days, 14);
                assert!(!dry_run);
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn prune_parses_stale_days_and_dry_run() {
        let invocation = command::parse(&strings([
            "--role",
            "orchestrator",
            "worktree",
            "prune",
            "--stale-days",
            "30",
            "--dry-run",
        ]))
        .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::Prune {
                stale_days,
                dry_run,
            }) => {
                assert_eq!(stale_days, 30);
                assert!(dry_run);
            }
            other => panic!("unexpected command {other:?}"),
        }
    }
}
