use super::support::*;
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

#[test]
fn parse_worktree_list_returns_empty_for_empty_input() {
    assert!(parse_worktree_list("").is_empty());
    assert!(parse_worktree_list("\n\n\n").is_empty());
}

// ---------------------------------------------------------------------------
// repo_identity via FakeWorktreeRunner.
// ---------------------------------------------------------------------------

#[test]
fn repo_identity_canonicalises_relative_and_absolute_common_dirs() {
    let repo_path = PathBuf::from("/tmp/some/repo");
    let runner = FakeWorktreeRunner::new(vec![FakeResponse {
        args: vec!["rev-parse".to_string(), "--git-common-dir".to_string()],
        status: 0,
        stdout: ".git".to_string(),
    }]);
    let identity = repo_identity(&runner, &repo_path).expect("identity");
    assert!(identity.ends_with("/repo/.git") || identity.ends_with("/repo"));
    let recorded = runner.recorded();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].0, vec!["rev-parse", "--git-common-dir"]);
    assert_eq!(recorded[0].1, repo_path);
}

#[test]
fn repo_identity_reports_structured_git_failure() {
    let repo_path = PathBuf::from("/tmp/some/repo");
    let runner = FakeWorktreeRunner::new(vec![FakeResponse {
        args: vec!["rev-parse".to_string(), "--git-common-dir".to_string()],
        status: 128,
        stdout: "fatal: not a git repo".to_string(),
    }]);
    let error = repo_identity(&runner, &repo_path).unwrap_err();
    assert_eq!(error.kind, "git");
    assert!(!error.message.is_empty());
}

// ---------------------------------------------------------------------------
// Schema and storage tests (real SQLite via Storage::open_at).
// ---------------------------------------------------------------------------

#[test]
fn cache_root_in_creates_private_dir_under_the_provided_base() {
    let base = unique_cache("root");
    let fingerprint = "abcdef123456";
    let dir = cache_root_in(base.path(), fingerprint).expect("cache root");
    assert!(dir.exists());
    assert!(dir.ends_with("worktrees/abcdef123456"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "cache dir must be 0700");
    }
    drop(base);
}

// ---------------------------------------------------------------------------
// Real-git tests. Each test runs in its own temp repo so the
// production / current worktree tree is never mutated.
// ---------------------------------------------------------------------------
