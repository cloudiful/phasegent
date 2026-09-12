use super::prelude::*;

pub(crate) fn parse_hooks(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || matches!(name, Some("--help" | "-h")) {
        return Ok(Command::Help(HelpTopic::Hooks));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::HooksCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "install" => {
            require_exact_positionals(args, 1, "hooks install")?;
            Ok(Command::Hooks(HooksCommand::Install))
        }
        "run" => parse_hooks_run(args),
        value => Err(format!("unknown hooks command '{value}'")),
    }
}

/// Parses the hidden internal `hooks run` forms invoked by generated hook
/// scripts: `hooks run prepare-commit-msg <file> [source]` and
/// `hooks run commit-msg <file>`. Exact argument counts are validated here
/// and again at execution time.
fn parse_hooks_run(args: &[String]) -> Result<Command, String> {
    let hook_name = args.get(1).map(String::as_str).ok_or_else(|| {
        "hooks run requires a hook name: prepare-commit-msg or commit-msg".to_owned()
    })?;
    let hook = crate::hooks::HookKind::parse(hook_name)
        .ok_or_else(|| format!("unknown hooks run target '{hook_name}'"))?;
    let rest = &args[2..];
    if rest.iter().any(|value| value.starts_with('-')) {
        return Err("hooks run takes only positional arguments".to_owned());
    }
    // Exact counts: prepare-commit-msg takes a file plus optional source;
    // commit-msg takes exactly one file.
    let valid = match hook {
        crate::hooks::HookKind::PrepareCommitMsg => matches!(rest.len(), 1 | 2),
        crate::hooks::HookKind::CommitMsg => rest.len() == 1,
    };
    if !valid {
        return Err(match hook {
            crate::hooks::HookKind::PrepareCommitMsg => {
                "usage: phasegent hooks run prepare-commit-msg <message-file> [source]".to_owned()
            }
            crate::hooks::HookKind::CommitMsg => {
                "usage: phasegent hooks run commit-msg <message-file>".to_owned()
            }
        });
    }
    Ok(Command::Hooks(HooksCommand::Run {
        hook,
        message_file: rest[0].clone(),
        source: rest.get(1).cloned(),
    }))
}
