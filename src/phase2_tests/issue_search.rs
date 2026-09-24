use super::*;

#[test]
fn issue_search_bounded_pagination_parses_and_validates() {
    // Default page 1 limit 50, requires query or --all
    let args = ["issue", "search", "--query", "needle"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    match command::parse_with_role_env(&args, Some("orchestrator"))
        .unwrap()
        .command
    {
        command::Command::Issue(command::IssueCommand::Search {
            query,
            state,
            page,
            limit,
            all,
            include_body,
        }) => {
            assert_eq!(query.as_deref(), Some("needle"));
            assert_eq!(state, "all");
            assert_eq!(page, crate::providers::api::ISSUE_SEARCH_DEFAULT_PAGE);
            assert_eq!(limit, crate::providers::api::ISSUE_SEARCH_DEFAULT_LIMIT);
            assert!(!all);
            assert!(!include_body);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // --all allows empty query, bounded listing
    let args = ["issue", "search", "--all"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    match command::parse_with_role_env(&args, Some("orchestrator"))
        .unwrap()
        .command
    {
        command::Command::Issue(command::IssueCommand::Search { all, query, .. }) => {
            assert!(all);
            assert!(query.is_none());
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // whitespace-only query without --all is rejected at validation layer
    let args = ["issue", "search", "--query", "   "]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse_with_role_env(&args, Some("orchestrator")).unwrap();
    match invocation.command {
        command::Command::Issue(command::IssueCommand::Search {
            query,
            state,
            page,
            limit,
            all,
            include_body,
        }) => {
            let opts = crate::providers::IssueSearchOptions {
                query,
                state,
                page,
                limit,
                include_body,
                all,
            };
            assert!(opts.validate().is_err());
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // without query and without --all is rejected at validation layer
    let args = ["issue", "search"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse_with_role_env(&args, Some("orchestrator")).unwrap();
    match invocation.command {
        command::Command::Issue(command::IssueCommand::Search {
            query,
            state,
            page,
            limit,
            all,
            include_body,
        }) => {
            let opts = crate::providers::IssueSearchOptions {
                query,
                state,
                page,
                limit,
                include_body,
                all,
            };
            assert!(opts.validate().is_err());
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // page/limit validation
    let args = [
        "issue",
        "search",
        "--query",
        "q",
        "--page",
        "2",
        "--limit",
        "10",
        "--include-body",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse_with_role_env(&args, Some("orchestrator"))
        .unwrap()
        .command
    {
        command::Command::Issue(command::IssueCommand::Search {
            page,
            limit,
            include_body,
            ..
        }) => {
            assert_eq!(page, 2);
            assert_eq!(limit, 10);
            assert!(include_body);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    for (page, limit) in [(0, 50), (1, 0), (1, 101)] {
        let args = [
            "issue",
            "search",
            "--query",
            "q",
            "--page",
            &page.to_string(),
            "--limit",
            &limit.to_string(),
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        assert!(
            command::parse_with_role_env(&args, Some("orchestrator")).is_err(),
            "should reject page={page} limit={limit}"
        );
    }

    // provider-neutral options validation and envelope truncation
    let opts = crate::providers::IssueSearchOptions {
        query: Some("q".to_owned()),
        state: "open".to_owned(),
        page: 1,
        limit: 50,
        include_body: false,
        all: false,
    };
    assert!(opts.validate().is_ok());
    assert_eq!(opts.effective_query(), Some("q"));

    let long = "b".repeat(crate::providers::api::ISSUE_SEARCH_MAX_BODY_BYTES + 3);
    let summary = crate::providers::IssueSummary {
        id: 1,
        number: 1,
        title: "t".to_owned(),
        body: long.clone(),
        state: "open".to_owned(),
        html_url: None,
        project: None,
    };
    let item = crate::providers::api::IssueSearchItem::from_summary(summary, false);
    assert!(item.body.is_none());
    assert!(item.body_truncated.is_none());

    let summary2 = crate::providers::IssueSummary {
        id: 1,
        number: 1,
        title: "t".to_owned(),
        body: long.clone(),
        state: "open".to_owned(),
        html_url: None,
        project: None,
    };
    let item2 = crate::providers::api::IssueSearchItem::from_summary(summary2, true);
    assert_eq!(item2.body_truncated, Some(true));
    assert_eq!(
        item2.body.as_ref().unwrap().len(),
        crate::providers::api::ISSUE_SEARCH_MAX_BODY_BYTES
    );

    let short = crate::providers::IssueSummary {
        id: 2,
        number: 2,
        title: "t".to_owned(),
        body: "short".to_owned(),
        state: "open".to_owned(),
        html_url: None,
        project: None,
    };
    let item3 = crate::providers::api::IssueSearchItem::from_summary(short, true);
    assert_eq!(item3.body_truncated, Some(false));
    assert_eq!(item3.body.as_deref(), Some("short"));
}

#[test]
fn issue_search_body_truncation_is_byte_safe_for_multibyte() {
    // 8192 byte cap must be enforced on byte length, not char count, and must
    // not split UTF-8 code points. CJK (3 bytes) and emoji (4 bytes) are the
    // canonical edge cases.
    let cjk = "汉".repeat(3000); // 9000 bytes, 3000 chars
    let summary = crate::providers::IssueSummary {
        id: 10,
        number: 10,
        title: "cjk".to_owned(),
        body: cjk,
        state: "open".to_owned(),
        html_url: None,
        project: None,
    };
    let item = crate::providers::api::IssueSearchItem::from_summary(summary, true);
    assert_eq!(item.body_truncated, Some(true));
    let body = item.body.unwrap();
    assert!(body.len() <= crate::providers::api::ISSUE_SEARCH_MAX_BODY_BYTES);
    // Must remain valid UTF-8 and end on a char boundary; the helper must
    // have trimmed to the previous boundary rather than splitting 汉.
    assert!(body.is_char_boundary(body.len()));
    // 8192 is not divisible by 3, so the truncated CJK body must be <8192.
    // 8192 /3 = 2730 rem 2 => floor is 2730*3 = 8190 bytes.
    assert_eq!(body.len(), 8190);
    assert_eq!(body.chars().count(), 2730);

    let emoji = "😀".repeat(3000); // 12000 bytes, 3000 chars
    let summary = crate::providers::IssueSummary {
        id: 11,
        number: 11,
        title: "emoji".to_owned(),
        body: emoji,
        state: "open".to_owned(),
        html_url: None,
        project: None,
    };
    let item = crate::providers::api::IssueSearchItem::from_summary(summary, true);
    assert_eq!(item.body_truncated, Some(true));
    let body = item.body.unwrap();
    assert!(body.len() <= crate::providers::api::ISSUE_SEARCH_MAX_BODY_BYTES);
    assert!(body.is_char_boundary(body.len()));
    // 8192 /4 = 2048 exactly, so emoji truncation lands exactly on 8192.
    assert_eq!(body.len(), 8192);
    assert_eq!(body.chars().count(), 2048);

    // Mixed content where the cut falls inside a multibyte sequence
    let mixed = format!("{}{}", "a".repeat(8190), "😀"); // 8190+4=8194 bytes
    let summary = crate::providers::IssueSummary {
        id: 12,
        number: 12,
        title: "mixed".to_owned(),
        body: mixed,
        state: "open".to_owned(),
        html_url: None,
        project: None,
    };
    let item = crate::providers::api::IssueSearchItem::from_summary(summary, true);
    assert_eq!(item.body_truncated, Some(true));
    let body = item.body.unwrap();
    // The emoji would push over 8192, so it must be dropped entirely rather
    // than split; result is the 8190 ascii bytes.
    assert_eq!(body.len(), 8190);
    assert_eq!(body, "a".repeat(8190));
    assert!(body.is_char_boundary(body.len()));

    // Exactly at cap must not be marked truncated
    let exact = "b".repeat(crate::providers::api::ISSUE_SEARCH_MAX_BODY_BYTES);
    let summary = crate::providers::IssueSummary {
        id: 13,
        number: 13,
        title: "exact".to_owned(),
        body: exact.clone(),
        state: "open".to_owned(),
        html_url: None,
        project: None,
    };
    let item = crate::providers::api::IssueSearchItem::from_summary(summary, true);
    assert_eq!(item.body_truncated, Some(false));
    assert_eq!(
        item.body.unwrap().len(),
        crate::providers::api::ISSUE_SEARCH_MAX_BODY_BYTES
    );
}

// ---------------------------------------------------------------------------
// Issue 305 Task 3, widened by issue 537 Phase 2: `issue close` releases
// every active worktree lease the closed issue holds across all sessions,
// and only after the provider confirmed the close.
//
// These tests drive `execute_issue` end-to-end against the deterministic
// local provider (no mock HTTP) with a temp worktree database and a temp
// git checkout. They prove the provider success/failure split, the
// cross-session release, and the no-session no-op.
// ---------------------------------------------------------------------------
