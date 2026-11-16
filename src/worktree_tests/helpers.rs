use super::*;

#[test]
fn validate_ref_format_accepts_documented_branch_shape() {
    for input in [
        "phasegent/239-abcdef",
        "phasegent/1-aaaaaa",
        "main",
        "feature/x",
        "release/2024-01",
        "x",
    ] {
        assert!(
            validate_ref_format(input).is_ok(),
            "input {input:?} must pass"
        );
    }
}

#[test]
fn validate_ref_format_rejects_disallowed_characters() {
    for input in [
        "",                       // empty
        "PhaseGent/239-aaa",      // uppercase
        "phasegent/.239-aaa",     // leading dot inside segment
        "phasegent/239-aaa..",    // trailing ..
        "phasegent/239-aaa@{",    // @{ sequence
        "phasegent/239-aaa\\x",   // backslash
        "-leading-dash",          // leading dash
        "trailing-dot.",          // trailing dot
        "trailing.lock",          // trailing .lock
        "trailing/slash/",        // trailing slash
        "phasegent/239-aaa..bbb", // .. segment
        "phasegent/239-aaa bbb",  // embedded space
        "phasegent/239-aaa~1",    // ~ (not in our allowlist)
        "phasegent/239-aaa^1",    // ^
        "phasegent/239-aaa:1",    // :
        "phasegent/239-aaa?1",    // ?
        "phasegent/239-aaa*1",    // *
        "phasegent/239-aaa[1]",   // [
    ] {
        let error = validate_ref_format(input)
            .err()
            .unwrap_or_else(|| panic!("input {input:?} should have failed"));
        assert_eq!(error.kind, "argument", "input {input:?}");
    }
}

#[test]
fn validate_ref_format_rejects_oversized_names() {
    let oversized = "a".repeat(129);
    let error = validate_ref_format(&oversized).unwrap_err();
    assert_eq!(error.kind, "argument");
}

#[test]
fn compute_fingerprint_is_deterministic_and_distinguishes_repos() {
    let a = compute_fingerprint("/home/dev/repo-a/.git");
    let b = compute_fingerprint("/home/dev/repo-b/.git");
    let a2 = compute_fingerprint("/home/dev/repo-a/.git");
    assert_eq!(a, a2, "fingerprint must be stable across calls");
    assert_ne!(a, b, "different repos must produce different fingerprints");
    assert_eq!(a.len(), 12);
    assert!(
        a.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
}

#[test]
fn generate_branch_produces_phasegent_namespaced_short_hex_branches() {
    for issue in [1, 42, 239, 999_999] {
        let (branch, short) = generate_branch(issue).expect("generate");
        assert!(branch.starts_with(&format!("phasegent/{issue}-")));
        assert_eq!(short.len(), 6);
        assert!(short.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(validate_ref_format(&branch).is_ok());
    }
}

#[test]
fn slug_from_branch_replaces_slash_with_dash() {
    assert_eq!(
        slug_from_branch("phasegent/239-abcdef").unwrap(),
        "phasegent-239-abcdef"
    );
    assert_eq!(slug_from_branch("main").unwrap(), "main");
    let error = slug_from_branch("PhaseGent/239-abcdef").unwrap_err();
    assert_eq!(error.kind, "argument");
}

#[test]
fn parse_worktree_list_handles_porcelain_blocks_with_extra_fields() {
    let raw = "\
worktree /home/dev/repo
HEAD abcdef1234567890
branch refs/heads/main

worktree /home/dev/repo-wt
HEAD 0123456789abcdef
branch refs/heads/phasegent/239-abcdef
detached

";
    let entries: Vec<WorktreeListEntry> = parse_worktree_list(raw);
    assert_eq!(
        entries,
        vec![
            WorktreeListEntry {
                worktree: "/home/dev/repo".to_owned(),
                head: Some("abcdef1234567890".to_owned()),
                branch: Some("refs/heads/main".to_owned()),
            },
            WorktreeListEntry {
                worktree: "/home/dev/repo-wt".to_owned(),
                head: Some("0123456789abcdef".to_owned()),
                branch: Some("refs/heads/phasegent/239-abcdef".to_owned()),
            },
        ]
    );
}

#[cfg(unix)]
#[test]
fn parse_worktree_list_preserves_a_trailing_cr_path_on_unix() {
    let entries = parse_worktree_list("worktree /tmp/repo\r\nHEAD abc\n\n");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].worktree, "/tmp/repo\r");
}

#[test]
fn parse_worktree_list_returns_empty_for_empty_input() {
    assert!(parse_worktree_list("").is_empty());
    assert!(parse_worktree_list("\n\n\n").is_empty());
}
