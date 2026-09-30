use crate::branch_naming::parse_issue_id_from_branch_name;

#[test]
fn branch_name_fallback_prefers_type_id_then_trailing_then_bare() {
    // Rule 1: type/id[-slug] first.
    assert_eq!(parse_issue_id_from_branch_name("feat/452"), Some(452));
    assert_eq!(parse_issue_id_from_branch_name("feat/452-slug"), Some(452));
    assert_eq!(parse_issue_id_from_branch_name("fix/452_extra"), Some(452));
    // Rule 2: trailing -/_id.
    assert_eq!(
        parse_issue_id_from_branch_name("feat/help-unify-447"),
        Some(447)
    );
    assert_eq!(
        parse_issue_id_from_branch_name("feat/help-unify_447"),
        Some(447)
    );
    // Rule 3: bare digits.
    assert_eq!(parse_issue_id_from_branch_name("452"), Some(452));
    // Rule 1 wins over rule 2 when both match.
    assert_eq!(
        parse_issue_id_from_branch_name("feat/452-help-447"),
        Some(452)
    );
    // Non-positive and non-numeric names have no fallback.
    assert_eq!(parse_issue_id_from_branch_name("main"), None);
    assert_eq!(parse_issue_id_from_branch_name("feature/x"), None);
    assert_eq!(parse_issue_id_from_branch_name("0"), None);
    assert_eq!(parse_issue_id_from_branch_name("feat/0"), None);
    assert_eq!(parse_issue_id_from_branch_name(""), None);
    assert_eq!(parse_issue_id_from_branch_name("feat/"), None);
}

#[test]
fn real_workflow_branch_shapes_resolve_to_the_expected_issue() {
    // Seeds include the worktree branch names minted for issues #544, #552
    // and #558 (`phasegent/<issue>-<short6hex>`), which is the shape the
    // leasing flow produces for every session.
    let cases: &[(&str, Option<u64>)] = &[
        ("phasegent/544-d5dc27", Some(544)),
        ("phasegent/552-c77a19", Some(552)),
        ("phasegent/558-544da0", Some(558)),
        ("feat/452", Some(452)),
        ("feat/452-slug", Some(452)),
        ("feat/help-unify-447", Some(447)),
        ("452", Some(452)),
        ("feat/452-help-447", Some(452)),
        ("feat/0", None),
        ("0", None),
        ("feat/", None),
        ("", None),
    ];
    for (branch, expected) in cases {
        assert_eq!(
            parse_issue_id_from_branch_name(branch),
            *expected,
            "branch {branch:?}"
        );
    }
}
