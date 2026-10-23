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
            validate_options(args, 0, &["--page", "--limit"], &[], "version list")?;
            Ok(Command::VersionCommand(VersionCommand::List {
                page: Some(page_request(args, "version list")?),
            }))
        }
        value => Err(format!("unknown version command '{value}'")),
    }
}
