use super::prelude::*;

pub(crate) fn parse_project(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || name == Some("--help") || name == Some("-h") {
        return Ok(Command::Help(HelpTopic::Project));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::ProjectCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "list" => {
            require_exact_positionals(args, 1, "project list")?;
            Ok(Command::Project(ProjectCommand::List))
        }
        "create" => {
            validate_options(
                args,
                0,
                &["--name", "--identifier", "--description"],
                &["--confirm"],
                "project create",
            )?;
            if !has_flag(args, "--confirm") {
                return Err("project create requires --confirm".to_owned());
            }
            Ok(Command::Project(ProjectCommand::Create {
                name: required_nonempty_option(args, "--name", "project create")?,
                identifier: required_nonempty_option(args, "--identifier", "project create")?,
                description: optional_option(args, "--description"),
                confirmed: has_flag(args, "--confirm"),
            }))
        }
        value => Err(format!("unknown project command '{value}'")),
    }
}
