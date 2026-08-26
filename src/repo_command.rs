use crate::command::{self, Command, HelpTopic};
use crate::remote;

#[derive(Debug)]
pub enum RepoCommand {
    Create {
        target: String,
        private: bool,
        description: String,
        auto_init: bool,
    },
}

pub fn parse(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || name == Some("--help") || name == Some("-h") {
        return Ok(Command::Help(HelpTopic::Repo));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::RepoCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "create" => {
            command::validate_options(
                args,
                1,
                &["--description"],
                &["--private", "--auto-init"],
                "repo create",
            )?;
            if !command::has_flag(args, "--private") {
                return Err("repo create requires --private".to_owned());
            }
            Ok(Command::Repo(RepoCommand::Create {
                target: positional_repository(args, 1, "repo create")?,
                private: true,
                description: command::optional_option(args, "--description").unwrap_or_default(),
                auto_init: command::has_flag(args, "--auto-init"),
            }))
        }
        value => Err(format!("unknown repo command '{value}'")),
    }
}

fn positional_repository(args: &[String], index: usize, operation: &str) -> Result<String, String> {
    let value = args
        .get(index)
        .ok_or_else(|| format!("{operation} requires OWNER/REPO"))?;
    remote::validate_repository_create_target(value)
        .map_err(|error| format!("{operation}: {error}"))
}
