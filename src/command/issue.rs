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
            validate_options(
                args,
                0,
                &[
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
                ],
                &["--keep-body-file"],
                "issue create",
            )?;
            let (body, body_file, keep_body_file) =
                crate::body_file::parse_body_flags(args, "issue create", false)?;
            Ok(Command::Issue(IssueCommand::Create {
                title: required_option(args, "--title", "issue create")?,
                body,
                body_file,
                keep_body_file,
                tracker: optional_option(args, "--tracker"),
                planning: planning_options(args),
            }))
        }
        "update-body" => {
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
                "issue update-body",
            )?;
            let (body, body_file, keep_body_file) =
                crate::body_file::parse_body_flags(args, "issue update-body", true)?;
            Ok(Command::Issue(IssueCommand::UpdateBody {
                number: positional_number(args, 1, "issue update-body")?,
                body,
                body_file,
                keep_body_file,
                tracker: optional_option(args, "--tracker"),
                planning: planning_options(args),
            }))
        }
        "close" => {
            require_exact_positionals(args, 2, "issue close")?;
            Ok(Command::Issue(IssueCommand::Close {
                number: positional_number(args, 1, "issue close")?,
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
            validate_options(args, 1, &[], &["--replace"], "issue bind")?;
            let issue_id = positional_number(args, 1, "issue bind")?;
            if issue_id == 0 {
                return Err("issue bind requires a positive issue id".to_owned());
            }
            Ok(Command::Issue(IssueCommand::Bind {
                issue_id,
                replace: has_flag(args, "--replace"),
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
