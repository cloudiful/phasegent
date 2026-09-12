use super::prelude::*;

pub(crate) fn parse_version(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || name == Some("--help") || name == Some("-h") {
        return Ok(Command::Help(HelpTopic::Version));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::VersionCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "list" => {
            require_exact_positionals(args, 1, "version list")?;
            Ok(Command::VersionCommand(VersionCommand::List))
        }
        value => Err(format!("unknown version command '{value}'")),
    }
}
