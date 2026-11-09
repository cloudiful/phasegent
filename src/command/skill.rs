//! Parser for the `skill` command group (issue #22 P2).
//!
//! `phasegent skill install` installs the single bundled `phasegent` skill
//! directory into the selected skills root. There is no `skill status` or
//! `skill uninstall`: filesystem skill discovery owns presence, and a user's
//! skill directory is never removed automatically.
//!
//! * Default scope is the agent home skills root (`~/.agents/skills`).
//! * `--project` selects `.agents/skills` in the current working directory.
//! * `--path DIR` selects an explicit skills root that will contain
//!   `phasegent/`, and cannot be combined with `--project`.
//! * `--global` was removed and is rejected as an unknown option.
//! * `--force` preserves a foreign `phasegent` directory or file before the
//!   managed skill is written. Without it, foreign content is refused.

use super::parse_helpers::{has_flag, optional_nonempty_option, validate_options};
use super::{Command as RootCommand, HelpTopic, SkillCommand};

pub(crate) fn parse_skill(args: &[String]) -> Result<RootCommand, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || matches!(name, Some("--help" | "-h")) {
        return Ok(RootCommand::Help(HelpTopic::Skill));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(RootCommand::Help(HelpTopic::SkillCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "install" => parse_install(args),
        value => Err(format!("unknown skill command '{value}'")),
    }
}

fn parse_install(args: &[String]) -> Result<RootCommand, String> {
    validate_options(
        args,
        0,
        &["--path"],
        &["--project", "--force"],
        "skill install",
    )?;
    let project = has_flag(args, "--project");
    let force = has_flag(args, "--force");
    let path = optional_nonempty_option(args, "--path", "skill install")?;
    if path.is_some() && project {
        return Err("skill install --path cannot be combined with --project".to_owned());
    }
    Ok(RootCommand::Skill(SkillCommand::Install {
        project,
        force,
        path,
    }))
}
