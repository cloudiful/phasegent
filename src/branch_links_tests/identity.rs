use super::*;
use crate::branch_links::{self, IssueKey};

#[test]
fn canonical_origin_is_portable_across_transports() {
    let https = branch_links::repo_key_for_origin("https://forge.example.com/owner/repo.git")
        .expect("https origin must parse");
    let ssh = branch_links::repo_key_for_origin("git@forge.example.com:owner/repo.git")
        .expect("scp origin must parse");
    assert_eq!(https, ssh);
    let other = branch_links::repo_key_for_origin("https://forge.example.com/owner/other.git")
        .expect("other repo must parse");
    assert_ne!(https, other);
}

#[test]
fn same_origin_clones_share_links_while_distinct_repos_do_not() {
    let first = TempRepo::init("clone-a");
    let second = TempRepo::init("clone-b");
    first.set_origin("https://forge.example.com/owner/repo.git");
    second.set_origin("git@forge.example.com:owner/repo.git");

    let key_a = branch_links::repo_key_for_origin("https://forge.example.com/owner/repo.git")
        .expect("key a");
    let key_b =
        branch_links::repo_key_for_origin("git@forge.example.com:owner/repo.git").expect("key b");
    assert_eq!(key_a, key_b);

    let connection = open_memory_db();
    let issue = IssueKey::from_number("redmine", "tools/phasegent", 628).expect("issue key");
    branch_links::store::link(
        &connection,
        &branch_links::LinkParams {
            repo_key: &key_a,
            branch: "feat/628",
            issue: &issue,
            issue_number: 628,
            source: "manual",
            now: 1_700_000_001,
        },
    )
    .expect("link must insert");

    let via_second = branch_links::issues_for_branch(
        &connection,
        &key_b,
        "feat/628",
        false,
        &branch_links::UnknownState,
    )
    .expect("forward read must work");
    assert_eq!(via_second.len(), 1);

    let foreign = branch_links::repo_key_for_origin("https://forge.example.com/owner/other.git")
        .expect("foreign key");
    let via_foreign = branch_links::issues_for_branch(
        &connection,
        &foreign,
        "feat/628",
        false,
        &branch_links::UnknownState,
    )
    .expect("foreign read must work");
    assert!(via_foreign.is_empty());
}

#[test]
fn missing_origin_falls_back_to_local_only_key() {
    let checkout = Path::new("/tmp/phasegent-no-origin-checkout");
    let resolved = branch_links::resolve_repo_key(None, checkout).expect("fallback must resolve");
    assert!(resolved.local_only);
    assert!(resolved.key.starts_with(branch_links::LOCAL_KEY_PREFIX));

    let canonical =
        branch_links::resolve_repo_key(Some("https://forge.example.com/owner/repo.git"), checkout)
            .expect("canonical must resolve");
    assert!(!canonical.local_only);
    assert!(!canonical.key.starts_with(branch_links::LOCAL_KEY_PREFIX));
}

#[test]
fn default_branch_detection_and_protection() {
    use crate::branch_context::{BranchContextError, GitOutput, GitRunner};
    use std::cell::RefCell;

    struct Fake {
        calls: RefCell<Vec<Vec<String>>>,
    }
    impl GitRunner for Fake {
        fn run(&self, args: &[&str]) -> Result<GitOutput, BranchContextError> {
            self.calls
                .borrow_mut()
                .push(args.iter().map(|value| value.to_string()).collect());
            if args == ["symbolic-ref", "refs/remotes/origin/HEAD"] {
                return Ok(GitOutput {
                    status: 0,
                    stdout: "refs/remotes/origin/main".to_owned(),
                });
            }
            Err(BranchContextError::new("git", "unexpected"))
        }
    }

    let runner = Fake {
        calls: RefCell::new(Vec::new()),
    };
    assert_eq!(
        branch_links::detect_default_branch(&runner),
        Some("main".to_owned())
    );
    assert!(branch_links::validate_not_default_branch("feat/628", Some("main")).is_ok());
    let error = branch_links::validate_not_default_branch("main", Some("main"))
        .expect_err("default branch must be rejected");
    assert!(error.contains("default branch"));
    assert!(branch_links::is_default_branch("main", Some("main")));
    assert!(!branch_links::is_default_branch("feat/628", Some("main")));
    assert!(!branch_links::is_default_branch("main", None));
}

#[test]
fn unknown_default_branch_means_no_protection_claim() {
    use crate::branch_context::{BranchContextError, GitOutput, GitRunner};

    struct Failing;
    impl GitRunner for Failing {
        fn run(&self, _args: &[&str]) -> Result<GitOutput, BranchContextError> {
            Ok(GitOutput {
                status: 1,
                stdout: String::new(),
            })
        }
    }
    let runner = Failing;
    assert_eq!(branch_links::detect_default_branch(&runner), None);
    assert!(
        branch_links::validate_not_default_branch("main", None).is_ok(),
        "unknown default must not block, only skip auto-switch"
    );
}

#[test]
fn default_branch_fallback_uses_cached_remote_show_only() {
    use crate::branch_context::{BranchContextError, GitOutput, GitRunner};
    use std::cell::RefCell;

    struct Cached {
        calls: RefCell<Vec<Vec<String>>>,
        fixture: String,
    }
    impl GitRunner for Cached {
        fn run(&self, args: &[&str]) -> Result<GitOutput, BranchContextError> {
            self.calls
                .borrow_mut()
                .push(args.iter().map(|value| value.to_string()).collect());
            if args == ["symbolic-ref", "refs/remotes/origin/HEAD"] {
                return Ok(GitOutput {
                    status: 1,
                    stdout: String::new(),
                });
            }
            if args == ["remote", "show", "-n", "origin"] {
                return Ok(GitOutput {
                    status: 0,
                    stdout: self.fixture.clone(),
                });
            }
            Err(BranchContextError::new("git", "unexpected"))
        }
    }

    let cached = Cached {
        calls: RefCell::new(Vec::new()),
        fixture: "* remote origin\n  HEAD branch: main\n  Remote branches:\n    main tracked\n"
            .to_owned(),
    };
    assert_eq!(
        branch_links::detect_default_branch(&cached),
        Some("main".to_owned())
    );
    let calls = cached.calls.borrow();
    assert!(
        calls
            .iter()
            .any(|call| call == &["remote", "show", "-n", "origin"]),
        "fallback must use the cached-only probe"
    );
    assert!(
        !calls
            .iter()
            .any(|call| call == &["remote", "show", "origin"]),
        "fallback must never query the remote without -n"
    );

    let unknown = Cached {
        calls: RefCell::new(Vec::new()),
        fixture: "* remote origin\n  HEAD branch: (unknown)\n".to_owned(),
    };
    assert_eq!(branch_links::detect_default_branch(&unknown), None);

    // `remote show -n` reports `(not queried)` when remote heads were
    // never listed; it is not a branch name either.
    let not_queried = Cached {
        calls: RefCell::new(Vec::new()),
        fixture: "* remote origin\n  HEAD branch: (not queried)\n".to_owned(),
    };
    assert_eq!(branch_links::detect_default_branch(&not_queried), None);
}

#[test]
fn checkout_root_prefers_git_toplevel_over_cwd() {
    use crate::branch_context::{BranchContextError, GitOutput, GitRunner};

    struct Top {
        toplevel: String,
    }
    impl GitRunner for Top {
        fn run(&self, args: &[&str]) -> Result<GitOutput, BranchContextError> {
            assert_eq!(args, ["rev-parse", "--show-toplevel"]);
            Ok(GitOutput {
                status: 0,
                stdout: self.toplevel.clone(),
            })
        }
    }
    let runner = Top {
        toplevel: "/repo/checkout\n".to_owned(),
    };
    assert_eq!(
        branch_links::checkout_root(&runner),
        std::path::PathBuf::from("/repo/checkout")
    );
}
