use super::PlanningOptions;

pub(crate) fn required_value(
    args: &[String],
    index: usize,
    option: &str,
) -> Result<String, String> {
    // Accept the inline `--option=value` form first so values starting with `-`
    // (Markdown bullets, separator lines, etc.) are not rejected. The two-arg
    // form keeps its strict missing-value detection.
    if let Some(value) = split_inline(&args[index], option) {
        return Ok(value);
    }
    match args.get(index + 1) {
        None => Err(format!("{option} requires a value")),
        Some(value) if value.starts_with('-') => Err(format!(
            "{option} requires a value (use {option}=VALUE when VALUE starts with `-`)"
        )),
        Some(value) => Ok(value.clone()),
    }
}

pub(crate) fn required_option(
    args: &[String],
    option: &str,
    operation: &str,
) -> Result<String, String> {
    if let Some(value) = optional_option(args, option) {
        return Ok(value);
    }
    // When the option appears but its next token starts with `-`, the parser
    // treats it as missing to keep ambiguous two-arg detection strict. Surface
    // the inline `--option=value` escape hatch so the leading-dash case is
    // discoverable from the error message.
    if args
        .windows(2)
        .any(|values| values[0] == option && values[1].starts_with('-'))
    {
        return Err(format!(
            "{operation} requires a non-empty {option} (use {option}=VALUE when VALUE starts with `-`)"
        ));
    }
    Err(format!("{operation} requires {option}"))
}

pub(crate) fn required_nonempty_option(
    args: &[String],
    option: &str,
    operation: &str,
) -> Result<String, String> {
    let value = required_option(args, option, operation)?;
    if value.trim().is_empty() {
        return Err(format!("{operation} requires a non-empty {option}"));
    }
    Ok(value)
}

pub(crate) fn optional_option(args: &[String], option: &str) -> Option<String> {
    // Inline `--option=value` form bypasses the leading-dash guard so
    // legitimate values like `- Goal` or `---` are not lost.
    if let Some(value) = args.iter().find_map(|arg| split_inline(arg, option)) {
        return Some(value);
    }
    args.windows(2)
        .find(|values| values[0] == option)
        .and_then(|values| (!values[1].starts_with('-')).then(|| values[1].clone()))
}

/// If `arg` has the form `--option=value`, return the value with `option` matching
/// the full long-name prefix (e.g. `--body` does not match `--bodyline`). Used so
/// that recognized value-bearing options can carry values that legitimately begin
/// with `-` without breaking the existing strict missing-value behavior.
pub(crate) fn split_inline(arg: &str, option: &str) -> Option<String> {
    if arg.len() > option.len()
        && arg.starts_with(option)
        && arg.as_bytes().get(option.len()).copied() == Some(b'=')
    {
        Some(arg[option.len() + 1..].to_owned())
    } else {
        None
    }
}

pub(crate) fn positional_number(
    args: &[String],
    index: usize,
    operation: &str,
) -> Result<u64, String> {
    args.get(index)
        .ok_or_else(|| format!("{operation} requires an issue number"))?
        .parse()
        .map_err(|_| format!("{operation} requires a numeric issue number"))
}

pub(crate) fn require_exact_positionals(
    args: &[String],
    expected: usize,
    operation: &str,
) -> Result<(), String> {
    if args.len() != expected {
        return Err(format!("{operation} has unexpected arguments"));
    }
    Ok(())
}

pub(crate) fn validate_options(
    args: &[String],
    expected_positionals: usize,
    value_options: &[&str],
    flag_options: &[&str],
    operation: &str,
) -> Result<(), String> {
    let mut positionals = 0;
    let mut index = 1;
    while index < args.len() {
        let value = &args[index];
        if value.starts_with('-') {
            if flag_options.contains(&value.as_str()) {
                index += 1;
                continue;
            }
            // Inline `--option=value` form is recognized so leading-dash values are accepted.
            // The actual value is later extracted by `optional_option`/`required_option`;
            // here we only need to confirm the option is recognized and advance by one token.
            if value_options
                .iter()
                .any(|option| split_inline(value, option).is_some())
            {
                index += 1;
                continue;
            }
            if value_options.contains(&value.as_str()) {
                match args.get(index + 1) {
                    None => {
                        return Err(format!("{value} requires a value"));
                    }
                    Some(next) if next.starts_with('-') => {
                        return Err(format!(
                            "{value} requires a value (use {value}=VALUE when VALUE starts with `-`)"
                        ));
                    }
                    Some(_) => {}
                }
                index += 2;
                continue;
            }
            return Err(format!("unknown option '{value}'"));
        }
        positionals += 1;
        if positionals > expected_positionals {
            return Err(format!("{operation} has unexpected arguments"));
        }
        index += 1;
    }
    if positionals != expected_positionals {
        return Err(format!("{operation} has missing arguments"));
    }
    Ok(())
}

pub(crate) fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|value| value == flag)
}

/// Extract every native planning flag from an issue create/update-body
/// invocation. Values stay raw; semantic validation happens at execution
/// time so error messages can reference the exact operation context.
pub(crate) fn planning_options(args: &[String]) -> PlanningOptions {
    PlanningOptions {
        parent_issue: optional_option(args, "--parent-issue"),
        fixed_version: optional_option(args, "--fixed-version"),
        start_date: optional_option(args, "--start-date"),
        due_date: optional_option(args, "--due-date"),
        estimated_hours: optional_option(args, "--estimated-hours"),
        done_ratio: optional_option(args, "--done-ratio"),
    }
}
