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
        // Phase 1 (issue 443) single entry: a parser-level compat alias.
        // With `--to` (or the `--status` spelling) this lands on the
        // exact `Advance` path, so `transition --to` and
        // `advance --status` share the preflight plus PUT behaviour and
        // the orchestrator-only guard. Bare `transition` (automatic
        // routing) and `--note` projection belong to Phase 2, so both
        // fail loudly with guidance instead of silently dropping input.
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
                        return Err(
                            "status transition requires a non-empty --to".to_owned(),
                        );
                    }
                    Ok(Command::Status(StatusCommand::Advance {
                        number,
                        status: target,
                    }))
                }
                (None, None) => Err("status transition requires --to STATUS in Phase 1 (automatic routing arrives in Phase 2); use `status next <N>` to see the allowed targets".to_owned()),
            }
        }
        value => Err(format!("unknown status command '{value}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::super::StatusCommand;
    use super::parse_status;

    fn argv(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn transition_with_to_aliases_the_advance_path() {
        match parse_status(&argv(&["transition", "51", "--to", "In Review"])).unwrap() {
            super::super::Command::Status(StatusCommand::Advance { number, status }) => {
                assert_eq!(number, 51);
                assert_eq!(status, "In Review");
            }
            other => panic!("transition --to must alias Advance, got: {other:?}"),
        }
    }

    #[test]
    fn transition_accepts_status_and_inline_spellings() {
        match parse_status(&argv(&["transition", "51", "--status", "Blocked"])).unwrap() {
            super::super::Command::Status(StatusCommand::Advance { number, status }) => {
                assert_eq!(number, 51);
                assert_eq!(status, "Blocked");
            }
            other => panic!("transition --status must alias Advance, got: {other:?}"),
        }
        match parse_status(&argv(&["transition", "51", "--to=In Review"])).unwrap() {
            super::super::Command::Status(StatusCommand::Advance { status, .. }) => {
                assert_eq!(status, "In Review");
            }
            other => panic!("transition --to=VALUE must alias Advance, got: {other:?}"),
        }
    }

    #[test]
    fn transition_rejects_ambiguous_missing_and_deferred_inputs() {
        let both = parse_status(&argv(&["transition", "51", "--to", "A", "--status", "B"]));
        assert!(
            both.expect_err("both --to/--status must be rejected")
                .contains("only one")
        );
        let bare = parse_status(&argv(&["transition", "51"]));
        let error = bare.expect_err("bare transition must require --to in Phase 1");
        assert!(error.contains("--to"), "got: {error}");
        assert!(error.contains("status next"), "got: {error}");
        let noted = parse_status(&argv(&["transition", "51", "--to", "A", "--note", "hi"]));
        assert!(
            noted
                .expect_err("--note must be reserved")
                .contains("--note")
        );
        assert!(parse_status(&argv(&["transition", "51", "--unknown", "A"])).is_err());
    }
}
