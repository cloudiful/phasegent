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
        value => Err(format!("unknown status command '{value}'")),
    }
}
