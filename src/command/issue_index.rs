use super::parse_helpers::{
    has_flag, optional_option, required_nonempty_option, validate_options,
};
use super::{Command, HelpTopic, IssueCommand};

pub(crate) fn parse_issue_index(args: &[String]) -> Result<Command, String> {
    // args[0] is "index"
    if args.len() == 1 {
        return Ok(Command::Help(HelpTopic::IssueCommand("index".to_owned())));
    }
    if args.len() >= 2 && (args[1] == "--help" || args[1] == "-h") {
        // `issue index --help` => help for index
        return Ok(Command::Help(HelpTopic::IssueCommand("index".to_owned())));
    }
    let sub = args[1].as_str();
    if args.len() >= 3 && (args[2] == "--help" || args[2] == "-h") {
        return Ok(Command::Help(HelpTopic::IssueCommand(format!(
            "index {sub}"
        ))));
    }
    match sub {
        "sync" => parse_index_sync(&args[1..]),
        "search" => parse_index_search(&args[1..]),
        value => Err(format!("unknown issue index command '{value}'")),
    }
}

fn parse_index_sync(args: &[String]) -> Result<Command, String> {
    // args[0] is "sync"
    validate_options(
        args,
        0,
        &["--query", "-q", "--state", "--page", "--limit"],
        &["--all"],
        "issue index sync",
    )?;
    let query = optional_option(args, "--query").or_else(|| optional_option(args, "-q"));
    let state = optional_option(args, "--state").unwrap_or_else(|| "all".to_owned());
    if !matches!(state.as_str(), "open" | "closed" | "all") {
        return Err("--state must be open, closed, or all".to_owned());
    }
    let page = if let Some(value) = optional_option(args, "--page") {
        let parsed: usize = value
            .parse()
            .map_err(|_| "issue index sync --page must be a positive integer".to_owned())?;
        if parsed == 0 {
            return Err("issue index sync --page must be >= 1".to_owned());
        }
        parsed
    } else {
        crate::providers::api::ISSUE_SEARCH_DEFAULT_PAGE
    };
    let limit = if let Some(value) = optional_option(args, "--limit") {
        let parsed: usize = value
            .parse()
            .map_err(|_| "issue index sync --limit must be a positive integer".to_owned())?;
        if parsed == 0 || parsed > crate::providers::api::ISSUE_SEARCH_MAX_LIMIT {
            return Err(format!(
                "issue index sync --limit must be between 1 and {}",
                crate::providers::api::ISSUE_SEARCH_MAX_LIMIT
            ));
        }
        parsed
    } else {
        crate::providers::api::ISSUE_SEARCH_DEFAULT_LIMIT
    };
    let all = has_flag(args, "--all");
    // Validation mirrors IssueSearchOptions::validate but without has_query check here;
    // execution will validate via options.validate() and enforce tombstone scope logic.
    Ok(Command::Issue(IssueCommand::IndexSync {
        query,
        state,
        page,
        limit,
        all,
    }))
}

fn parse_index_search(args: &[String]) -> Result<Command, String> {
    // args[0] is "search"
    validate_options(
        args,
        0,
        &["--query", "-q", "--limit", "--offset"],
        &["--include-body"],
        "issue index search",
    )?;
    let query = optional_option(args, "--query")
        .or_else(|| optional_option(args, "-q"))
        .ok_or_else(|| "issue index search requires --query TEXT".to_owned())?;
    if query.trim().is_empty() {
        return Err("issue index search requires --query TEXT (empty queries are rejected)".to_owned());
    }
    let limit = if let Some(value) = optional_option(args, "--limit") {
        let parsed: usize = value
            .parse()
            .map_err(|_| "issue index search --limit must be a positive integer".to_owned())?;
        if parsed == 0 || parsed > crate::providers::index::ISSUE_INDEX_SEARCH_MAX_LIMIT {
            return Err(format!(
                "issue index search --limit must be between 1 and {}",
                crate::providers::index::ISSUE_INDEX_SEARCH_MAX_LIMIT
            ));
        }
        parsed
    } else {
        crate::providers::index::ISSUE_INDEX_SEARCH_DEFAULT_LIMIT
    };
    let offset = if let Some(value) = optional_option(args, "--offset") {
        value
            .parse::<usize>()
            .map_err(|_| "issue index search --offset must be a non-negative integer".to_owned())?
    } else {
        crate::providers::index::ISSUE_INDEX_SEARCH_DEFAULT_OFFSET
    };
    let include_body = has_flag(args, "--include-body");
    Ok(Command::Issue(IssueCommand::IndexSearch {
        query,
        limit,
        offset,
        include_body,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{Command, HelpTopic};

    #[test]
    fn index_sync_parser_defaults_and_all_flag() {
        let args = ["index", "sync"]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        match parse_issue_index(&args).unwrap() {
            Command::Issue(crate::command::IssueCommand::IndexSync {
                query,
                state,
                page,
                limit,
                all,
            }) => {
                assert!(query.is_none());
                assert_eq!(state, "all");
                assert_eq!(page, crate::providers::api::ISSUE_SEARCH_DEFAULT_PAGE);
                assert_eq!(limit, crate::providers::api::ISSUE_SEARCH_DEFAULT_LIMIT);
                assert!(!all);
            }
            other => panic!("unexpected {other:?}"),
        }
        let args = ["index", "sync", "--query", "bug", "--state", "open", "--page", "2", "--limit", "10", "--all"]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        match parse_issue_index(&args).unwrap() {
            Command::Issue(crate::command::IssueCommand::IndexSync { query, state, page, limit, all }) => {
                assert_eq!(query.as_deref(), Some("bug"));
                assert_eq!(state, "open");
                assert_eq!(page, 2);
                assert_eq!(limit, 10);
                assert!(all);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn index_search_parser_requires_query_and_validates_bounds() {
        let args = ["index", "search", "--query", "hello"]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        match parse_issue_index(&args).unwrap() {
            Command::Issue(crate::command::IssueCommand::IndexSearch { query, limit, offset, include_body }) => {
                assert_eq!(query, "hello");
                assert_eq!(limit, crate::providers::index::ISSUE_INDEX_SEARCH_DEFAULT_LIMIT);
                assert_eq!(offset, 0);
                assert!(!include_body);
            }
            other => panic!("unexpected {other:?}"),
        }
        let empty = ["index", "search", "--query", "   "].iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(parse_issue_index(&empty).is_err());
        let no_query = ["index", "search"].iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(parse_issue_index(&no_query).is_err());
        let bad_limit = ["index", "search", "--query", "q", "--limit", "0"].iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(parse_issue_index(&bad_limit).is_err());
        let bad_offset = ["index", "search", "--query", "q", "--offset", "notnum"].iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(parse_issue_index(&bad_offset).is_err());
    }

    #[test]
    fn index_help_topics() {
        let args = ["index"].iter().map(|s| s.to_string()).collect::<Vec<_>>();
        match parse_issue_index(&args).unwrap() {
            Command::Help(HelpTopic::IssueCommand(topic)) => assert_eq!(topic, "index"),
            other => panic!("unexpected {other:?}"),
        }
        let args = ["index", "--help"].iter().map(|s| s.to_string()).collect::<Vec<_>>();
        match parse_issue_index(&args).unwrap() {
            Command::Help(HelpTopic::IssueCommand(topic)) => assert_eq!(topic, "index"),
            other => panic!("unexpected {other:?}"),
        }
        let args = ["index", "sync", "--help"].iter().map(|s| s.to_string()).collect::<Vec<_>>();
        match parse_issue_index(&args).unwrap() {
            Command::Help(HelpTopic::IssueCommand(topic)) => assert_eq!(topic, "index sync"),
            other => panic!("unexpected {other:?}"),
        }
        let args = ["index", "search", "--help"].iter().map(|s| s.to_string()).collect::<Vec<_>>();
        match parse_issue_index(&args).unwrap() {
            Command::Help(HelpTopic::IssueCommand(topic)) => assert_eq!(topic, "index search"),
            other => panic!("unexpected {other:?}"),
        }
    }
}
