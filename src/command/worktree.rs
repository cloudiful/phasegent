//! Parser for the `worktree` command group.
//!
//! Phase 2 of issue #239 (worktree-seamless) exposes the Rust core to the
//! CLI. The parser follows the existing `command/<name>.rs` pattern used
//! by `comment`, `timer`, `notify`, and others:
//!
//! * `args.first()` is the subcommand name (`acquire` / `release` /
//!   `status` / `list` / `prune` / `heartbeat`); a
//!   missing or `--help` first token returns
//!   `Command::Help(HelpTopic::Worktree)`, and a trailing `--help`
//!   returns `Command::Help(HelpTopic::WorktreeCommand(name))`.
//! * Each subcommand uses the standard `validate_options` /
//!   `optional_option` / `required_nonempty_option` / `has_flag`
//!   helpers so the leading-dash escape hatch (`--option=VALUE`) works
//!   for values that begin with `-`.
//! * No provider / network is touched here; the role gate lives in
//!   `cli::worktree::execute_worktree`.

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
        "probe" => parse_probe(args),
        "prune" => parse_prune(args),
        "heartbeat" => parse_heartbeat(args),
        value => Err(format!("unknown worktree command '{value}'")),
    }
}

fn parse_acquire(args: &[String]) -> Result<Command, String> {
    validate_options(
        args,
        0,
        &["--issue", "--session", "--base", "--format"],
        &["--isolate", "--no-sync"],
        "worktree acquire",
    )?;
    let issue_raw = required_nonempty_option(args, "--issue", "worktree acquire")?;
    let issue: u64 = issue_raw
        .parse()
        .map_err(|_| "worktree acquire --issue must be a positive integer".to_owned())?;
    if issue == 0 {
        return Err("worktree acquire --issue must be greater than zero".to_owned());
    }
    let session = match optional_option(args, "--session") {
        Some(raw) => Some(validate_session_id(&raw, "worktree acquire")?),
        None => None,
    };
    let base = optional_nonempty_option(args, "--base", "worktree acquire")?;
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
        no_sync: has_flag(args, "--no-sync"),
    }))
}

fn parse_release(args: &[String]) -> Result<Command, String> {
    validate_options(
        args,
        0,
        &["--lease", "--retain", "--reason"],
        &["--force"],
        "worktree release",
    )?;
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
    let force = has_flag(args, "--force");
    let reason = optional_option(args, "--reason")
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if force && reason.is_none() {
        return Err(
            "worktree release --force requires a non-empty --reason so the override stays attributable"
                .to_owned(),
        );
    }
    if !force && reason.is_some() {
        return Err(
            "worktree release --reason requires --force; ordinary releases record no reason"
                .to_owned(),
        );
    }
    Ok(Command::Worktree(WorktreeCommand::Release {
        lease,
        retain,
        force,
        reason,
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
    validate_options(args, 0, &["--repo"], &["--no-sync"], "worktree list")?;
    let repo = optional_option(args, "--repo")
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    Ok(Command::Worktree(WorktreeCommand::List {
        repo,
        no_sync: has_flag(args, "--no-sync"),
    }))
}

fn parse_probe(args: &[String]) -> Result<Command, String> {
    validate_options(
        args,
        0,
        &["--path", "--issue", "--session"],
        &[],
        "worktree probe",
    )?;
    let path = optional_nonempty_option(args, "--path", "worktree probe")?;
    let issue = match optional_option(args, "--issue") {
        Some(raw) => {
            let parsed: u64 = raw
                .parse()
                .map_err(|_| "worktree probe --issue must be a positive integer".to_owned())?;
            if parsed == 0 {
                return Err("worktree probe --issue must be greater than zero".to_owned());
            }
            Some(parsed)
        }
        None => None,
    };
    let session = match optional_option(args, "--session") {
        Some(raw) => Some(validate_session_id(&raw, "worktree probe")?),
        None => None,
    };
    if path.is_some() && issue.is_some() {
        return Err("worktree probe --path and --issue are mutually exclusive".to_owned());
    }
    if session.is_some() && issue.is_none() {
        return Err("worktree probe --session requires --issue".to_owned());
    }
    Ok(Command::Worktree(WorktreeCommand::Probe {
        path,
        issue,
        session,
    }))
}

fn parse_prune(args: &[String]) -> Result<Command, String> {
    validate_options(
        args,
        0,
        &["--repo", "--stale-days", "--reason"],
        &["--release-stale", "--remove", "--no-sync"],
        "worktree prune",
    )?;
    let repo = optional_option(args, "--repo")
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let stale_days = match optional_option(args, "--stale-days") {
        Some(value) => value
            .parse::<u32>()
            .map_err(|_| "worktree prune --stale-days must be a non-negative integer".to_owned())?,
        None => 7,
    };
    let release_stale = has_flag(args, "--release-stale");
    let remove = has_flag(args, "--remove");
    let reason = optional_option(args, "--reason")
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if release_stale && reason.is_none() {
        return Err(
            "worktree prune --release-stale requires a non-empty --reason so the recovery stays attributable"
                .to_owned(),
        );
    }
    if !release_stale && reason.is_some() {
        return Err(
            "worktree prune --reason requires --release-stale; a dry-run records no reason"
                .to_owned(),
        );
    }
    Ok(Command::Worktree(WorktreeCommand::Prune {
        repo,
        stale_days,
        release_stale,
        remove,
        reason,
        no_sync: has_flag(args, "--no-sync"),
    }))
}

fn parse_heartbeat(args: &[String]) -> Result<Command, String> {
    validate_options(
        args,
        0,
        &["--lease", "--session"],
        &[],
        "worktree heartbeat",
    )?;
    let lease = required_nonempty_option(args, "--lease", "worktree heartbeat")?;
    let session = match optional_option(args, "--session") {
        Some(raw) => Some(validate_session_id(&raw, "worktree heartbeat")?),
        None => None,
    };
    Ok(Command::Worktree(WorktreeCommand::Heartbeat {
        lease,
        session,
    }))
}

/// Resolve an optional value option while preserving the difference
/// between an omitted option and an explicitly empty one.
///
/// The shared [`optional_option`] helper returns `Some("")` for both
/// `--base=` / `--base ""` and for a whitespace-only value. Trimming
/// that to `None` would make an invalid empty REF (or probe path) behave
/// exactly like an omitted option, silently selecting the default path
/// or the current checkout. This keeps presence observable: an omitted
/// option stays `None`, while a present-but-blank value is a structured
/// parser error.
fn optional_nonempty_option(
    args: &[String],
    option: &str,
    operation: &str,
) -> Result<Option<String>, String> {
    match optional_option(args, option) {
        None => Ok(None),
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                Err(format!("{operation} requires a non-empty {option}"))
            } else {
                Ok(Some(trimmed.to_owned()))
            }
        }
    }
}

/// Validate an explicit session id at parse time so blanks and overlong
/// values fail fast with a parser error (exit 2) instead of reaching the
/// lease table. `resolve_session` also trims the value, which keeps the
/// stored session id canonical.
pub(crate) fn validate_session_id(raw: &str, operation: &str) -> Result<String, String> {
    crate::worktree::resolve_session(Some(raw))
        .map(|context| context.id)
        .map_err(|error| format!("{operation}: {}", error.message))
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
        let invocation = command::parse_with_role_env(
            &strings(["worktree", "acquire", "--issue", "239"]),
            Some("orchestrator"),
        )
        .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::Acquire {
                issue,
                session,
                base,
                format,
                isolate,
                no_sync,
            }) => {
                assert_eq!(issue, 239);
                assert_eq!(session, None);
                assert_eq!(base, None);
                assert_eq!(format, "json");
                assert!(!isolate, "--isolate must default off");
                assert!(!no_sync, "--no-sync must default off");
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn acquire_parses_isolate_flag() {
        let invocation = command::parse_with_role_env(
            &strings(["worktree", "acquire", "--issue", "247", "--isolate"]),
            Some("orchestrator"),
        )
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
        let invocation = command::parse_with_role_env(
            &strings([
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
            ]),
            Some("orchestrator"),
        )
        .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::Acquire {
                issue,
                session,
                base,
                format,
                isolate,
                no_sync,
            }) => {
                assert_eq!(issue, 239);
                assert_eq!(session.as_deref(), Some("alpha"));
                assert_eq!(base.as_deref(), Some("main"));
                assert_eq!(format, "json");
                assert!(!isolate);
                assert!(!no_sync);
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn acquire_rejects_non_json_format() {
        let err = command::parse_with_role_env(
            &strings(["worktree", "acquire", "--issue", "1", "--format", "yaml"]),
            Some("orchestrator"),
        )
        .unwrap_err();
        assert!(err.contains("--format"));
    }

    #[test]
    fn acquire_rejects_zero_issue() {
        let err = command::parse_with_role_env(
            &strings(["worktree", "acquire", "--issue", "0"]),
            Some("orchestrator"),
        )
        .unwrap_err();
        assert!(err.contains("--issue"));
    }

    #[test]
    fn acquire_rejects_blank_session() {
        let err = command::parse_with_role_env(
            &strings(["worktree", "acquire", "--issue", "1", "--session", ""]),
            Some("orchestrator"),
        )
        .unwrap_err();
        assert!(err.contains("session"), "unexpected error: {err}");
    }

    #[test]
    fn acquire_rejects_overlong_session() {
        let overlong = "s".repeat(129);
        let err = command::parse_with_role_env(
            &strings([
                "worktree",
                "acquire",
                "--issue",
                "1",
                "--session",
                &overlong,
            ]),
            Some("orchestrator"),
        )
        .unwrap_err();
        assert!(
            err.contains("session") && err.contains("128"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn release_defaults_to_retain() {
        let invocation = command::parse_with_role_env(
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
    fn release_retain_false_is_recognised() {
        let invocation = command::parse_with_role_env(
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
    fn release_rejects_invalid_retain_value() {
        let err = command::parse_with_role_env(
            &strings([
                "worktree", "release", "--lease", "lease-1", "--retain", "maybe",
            ]),
            Some("orchestrator"),
        )
        .unwrap_err();
        assert!(err.contains("--retain"));
    }

    #[test]
    fn release_force_requires_non_empty_reason() {
        let invocation = command::parse_with_role_env(
            &strings([
                "worktree",
                "release",
                "--lease",
                "lease-1",
                "--force",
                "--reason",
                "stuck session cleanup",
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
                assert!(retain);
                assert!(force);
                assert_eq!(reason.as_deref(), Some("stuck session cleanup"));
            }
            other => panic!("unexpected command {other:?}"),
        }

        let missing = command::parse_with_role_env(
            &strings(["worktree", "release", "--lease", "lease-1", "--force"]),
            Some("orchestrator"),
        )
        .unwrap_err();
        assert!(
            missing.contains("--force requires a non-empty --reason"),
            "unexpected error: {missing}"
        );

        let dangling = command::parse_with_role_env(
            &strings([
                "worktree", "release", "--lease", "lease-1", "--reason", "no force",
            ]),
            Some("orchestrator"),
        )
        .unwrap_err();
        assert!(
            dangling.contains("--reason requires --force"),
            "unexpected error: {dangling}"
        );
    }

    #[test]
    fn status_parses_issue() {
        let invocation = command::parse_with_role_env(
            &strings(["worktree", "status", "--issue", "42"]),
            Some("orchestrator"),
        )
        .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::Status { issue }) => assert_eq!(issue, 42),
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn list_parses_repo_optional() {
        let invocation = command::parse_with_role_env(
            &strings(["worktree", "list", "--repo", "/tmp/repo"]),
            Some("executor"),
        )
        .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::List { repo, .. }) => {
                assert_eq!(repo.as_deref(), Some("/tmp/repo"));
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn prune_defaults_to_7_days_and_read_only_dry_run() {
        let invocation =
            command::parse_with_role_env(&strings(["worktree", "prune"]), Some("orchestrator"))
                .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::Prune {
                repo,
                stale_days,
                release_stale,
                remove,
                reason,
                no_sync,
            }) => {
                assert_eq!(repo, None);
                assert_eq!(stale_days, 7);
                assert!(!release_stale, "--release-stale must default off");
                assert!(!remove, "--remove must default off");
                assert_eq!(reason, None);
                assert!(!no_sync, "--no-sync must default off");
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn prune_parses_remove_and_stale_days() {
        let invocation = command::parse_with_role_env(
            &strings([
                "worktree",
                "prune",
                "--stale-days",
                "30",
                "--remove",
                "--repo",
                "/tmp/repo",
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
                assert_eq!(stale_days, 30);
                assert!(!release_stale);
                assert!(remove);
                assert_eq!(reason, None);
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn prune_combines_release_stale_and_remove() {
        let invocation = command::parse_with_role_env(
            &strings([
                "worktree",
                "prune",
                "--release-stale",
                "--remove",
                "--reason",
                "recover then remove",
            ]),
            Some("orchestrator"),
        )
        .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::Prune {
                release_stale,
                remove,
                reason,
                ..
            }) => {
                assert!(release_stale);
                assert!(remove);
                assert_eq!(reason.as_deref(), Some("recover then remove"));
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    // Issue 552 Phase 2: `acquire` / `list` / `prune` accept `--no-sync`,
    // the opt-out for the pre-subcommand reconciliation pass.

    #[test]
    fn acquire_parses_no_sync_flag() {
        let invocation = command::parse_with_role_env(
            &strings(["worktree", "acquire", "--issue", "552", "--no-sync"]),
            Some("orchestrator"),
        )
        .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::Acquire { no_sync, .. }) => {
                assert!(no_sync, "--no-sync must round-trip to the command");
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn list_parses_no_sync_flag() {
        let invocation = command::parse_with_role_env(
            &strings(["worktree", "list", "--no-sync"]),
            Some("orchestrator"),
        )
        .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::List { repo, no_sync }) => {
                assert_eq!(repo, None);
                assert!(no_sync, "--no-sync must round-trip to the command");
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn prune_parses_no_sync_flag_without_actions() {
        let invocation = command::parse_with_role_env(
            &strings(["worktree", "prune", "--no-sync"]),
            Some("orchestrator"),
        )
        .unwrap();
        match invocation.command {
            Command::Worktree(WorktreeCommand::Prune {
                release_stale,
                remove,
                no_sync,
                ..
            }) => {
                assert!(no_sync);
                assert!(!release_stale, "--no-sync does not imply --release-stale");
                assert!(!remove, "--no-sync does not imply --remove");
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn release_and_status_have_no_sync_switch() {
        // The reconciliation pass only runs for acquire / list / prune, so
        // every other subcommand reports no target and `--no-sync` removes
        // a taxable subcommand from the set entirely.
        for command in [
            WorktreeCommand::Release {
                lease: "lease-1".to_owned(),
                retain: true,
                force: false,
                reason: None,
            },
            WorktreeCommand::Status { issue: 1 },
            WorktreeCommand::Heartbeat {
                lease: "lease-1".to_owned(),
                session: None,
            },
        ] {
            assert!(command.sync_taxi().is_none());
        }
        assert!(
            WorktreeCommand::List {
                repo: None,
                no_sync: true,
            }
            .sync_taxi()
            .is_none(),
            "--no-sync removes the list pass"
        );
        assert_eq!(
            WorktreeCommand::List {
                repo: None,
                no_sync: false,
            }
            .sync_taxi(),
            Some(("worktree list", None)),
            "without --repo the pass targets the current directory"
        );
        assert_eq!(
            WorktreeCommand::Acquire {
                issue: 552,
                session: None,
                base: None,
                format: "json".to_owned(),
                isolate: false,
                no_sync: false,
            }
            .sync_taxi(),
            Some(("worktree acquire", None))
        );
        assert_eq!(
            WorktreeCommand::Prune {
                repo: Some("/tmp/repo".to_owned()),
                stale_days: 7,
                release_stale: false,
                remove: false,
                reason: None,
                no_sync: false,
            }
            .sync_taxi(),
            Some(("worktree prune", Some("/tmp/repo"))),
            "the pass targets the --repo checkout the subcommand operates on"
        );
    }
}
