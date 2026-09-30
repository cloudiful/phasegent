//! Lifecycle tests.
//!
//! Covers repository identity matching, bootstrap hook auto-install gating,
//! and branch-name generation, plus the guarantee that local failures never
//! fail a remote result. Tests run against fake Git runners and real
//! throwaway temp repositories; no network, credentials, or HOME access
//! is involved. SQLite access is isolated to temp databases under the
//! workflow lock for the switch tests.

use crate::git_runner::{GitError, GitOutput, GitRunner};
use crate::lifecycle::{self, HookAutoInstall};
use std::cell::RefCell;

/// P4 attempt-4 focused tests: repo-wide switch gating, explicit-branch
/// durable links, and conventional-default protection. Shared fixtures
/// stay in this file; only new tests live in the child module.
#[path = "phase3_tests/issue_switch.rs"]
mod issue_switch;

/// P4 safe same-checkout create/link/switch plus local-only provider
/// scoping (issue 628). Shared fixtures stay in the child module; only
/// new coverage lives there.
mod create_switch;
// Sibling `issue_switch` coverage (P5a2) resolves its P4 fixtures through
// this namespace; keep the four names reachable without regrowing the
// parent with duplicated helpers.
use create_switch::{canonical_key, current_branch, pin_temp_db, switch_repo};

struct ScriptedRunner {
    responses: RefCell<Vec<(Vec<String>, i32, String)>>,
    calls: RefCell<Vec<Vec<String>>>,
}

impl ScriptedRunner {
    fn new() -> Self {
        Self {
            responses: RefCell::new(Vec::new()),
            calls: RefCell::new(Vec::new()),
        }
    }

    fn expect(&self, args: &[&str], status: i32, stdout: &str) -> &Self {
        self.responses.borrow_mut().push((
            args.iter().map(|value| value.to_string()).collect(),
            status,
            stdout.to_owned(),
        ));
        self
    }

    /// Configures an scp-style origin resolving to OWNER/REPO.
    fn with_origin(&self, owner_repo: &str) -> &Self {
        let (owner, repo) = owner_repo.split_once('/').expect("OWNER/REPO form");
        self.expect(
            &["remote", "get-url", "origin"],
            0,
            &format!("git@git.example:{owner}/{repo}.git"),
        )
    }

    fn without_origin(&self) -> &Self {
        self.expect(&["remote", "get-url", "origin"], 128, "")
    }
}

impl GitRunner for ScriptedRunner {
    fn run(&self, args: &[&str]) -> Result<GitOutput, GitError> {
        self.calls
            .borrow_mut()
            .push(args.iter().map(|value| value.to_string()).collect());
        // Expectations are keyed by full argv and are reusable: identity
        // checks may query origin more than once per test.
        let responses = self.responses.borrow();
        if let Some((_, status, stdout)) = responses
            .iter()
            .find(|(expected, _, _)| expected.as_slice() == args)
        {
            return Ok(GitOutput {
                status: *status,
                stdout: stdout.clone(),
            });
        }
        Err(GitError::new(
            "git",
            format!("unexpected git invocation {args:?}"),
        ))
    }
}

#[test]
fn origin_identity_matches_owner_repo_across_url_shapes() {
    for url in [
        "git@git.example:acme/widgets.git",
        "https://git.example/acme/widgets.git",
        "https://user:secret@git.example/acme/widgets.git",
        "ssh://git@git.example:2222/acme/widgets.git",
    ] {
        let runner = ScriptedRunner::new();
        runner.expect(&["remote", "get-url", "origin"], 0, url);
        assert_eq!(
            lifecycle::origin_identity(&runner).as_deref(),
            Some("acme/widgets"),
            "url {url} should resolve to acme/widgets"
        );
    }
}

#[test]
fn origin_identity_is_none_without_origin_or_git() {
    let no_origin = ScriptedRunner::new();
    no_origin.without_origin();
    assert_eq!(lifecycle::origin_identity(&no_origin), None);

    let not_git = ScriptedRunner::new();
    assert_eq!(lifecycle::origin_identity(&not_git), None);
}

#[test]
fn origin_identity_never_exposes_the_remote_url() {
    let runner = ScriptedRunner::new();
    runner.expect(
        &["remote", "get-url", "origin"],
        0,
        "https://user:super-secret-token@git.example/acme/widgets.git",
    );
    let identity = lifecycle::origin_identity(&runner).unwrap();
    assert_eq!(identity, "acme/widgets");
    assert!(!identity.contains("super-secret-token"));
}

#[test]
fn checkout_gate_requires_origin_and_matching_explicit_repository() {
    let matching = ScriptedRunner::new();
    matching.with_origin("acme/widgets");
    assert!(lifecycle::current_checkout_matches(&matching, None).is_ok());
    assert!(lifecycle::current_checkout_matches(&matching, Some("acme/widgets")).is_ok());

    let mismatch = ScriptedRunner::new();
    mismatch.with_origin("acme/widgets");
    assert!(lifecycle::current_checkout_matches(&mismatch, Some("other/tools")).is_err());
    assert_eq!(
        lifecycle::current_checkout_matches(&mismatch, Some("other/tools")).unwrap_err(),
        "git origin 'acme/widgets' does not match explicit repository 'other/tools'; \
         skipping branch binding"
    );

    let no_origin = ScriptedRunner::new();
    no_origin.without_origin();
    assert!(lifecycle::current_checkout_matches(&no_origin, None).is_err());
}

struct TempRepo(std::path::PathBuf);

impl TempRepo {
    fn new(tag: &str) -> Option<Self> {
        let dir = crate::test_scratch::root().join(format!(
            "phasegent-phase3-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_nanos()
        ));
        let setup = crate::git_runner::ProcessGitRunner::in_directory(&dir);
        setup.run(&["init", "-q"]).ok()?;
        Some(Self(dir))
    }

    fn runner(&self) -> crate::git_runner::ProcessGitRunner {
        crate::git_runner::ProcessGitRunner::in_directory(self.0.clone())
    }

    fn set_origin(&self, url: &str) {
        let output = self
            .runner()
            .run(&["remote", "add", "origin", url])
            .expect("git remote add runs");
        assert_eq!(output.status, 0);
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
fn hook_path(repo: &TempRepo, name: &str) -> std::path::PathBuf {
    let output = repo
        .runner()
        .run(&["rev-parse", "--git-path", "hooks"])
        .expect("git rev-parse works");
    assert_eq!(output.status, 0);
    repo.0.join(output.stdout.trim()).join(name)
}

#[test]
fn hooks_install_only_when_origin_matches_bootstrap_repository() {
    let Some(repo) = TempRepo::new("match") else {
        eprintln!("git unavailable; skipping");
        return;
    };
    repo.set_origin("https://git.example/acme/widgets.git");

    let outcome = lifecycle::auto_install_hooks(&repo.runner(), &repo.0, "acme/widgets");
    let HookAutoInstall::Installed(installed) = outcome else {
        panic!("matching origin should install hooks");
    };
    #[cfg(unix)]
    for name in ["prepare-commit-msg", "commit-msg"] {
        assert!(hook_path(&repo, name).is_file(), "{name} should exist");
    }
    assert!(installed.installed.contains(&"prepare-commit-msg"));
    assert!(installed.installed.contains(&"commit-msg"));
}

#[test]
fn hooks_skip_for_mismatched_bootstrap_repository() {
    let Some(repo) = TempRepo::new("mismatch") else {
        eprintln!("git unavailable; skipping");
        return;
    };
    repo.set_origin("https://git.example/acme/widgets.git");

    let outcome = lifecycle::auto_install_hooks(&repo.runner(), &repo.0, "other/tools");
    match outcome {
        HookAutoInstall::Skipped { reason } => {
            assert!(reason.contains("does not match"));
        }
        other => panic!("mismatched repository must skip, got {other:?}"),
    }
    #[cfg(unix)]
    assert!(!hook_path(&repo, "prepare-commit-msg").exists());
}

#[test]
fn hooks_skip_without_origin() {
    let Some(repo) = TempRepo::new("noorigin") else {
        eprintln!("git unavailable; skipping");
        return;
    };

    let outcome = lifecycle::auto_install_hooks(&repo.runner(), &repo.0, "acme/widgets");
    assert!(matches!(outcome, HookAutoInstall::Skipped { .. }));
    #[cfg(unix)]
    assert!(!hook_path(&repo, "prepare-commit-msg").exists());
}

#[test]
fn branch_prefix_maps_bug_to_fix_and_defaults_to_feat() {
    assert_eq!(lifecycle::branch_prefix_for_tracker(Some("Bug")), "fix");
    assert_eq!(lifecycle::branch_prefix_for_tracker(Some("bug")), "fix");
    assert_eq!(
        lifecycle::branch_prefix_for_tracker(Some("Feature")),
        "feat"
    );
    assert_eq!(lifecycle::branch_prefix_for_tracker(None), "feat");
    assert_eq!(lifecycle::branch_prefix_for_tracker(Some("1")), "feat");
    assert_eq!(
        lifecycle::branch_name_for_issue(Some("Bug"), 452),
        "fix/452"
    );
    assert_eq!(
        lifecycle::branch_name_for_issue(Some("Feature"), 452),
        "feat/452"
    );
    assert_eq!(lifecycle::branch_name_for_issue(None, 452), "feat/452");
}
