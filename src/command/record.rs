use super::prelude::*;

/// Parse `record create|get|list`.
///
/// The flags are validated syntactically here; the kind/actor binding
/// and the per-kind field rules live in [`crate::record::RecordSpec`] so
/// an impossible combination is rejected identically whether it comes
/// from argv or from a decoded header.
pub(crate) fn parse_record(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || name == Some("--help") || name == Some("-h") {
        return Ok(Command::Help(HelpTopic::Record));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::RecordCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "create" => parse_create(args),
        "get" => {
            require_exact_positionals(args, 3, "record get")?;
            Ok(Command::Record(RecordCommand::Get {
                issue: positional_number(args, 1, "record get")?,
                record: positional_number(args, 2, "record get")?,
            }))
        }
        "list" => {
            validate_options(
                args,
                1,
                &["--kind", "--phase", "--recon"],
                &[],
                "record list",
            )?;
            Ok(Command::Record(RecordCommand::List {
                issue: positional_number(args, 1, "record list")?,
                kind: optional_kind(args, "--kind", "record list")?,
                phase: optional_nonempty_option(args, "--phase", "record list")?,
                recon: optional_nonempty_option(args, "--recon", "record list")?,
            }))
        }
        value => Err(format!("unknown record command '{value}'")),
    }
}

fn parse_create(args: &[String]) -> Result<Command, String> {
    validate_options(
        args,
        1,
        &[
            "--kind",
            "--key",
            "--phase",
            "--attempt",
            "--review",
            "--recon",
            "--body",
            "--body-file",
        ],
        &["--authorized", "--keep-body-file"],
        "record create",
    )?;
    let (body, body_file, keep_body_file) =
        crate::body_file::parse_body_flags(args, "record create", true)?;
    Ok(Command::Record(RecordCommand::Create {
        issue: positional_number(args, 1, "record create")?,
        kind: required_kind(args, "--kind", "record create")?,
        key: required_nonempty_option(args, "--key", "record create")?,
        phase: optional_nonempty_option(args, "--phase", "record create")?,
        attempt: optional_attempt(args)?,
        review: optional_nonempty_option(args, "--review", "record create")?,
        recon: optional_nonempty_option(args, "--recon", "record create")?,
        body,
        body_file,
        keep_body_file,
        authorized: has_flag(args, "--authorized"),
    }))
}

fn required_kind(args: &[String], option: &str, operation: &str) -> Result<RecordKind, String> {
    optional_nonempty_option(args, option, operation)?
        .map(|value| value.parse::<RecordKind>())
        .transpose()?
        .ok_or_else(|| format!("{operation} requires {option}"))
}

/// The attempt is a positive integer; zero never names a real attempt and
/// is rejected at parse time rather than reaching the spec, which would
/// otherwise accept `0` as a valid `u32`.
fn optional_attempt(args: &[String]) -> Result<Option<u32>, String> {
    match optional_nonempty_option(args, "--attempt", "record create")? {
        None => Ok(None),
        Some(raw) => raw
            .parse::<u32>()
            .ok()
            .filter(|attempt| *attempt > 0)
            .map(Some)
            .ok_or_else(|| "record create requires a positive --attempt".to_owned()),
    }
}

fn optional_kind(
    args: &[String],
    option: &str,
    operation: &str,
) -> Result<Option<RecordKind>, String> {
    optional_nonempty_option(args, option, operation)?
        .map(|value| value.parse::<RecordKind>())
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn create_error(values: &[&str]) -> String {
        parse_create(&args(values)).unwrap_err()
    }

    #[test]
    fn parses_a_complete_executor_create() {
        let command = parse_record(&args(&[
            "create",
            "42",
            "--kind",
            "executor",
            "--key",
            "k1",
            "--phase",
            "P1",
            "--attempt",
            "2",
            "--body",
            "note",
        ]))
        .unwrap();
        match command {
            Command::Record(RecordCommand::Create {
                issue,
                kind,
                key,
                phase,
                attempt,
                body,
                authorized,
                ..
            }) => {
                assert_eq!(issue, 42);
                assert_eq!(kind.as_str(), "executor");
                assert_eq!(key, "k1");
                assert_eq!(phase.as_deref(), Some("P1"));
                assert_eq!(attempt, Some(2));
                assert_eq!(body, "note");
                assert!(!authorized);
            }
            other => panic!("expected a record create, got {other:?}"),
        }
    }

    #[test]
    fn rejects_a_missing_key_or_kind() {
        assert!(create_error(&["create", "1", "--body", "n"]).contains("--kind"));
        assert!(
            create_error(&["create", "1", "--kind", "executor", "--body", "n"]).contains("--key")
        );
        assert!(
            create_error(&[
                "create", "1", "--kind", "wizard", "--key", "k", "--body", "n"
            ])
            .contains("invalid record kind")
        );
    }

    #[test]
    fn rejects_a_non_positive_attempt() {
        for bad in ["0", "-1", "two"] {
            assert!(
                create_error(&[
                    "create",
                    "1",
                    "--kind",
                    "executor",
                    "--key",
                    "k",
                    "--phase",
                    "P",
                    "--attempt",
                    bad,
                    "--body",
                    "n"
                ])
                .contains("--attempt"),
                "{bad} must be rejected"
            );
        }
    }

    #[test]
    fn requires_a_body_source() {
        assert!(
            create_error(&["create", "1", "--kind", "executor", "--key", "k"])
                .contains("--body or --body-file")
        );
        assert!(
            create_error(&[
                "create",
                "1",
                "--kind",
                "executor",
                "--key",
                "k",
                "--body",
                "n",
                "--body-file",
                "p"
            ])
            .contains("mutually exclusive")
        );
        assert!(
            create_error(&[
                "create",
                "1",
                "--kind",
                "executor",
                "--key",
                "k",
                "--keep-body-file"
            ])
            .contains("--keep-body-file requires --body-file")
        );
    }

    #[test]
    fn list_accepts_only_its_own_filters() {
        let command =
            parse_record(&args(&["list", "7", "--kind", "recon", "--recon", "scan"])).unwrap();
        match command {
            Command::Record(RecordCommand::List {
                issue,
                kind,
                phase,
                recon,
            }) => {
                assert_eq!(issue, 7);
                assert_eq!(kind.map(|kind| kind.as_str()), Some("recon"));
                assert_eq!(phase, None);
                assert_eq!(recon.as_deref(), Some("scan"));
            }
            other => panic!("expected a record list, got {other:?}"),
        }
        assert!(parse_record(&args(&["list", "7", "--authorized"])).is_err());
    }

    #[test]
    fn get_requires_exactly_two_numbers() {
        assert!(parse_record(&args(&["get", "7", "9"])).is_ok());
        assert!(parse_record(&args(&["get", "7"])).is_err());
        assert!(parse_record(&args(&["get", "7", "nine"])).is_err());
    }

    #[test]
    fn help_and_unknown_subcommands_are_distinct() {
        assert!(matches!(
            parse_record(&args(&["--help"])).unwrap(),
            Command::Help(HelpTopic::Record)
        ));
        assert!(matches!(
            parse_record(&args(&["create", "--help"])).unwrap(),
            Command::Help(HelpTopic::RecordCommand(command)) if command == "create"
        ));
        assert_eq!(
            parse_record(&args(&["frobnicate"])).unwrap_err(),
            "unknown record command 'frobnicate'"
        );
    }
}
