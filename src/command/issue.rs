use super::AssigneeOption;
use super::BranchOption;
use super::prelude::*;

pub(crate) fn parse_issue(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || name == Some("--help") || name == Some("-h") {
        return Ok(Command::Help(HelpTopic::Issue));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::IssueCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "get" => parse_issue_get(args),
        "search" => parse_issue_search(args),
        "create" => {
            validate_create_options(args)?;
            let (body, body_file, keep_body_file) =
                crate::body_file::parse_body_flags(args, "issue create", false)?;
            let (branch, base) = parse_branch_options(args)?;
            Ok(Command::Issue(IssueCommand::Create {
                title: required_option(args, "--title", "issue create")?,
                body,
                body_file,
                keep_body_file,
                tracker: optional_option(args, "--tracker"),
                planning: planning_options(args),
                assignee: parse_assignee(args)?,
                branch,
                base,
                session: parse_session_option(args, "issue create")?,
            }))
        }
        "update" => {
            validate_options(
                args,
                1,
                &[
                    "--body",
                    "--body-file",
                    "--tracker",
                    "--parent-issue",
                    "--fixed-version",
                    "--start-date",
                    "--due-date",
                    "--estimated-hours",
                    "--done-ratio",
                ],
                &["--keep-body-file"],
                "issue update",
            )?;
            let (body, body_file, keep_body_file) =
                crate::body_file::parse_body_flags(args, "issue update", true)?;
            Ok(Command::Issue(IssueCommand::Update {
                number: positional_number(args, 1, "issue update")?,
                body,
                body_file,
                keep_body_file,
                tracker: optional_option(args, "--tracker"),
                planning: planning_options(args),
            }))
        }
        "close" => {
            validate_options(args, 1, &["--worktree-session"], &[], "issue close")?;
            let worktree_session = match optional_option(args, "--worktree-session") {
                Some(raw) => Some(super::worktree::validate_session_id(&raw, "issue close")?),
                None => None,
            };
            Ok(Command::Issue(IssueCommand::Close {
                number: positional_number(args, 1, "issue close")?,
                worktree_session,
            }))
        }
        "upload-attachment" => {
            validate_options(
                args,
                1,
                &["--path", "--description"],
                &[],
                "issue upload-attachment",
            )?;
            let number = positional_number(args, 1, "issue upload-attachment")?;
            if number == 0 {
                return Err("issue upload-attachment requires a positive issue id".to_owned());
            }
            let path = required_nonempty_option(args, "--path", "issue upload-attachment")?;
            let description = optional_option(args, "--description")
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty());
            Ok(Command::Issue(IssueCommand::UploadAttachment {
                number,
                path,
                description,
            }))
        }
        "bind" => {
            validate_options(args, 1, &["--session"], &["--replace"], "issue bind")?;
            let issue_id = positional_number(args, 1, "issue bind")?;
            if issue_id == 0 {
                return Err("issue bind requires a positive issue id".to_owned());
            }
            Ok(Command::Issue(IssueCommand::Bind {
                issue_id,
                replace: has_flag(args, "--replace"),
                session: parse_session_option(args, "issue bind")?,
            }))
        }
        "unbind" => {
            require_exact_positionals(args, 1, "issue unbind")?;
            Ok(Command::Issue(IssueCommand::Unbind))
        }
        // Local branch context status; unrelated to the provider-backed
        // top-level `status list` command.
        "status" => {
            require_exact_positionals(args, 1, "issue status")?;
            Ok(Command::Issue(IssueCommand::StatusBranch))
        }
        value => Err(format!("unknown issue command '{value}'")),
    }
}

/// Validate an explicit `--session` value for `issue create` / `issue bind`
/// with the shared session rules (non-empty, at most 128 chars) so a blank
/// or overlong value fails at parse time (exit 2) instead of silently
/// falling back inside the auto-acquire hook.
fn parse_session_option(args: &[String], operation: &str) -> Result<Option<Box<str>>, String> {
    match optional_option(args, "--session") {
        Some(raw) => Ok(Some(
            super::worktree::validate_session_id(&raw, operation)?.into_boxed_str(),
        )),
        None => Ok(None),
    }
}

/// Parse the GitLab assignee selector for `issue create`. `--assignee`
/// (numeric id or username) and `--no-assign` are mutually exclusive; the
/// raw value is resolved against the provider at execution time.
fn parse_assignee(args: &[String]) -> Result<AssigneeOption, String> {
    let explicit = optional_option(args, "--assignee");
    let no_assign = has_flag(args, "--no-assign");
    match (explicit, no_assign) {
        (Some(_), true) => Err(
            "issue create rejects --assignee together with --no-assign (they are mutually exclusive)"
                .to_owned(),
        ),
        (Some(value), false) => {
            let value = value.trim().to_owned();
            if value.is_empty() {
                return Err("issue create requires a non-empty --assignee".to_owned());
            }
            Ok(AssigneeOption::Explicit(value))
        }
        (None, true) => Ok(AssigneeOption::Unassigned),
        (None, false) => Ok(AssigneeOption::Unset),
    }
}

/// Validate `issue create` options with `--branch` as an optional-value flag.
///
/// Bare `--branch` (no value, or next token is another flag) means
/// auto-generate `<type>/<id>`; `--branch NAME` / `--branch=NAME` uses
/// `NAME`. `--base` always requires a value and requires `--branch`.
fn validate_create_options(args: &[String]) -> Result<(), String> {
    const OPERATION: &str = "issue create";
    const VALUE_OPTIONS: &[&str] = &[
        "--title",
        "--body",
        "--body-file",
        "--tracker",
        "--parent-issue",
        "--fixed-version",
        "--start-date",
        "--due-date",
        "--estimated-hours",
        "--done-ratio",
        "--assignee",
        "--base",
        "--session",
    ];
    const FLAG_OPTIONS: &[&str] = &["--keep-body-file", "--no-assign"];
    let mut positionals = 0;
    let mut index = 1;
    while index < args.len() {
        let value = &args[index];
        if value == "--branch" {
            match args.get(index + 1) {
                None => {
                    index += 1;
                    continue;
                }
                Some(next) if next.starts_with('-') => {
                    // Inline `--branch=NAME` already handled below; a bare
                    // `--branch` followed by another flag is the auto form.
                    // `--branch=-slug` must use the inline form.
                    index += 1;
                    continue;
                }
                Some(_) => {
                    index += 2;
                    continue;
                }
            }
        }
        if value.starts_with('-') {
            if FLAG_OPTIONS.contains(&value.as_str()) {
                index += 1;
                continue;
            }
            if VALUE_OPTIONS
                .iter()
                .any(|option| super::parse_helpers::split_inline(value, option).is_some())
                || super::parse_helpers::split_inline(value, "--branch").is_some()
            {
                index += 1;
                continue;
            }
            if VALUE_OPTIONS.contains(&value.as_str()) {
                match args.get(index + 1) {
                    None => return Err(format!("{value} requires a value")),
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
        if positionals > 0 {
            return Err(format!("{OPERATION} has unexpected arguments"));
        }
        index += 1;
    }
    if positionals != 0 {
        return Err(format!("{OPERATION} has missing arguments"));
    }
    Ok(())
}

/// Parse `--branch [NAME]` / `--branch=NAME` plus `--base REF`.
///
/// Returns `(BranchOption, base)`. `--base` without `--branch` is rejected;
/// empty branch/base values are rejected; surrounding whitespace is trimmed.
fn parse_branch_options(args: &[String]) -> Result<(BranchOption, Option<String>), String> {
    const OPERATION: &str = "issue create";
    let mut branch: BranchOption = BranchOption::Unset;
    let mut branch_seen = false;
    for (index, arg) in args.iter().enumerate() {
        if let Some(inline) = super::parse_helpers::split_inline(arg, "--branch") {
            if branch_seen {
                return Err(format!("{OPERATION} accepts --branch only once"));
            }
            branch_seen = true;
            let name = inline.trim().to_owned();
            if name.is_empty() {
                branch = BranchOption::Auto;
            } else {
                branch = BranchOption::Named(validate_branch_name(&name, OPERATION)?);
            }
        } else if arg == "--branch" {
            if branch_seen {
                return Err(format!("{OPERATION} accepts --branch only once"));
            }
            branch_seen = true;
            match args.get(index + 1) {
                None => branch = BranchOption::Auto,
                Some(next) if next.starts_with('-') => branch = BranchOption::Auto,
                Some(next) => {
                    branch = BranchOption::Named(validate_branch_name(next, OPERATION)?);
                }
            }
        }
    }
    let base = match optional_option(args, "--base") {
        Some(raw) => {
            let value = raw.trim().to_owned();
            if value.is_empty() {
                return Err(format!("{OPERATION} requires a non-empty --base"));
            }
            if value.chars().any(char::is_whitespace) {
                return Err(format!("{OPERATION} --base must not contain whitespace"));
            }
            Some(value)
        }
        None => None,
    };
    if base.is_some() && !branch_seen {
        return Err(format!("{OPERATION} --base requires --branch"));
    }
    Ok((branch, base))
}

/// Validate an explicit `--branch NAME` value with Git ref-safe rules.
fn validate_branch_name(raw: &str, operation: &str) -> Result<String, String> {
    let name = raw.trim().to_owned();
    if name.is_empty() {
        return Err(format!("{operation} requires a non-empty --branch"));
    }
    if name.chars().any(char::is_whitespace) {
        return Err(format!("{operation} --branch must not contain whitespace"));
    }
    if name.contains("..") || name.contains('\0') {
        return Err(format!(
            "{operation} --branch {name:?} is not a valid branch name"
        ));
    }
    if name.starts_with('-') || name.starts_with('/') || name.ends_with('/') {
        return Err(format!(
            "{operation} --branch {name:?} is not a valid branch name"
        ));
    }
    if name.ends_with(".lock") || name.contains("//") {
        return Err(format!(
            "{operation} --branch {name:?} is not a valid branch name"
        ));
    }
    Ok(name)
}

fn parse_issue_search(args: &[String]) -> Result<Command, String> {
    validate_options(
        args,
        0,
        &["--query", "-q", "--state", "--page", "--limit"],
        &["--all", "--include-body"],
        "issue search",
    )?;
    let query = optional_option(args, "--query").or_else(|| optional_option(args, "-q"));
    let state = optional_option(args, "--state").unwrap_or_else(|| "all".to_owned());
    if !matches!(state.as_str(), "open" | "closed" | "all") {
        return Err("--state must be open, closed, or all".to_owned());
    }
    let page = if let Some(value) = optional_option(args, "--page") {
        let parsed: usize = value
            .parse()
            .map_err(|_| "issue search --page must be a positive integer".to_owned())?;
        if parsed == 0 {
            return Err("issue search --page must be >= 1".to_owned());
        }
        parsed
    } else {
        crate::providers::api::ISSUE_SEARCH_DEFAULT_PAGE
    };
    let limit = if let Some(value) = optional_option(args, "--limit") {
        let parsed: usize = value
            .parse()
            .map_err(|_| "issue search --limit must be a positive integer".to_owned())?;
        if parsed == 0 || parsed > crate::providers::api::ISSUE_SEARCH_MAX_LIMIT {
            return Err(format!(
                "issue search --limit must be between 1 and {}",
                crate::providers::api::ISSUE_SEARCH_MAX_LIMIT
            ));
        }
        parsed
    } else {
        crate::providers::api::ISSUE_SEARCH_DEFAULT_LIMIT
    };
    let all = has_flag(args, "--all");
    let include_body = has_flag(args, "--include-body");
    Ok(Command::Issue(IssueCommand::Search {
        query,
        state,
        page,
        limit,
        all,
        include_body,
    }))
}

/// Upper bound for `issue get` batch fetches: one invocation stays a
/// bounded burst of single-issue reads, never an unbounded crawl.
pub(crate) const MAX_BATCH_GET: usize = 20;

/// `issue get <NUMBER> [<NUMBER>...]`. A single number keeps the
/// legacy `Get` shape (and its single-object output); two or more
/// numbers become `GetBatch`. Numbers must be positive, unique, and
/// within the batch cap so one call cannot fan out without limit.
fn parse_issue_get(args: &[String]) -> Result<Command, String> {
    const OPERATION: &str = "issue get";
    if args.len() < 2 {
        return Err(format!("{OPERATION} requires an issue number"));
    }
    let mut numbers = Vec::with_capacity(args.len() - 1);
    for raw in args.iter().skip(1) {
        let number: u64 = raw
            .parse()
            .map_err(|_| format!("{OPERATION} requires a numeric issue number"))?;
        if number == 0 {
            return Err(format!(
                "{OPERATION} requires issue numbers greater than zero"
            ));
        }
        if numbers.contains(&number) {
            return Err(format!(
                "{OPERATION} rejects duplicate issue number {number}"
            ));
        }
        numbers.push(number);
    }
    if numbers.len() > MAX_BATCH_GET {
        return Err(format!(
            "{OPERATION} accepts at most {MAX_BATCH_GET} issue numbers per invocation"
        ));
    }
    if numbers.len() == 1 {
        Ok(Command::Issue(IssueCommand::Get { number: numbers[0] }))
    } else {
        Ok(Command::Issue(IssueCommand::GetBatch { numbers }))
    }
}
