use super::prelude::*;

#[derive(Debug)]
pub enum HierarchyCommand {
    Get { id: u64 },
    Set { parent: u64, child: u64 },
    Unset { child: u64 },
}

pub(crate) fn parse_hierarchy(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || name == Some("--help") || name == Some("-h") {
        return Ok(Command::Help(HelpTopic::Hierarchy));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::HierarchyCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "get" => {
            require_exact_positionals(args, 2, "hierarchy get")?;
            Ok(Command::Hierarchy(HierarchyCommand::Get {
                id: positional_number(args, 1, "hierarchy get")?,
            }))
        }
        "set" => {
            validate_options(args, 0, &["--parent", "--child"], &[], "hierarchy set")?;
            Ok(Command::Hierarchy(HierarchyCommand::Set {
                parent: hierarchy_id(args, "--parent", "hierarchy set")?,
                child: hierarchy_id(args, "--child", "hierarchy set")?,
            }))
        }
        "unset" => {
            validate_options(args, 0, &["--child"], &[], "hierarchy unset")?;
            Ok(Command::Hierarchy(HierarchyCommand::Unset {
                child: hierarchy_id(args, "--child", "hierarchy unset")?,
            }))
        }
        value => Err(format!("unknown hierarchy command '{value}'")),
    }
}

/// Parse a required positive work-item id option. Zero is rejected locally
/// so it never reaches a provider read or write.
fn hierarchy_id(args: &[String], option: &str, operation: &str) -> Result<u64, String> {
    let raw = required_option(args, option, operation)?;
    let id = raw
        .parse::<u64>()
        .map_err(|_| format!("{operation} {option} requires a numeric work item id"))?;
    if id == 0 {
        return Err(format!(
            "{operation} {option} requires a work item id greater than zero"
        ));
    }
    Ok(id)
}
