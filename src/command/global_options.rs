/// Global options recognized before the command, paired with a concrete
/// example value used when explaining a misplaced option.
const GLOBAL_OPTIONS: &[(&str, &str)] = &[
    ("--provider", "redmine"),
    ("--api-base", "https://redmine.example.com"),
    ("--repository", "owner/repo"),
    ("--project-id", "23"),
    ("--close-status-id", "5"),
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
    format!(
        "PHASEGENT_ROLE=executor phasegent {flag} {value} issue create --title TITLE --body BODY"
    )
}
