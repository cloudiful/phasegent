use super::prelude::*;

pub(crate) fn parse_status(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || name == Some("--help") || name == Some("-h") {
        return Ok(Command::Help(HelpTopic::Status));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::StatusCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "list" => {
            require_exact_positionals(args, 1, "status list")?;
            Ok(Command::Status(StatusCommand::List))
        }
        "next" => {
            require_exact_positionals(args, 2, "status next")?;
            Ok(Command::Status(StatusCommand::Next {
                number: positional_number(args, 1, "status next")?,
            }))
        }
        "set" => {
            validate_options(args, 1, &["--status"], &[], "status set")?;
            Ok(Command::Status(StatusCommand::Set {
                number: positional_number(args, 1, "status set")?,
                status: required_nonempty_option(args, "--status", "status set")?,
            }))
        }
        "advance" => {
            validate_options(args, 1, &["--status"], &[], "status advance")?;
            Ok(Command::Status(StatusCommand::Advance {
                number: positional_number(args, 1, "status advance")?,
                status: required_nonempty_option(args, "--status", "status advance")?,
            }))
        }
        // Phase 2 (issue 443) automatic routing: a bare `transition`
        // (no `--to`/`--status`, no `--note`) parses to `Advance`
        // with an empty-target sentinel. `cli/status.rs` resolves
        // the sentinel via `status_next` first-allowed and reuses
        // the exact preflight plus PUT path, so the orchestrator-only
        // guard and scope guard stay shared. `--note` projection
        // stays deferred: post the note with `comment create`.
        "transition" => {
            validate_options(
                args,
                1,
                &["--to", "--status", "--note"],
                &[],
                "status transition",
            )?;
            let number = positional_number(args, 1, "status transition")?;
            if optional_option(args, "--note").is_some() {
                return Err("status transition --note is reserved for Phase 2 note projection; post the note with `comment create` and retry without --note".to_owned());
            }
            let to = optional_option(args, "--to");
            let compat = optional_option(args, "--status");
            match (to, compat) {
                (Some(_), Some(_)) => Err(
                    "status transition accepts only one of --to/--status (they are aliases)"
                        .to_owned(),
                ),
                (Some(target), None) | (None, Some(target)) => {
                    if target.trim().is_empty() {
                        return Err("status transition requires a non-empty --to".to_owned());
                    }
                    Ok(Command::Status(StatusCommand::Advance {
                        number,
                        status: target,
                    }))
                }
                (None, None) => Ok(Command::Status(StatusCommand::Advance {
                    number,
                    status: String::new(),
                })),
            }
        }
        value => Err(format!("unknown status command '{value}'")),
    }
}
