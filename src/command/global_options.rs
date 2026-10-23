/// Global options recognized before the command, paired with a concrete
/// example value used when explaining a misplaced option. The `--api-base`
/// example names a Redmine address because the address override is a
/// per-invocation flag: the persisted Redmine REST address is a
/// machine-wide setting configured through `admin config set`.
const GLOBAL_OPTIONS: &[(&str, &str)] = &[
    ("--provider", "redmine"),
    ("--api-base", "https://redmine.example.com"),
    ("--repository", "owner/repo"),
    ("--project-id", "23"),
    ("--close-status-id", "5"),
    ("--output", "text"),
    ("--no-progress", ""),
];

/// Global options that consume a following value token. The presentation
/// pre-scan skips these values so a value is never read as a flag.
const VALUE_GLOBAL_OPTIONS: &[&str] = &[
    "--provider",
    "--api-base",
    "--repository",
    "--project-id",
    "--close-status-id",
    "--close-status-name",
];

/// Explain option placement when a subcommand parser rejected a token that is
/// a global option, since global options only parse before the command. Every
/// other unknown option keeps its original message.
pub(crate) fn with_global_option_hint(error: String) -> String {
    let Some(token) = error
        .strip_prefix("unknown option '")
        .and_then(|rest| rest.strip_suffix('\''))
    else {
        return error;
    };
    let Some((flag, value)) = matching_global_option(token) else {
        return error;
    };
    format!(
        "{error} (global option '{flag}' must come before the subcommand, e.g. `{}`)",
        global_option_example(flag, value)
    )
}

/// Match a rejected token against the global options, tolerating the inline
/// `--flag=value` form and separator/case variants such as `--project_id`.
fn matching_global_option(token: &str) -> Option<(&'static str, &'static str)> {
    let name = token.split('=').next().unwrap_or(token);
    let normalized = normalize_option_name(name);
    GLOBAL_OPTIONS
        .iter()
        .copied()
        .find(|(flag, _)| normalize_option_name(flag) == normalized)
}

fn normalize_option_name(name: &str) -> String {
    name.trim_start_matches('-')
        .chars()
        .filter(|character| *character != '-' && *character != '_')
        .flat_map(char::to_lowercase)
        .collect()
}

fn global_option_example(flag: &str, value: &str) -> String {
    let invocation = if value.is_empty() {
        format!("phasegent {flag}")
    } else {
        format!("phasegent {flag} {value}")
    };
    format!("PHASEGENT_ROLE=executor {invocation} issue create --title TITLE --body BODY")
}

/// Bounded presentation pre-scan of the leading global-option prefix.
///
/// The scan only walks the options that precede the command, so it never
/// inspects a subcommand's own arguments. It resolves the output preferences
/// for parser and permission failures, which are reported before the command
/// is known. It is not a second authorization parser: it decides presentation
/// only, and the real argv validation stays in `argv`.
pub(crate) fn prescan_presentation(args: &[String]) -> crate::output::context::OutputOptions {
    use crate::output::context::{OutputOptions, OutputSelection};

    let mut options = OutputOptions::default();
    let mut index = 0;
    while index < args.len() {
        let token = args[index].as_str();
        if token == "--no-progress" {
            options.no_progress = true;
            index += 1;
            continue;
        }
        if token == "--output" {
            if let Some(value) = args.get(index + 1) {
                if let Ok(selection) = OutputSelection::parse(value) {
                    options.selection = first_selection(options.selection, selection);
                }
                index += 2;
                continue;
            }
            index += 1;
            continue;
        }
        if let Some(value) = super::parse_helpers::split_inline(token, "--output") {
            if let Ok(selection) = OutputSelection::parse(&value) {
                options.selection = first_selection(options.selection, selection);
            }
            index += 1;
            continue;
        }
        if VALUE_GLOBAL_OPTIONS.contains(&token) {
            index += 2;
            continue;
        }
        if VALUE_GLOBAL_OPTIONS
            .iter()
            .any(|option| super::parse_helpers::split_inline(token, option).is_some())
        {
            index += 1;
            continue;
        }
        // The command (or an unknown token) ends the global-option prefix.
        break;
    }
    options
}

/// The first explicit selection wins for presentation; the real parser
/// reports a contradictory duplicate.
fn first_selection(
    current: crate::output::context::OutputSelection,
    next: crate::output::context::OutputSelection,
) -> crate::output::context::OutputSelection {
    use crate::output::context::OutputSelection;
    match current {
        OutputSelection::Auto => next,
        current => current,
    }
}
