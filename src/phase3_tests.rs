//! Phase 3 lifecycle tests.
//!
//! Covers repository identity matching, bootstrap hook auto-install gating,
//! Redmine create auto-bind / close auto-unbind, and the guarantee that local
//! failures never fail a remote result. Tests run against fake Git runners
//! and real throwaway temp repositories; no network, credentials, HOME, or
//! SQLite access is involved.

use crate::branch_context::{GitOutput, GitRunner};
use crate::lifecycle::{
    self, AutoBindOutcome, AutoUnbindOutcome, HookAutoInstall, MAX_WARNING_CHARS,
};
use std::cell::RefCell;

// ---------------------------------------------------------------------------
// Fake runner driven by a scripted argv -> (status, stdout) table.
// ---------------------------------------------------------------------------

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

    fn recorded_writes_to(&self, key: &str) -> Vec<Vec<String>> {
        self.calls
            .borrow()
            .iter()
            .filter(|args| {
                args.len() >= 4 && args[0] == "config" && args[1] == "--local" && args[2] == key
            })
            .cloned()
            .collect()
    }
}

impl GitRunner for ScriptedRunner {
    fn run(&self, args: &[&str]) -> Result<GitOutput, crate::branch_context::BranchContextError> {
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
        Err(crate::branch_context::BranchContextError::new(
            "git",
            format!("unexpected git invocation {args:?}"),
        ))
    }
}

fn branch_and_binding_runner(branch: &str, stored: Option<u64>) -> ScriptedRunner {
    let runner = ScriptedRunner::new();
    runner.expect(&["symbolic-ref", "--quiet", "--short", "HEAD"], 0, branch);
    match stored {
        Some(id) => runner.expect(
            &[
                "config",
                "--local",
                "--get",
                &format!("branch.{branch}.redmine-issue-id"),
            ],
            0,
            &id.to_string(),
        ),
        None => runner.expect(
            &[
                "config",
                "--local",
                "--get",
                &format!("branch.{branch}.redmine-issue-id"),
            ],
            1,
            "",
        ),
    };
    // Any remaining config write/unset succeeds unless a test overrides it
    // first; expectations are consumed so overrides must be queued later.
    runner
}

// ---------------------------------------------------------------------------
// Repository identity matching.
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Bootstrap hook auto-install gating (real temp repos, injectable runner).
// ---------------------------------------------------------------------------

struct TempRepo(std::path::PathBuf);

impl TempRepo {
    fn new(tag: &str) -> Option<Self> {
        let dir = std::env::temp_dir().join(format!(
            "phasegent-phase3-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_nanos()
        ));
        let setup = crate::branch_context::ProcessGitRunner::in_directory(&dir);
        setup.run(&["init", "-q"]).ok()?;
        Some(Self(dir))
    }

    fn runner(&self) -> crate::branch_context::ProcessGitRunner {
        crate::branch_context::ProcessGitRunner::in_directory(self.0.clone())
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

// ---------------------------------------------------------------------------
// Redmine create auto-bind.
// ---------------------------------------------------------------------------

#[test]
fn create_binds_issue_to_current_branch_on_success() {
    let runner = branch_and_binding_runner("feature/one", None);
    runner.with_origin("acme/widgets");
    runner.expect(
        &[
            "config",
            "--local",
            "branch.feature/one.redmine-issue-id",
            "77",
        ],
        0,
        "",
    );

    let outcome = lifecycle::bind_created_issue(&runner, 77, None);
    assert_eq!(
        outcome,
        AutoBindOutcome::Bound {
            branch: "feature/one".to_owned(),
            issue_id: 77,
        }
    );
    assert!(outcome.warning().is_none());
}

#[test]
fn create_same_issue_binding_is_idempotent() {
    let runner = branch_and_binding_runner("main", Some(42));
    runner.with_origin("acme/widgets");

    let outcome = lifecycle::bind_created_issue(&runner, 42, None);
    assert_eq!(
        outcome,
        AutoBindOutcome::Idempotent {
            branch: "main".to_owned(),
            issue_id: 42,
        }
    );
}

#[test]
fn create_skips_silently_in_non_git_directory() {
    let runner = ScriptedRunner::new();
    let outcome = lifecycle::bind_created_issue(&runner, 9, None);
    assert!(matches!(outcome, AutoBindOutcome::Skipped { .. }));
    assert!(outcome.warning().is_none());
}

#[test]
fn create_skips_silently_on_mismatched_explicit_repository() {
    let runner = branch_and_binding_runner("main", None);
    runner.with_origin("acme/widgets");

    let outcome = lifecycle::bind_created_issue(&runner, 9, Some("other/tools"));
    assert!(matches!(outcome, AutoBindOutcome::Skipped { .. }));
    assert!(outcome.warning().is_none());
}

#[test]
fn create_detached_head_warns_but_preserves_remote_result() {
    let runner = ScriptedRunner::new();
    runner.with_origin("acme/widgets");
    runner.expect(&["symbolic-ref", "--quiet", "--short", "HEAD"], 1, "");

    let outcome = lifecycle::bind_created_issue(&runner, 12, None);
    let warning = outcome.warning().expect("detached HEAD warns");
    assert!(warning.contains("issue 12 created"), "{warning}");
    assert!(warning.contains("detach"), "{warning}");
}

#[test]
fn create_does_not_overwrite_existing_different_binding() {
    let runner = branch_and_binding_runner("release", Some(5));
    runner.with_origin("acme/widgets");

    let outcome = lifecycle::bind_created_issue(&runner, 6, None);
    let warning = outcome.warning().expect("conflicting binding warns");
    assert!(
        warning.contains("issue 5") && warning.contains("issue 6"),
        "{warning}"
    );
    // No `git config --local branch.release.redmine-issue-id <id>` write was
    // attempted beyond the read.
    assert!(
        runner
            .recorded_writes_to("branch.release.redmine-issue-id")
            .is_empty(),
        "existing binding must not be rewritten"
    );
}

#[test]
fn create_local_write_failure_warns_with_bounded_message() {
    let runner = branch_and_binding_runner("main", None);
    runner.with_origin("acme/widgets");
    runner.expect(
        &["config", "--local", "branch.main.redmine-issue-id", "8"],
        1_000_000,
        "",
    ); // absurd status exercises the failure path deterministically

    let outcome = lifecycle::bind_created_issue(&runner, 8, None);
    let warning = outcome.warning().expect("write failure warns");
    assert!(warning.chars().count() <= MAX_WARNING_CHARS + 20);
    assert!(warning.contains("failed"), "{warning}");
}

// ---------------------------------------------------------------------------
// Redmine close auto-unbind.
// ---------------------------------------------------------------------------

#[test]
fn close_unbinds_only_exact_current_issue() {
    let runner = branch_and_binding_runner("fix/bug", Some(31));
    runner.with_origin("acme/widgets");
    runner.expect(
        &[
            "config",
            "--local",
            "--unset",
            "branch.fix/bug.redmine-issue-id",
        ],
        0,
        "",
    );

    let outcome = lifecycle::unbind_closed_issue(&runner, 31, None);
    assert_eq!(
        outcome,
        AutoUnbindOutcome::Unbound {
            branch: "fix/bug".to_owned(),
            issue_id: 31,
        }
    );
}

#[test]
fn close_never_unbinds_a_different_issue_or_missing_binding() {
    let bound_other = branch_and_binding_runner("main", Some(10));
    bound_other.with_origin("acme/widgets");
    let outcome = lifecycle::unbind_closed_issue(&bound_other, 11, None);
    match &outcome {
        AutoUnbindOutcome::Noop { reason } => {
            assert!(
                reason.contains("bound to 10") && reason.contains("issue 11"),
                "{reason}"
            );
        }
        other => panic!("different binding must be a noop, got {other:?}"),
    }
    assert!(outcome.warning().is_none());

    let unbound = branch_and_binding_runner("main", None);
    unbound.with_origin("acme/widgets");
    assert!(matches!(
        lifecycle::unbind_closed_issue(&unbound, 11, None),
        AutoUnbindOutcome::Noop { .. }
    ));
}

#[test]
fn close_detached_head_and_mismatched_repository_are_noops() {
    let detached = ScriptedRunner::new();
    detached.with_origin("acme/widgets");
    detached.expect(&["symbolic-ref", "--quiet", "--short", "HEAD"], 1, "");
    assert!(matches!(
        lifecycle::unbind_closed_issue(&detached, 3, None),
        AutoUnbindOutcome::Noop { .. }
    ));

    let mismatch = branch_and_binding_runner("main", Some(3));
    mismatch.with_origin("acme/widgets");
    assert!(matches!(
        lifecycle::unbind_closed_issue(&mismatch, 3, Some("other/tools")),
        AutoUnbindOutcome::Noop { .. }
    ));
}

#[test]
fn close_local_unbind_failure_warns_but_preserves_close_result() {
    let runner = branch_and_binding_runner("main", Some(55));
    runner.with_origin("acme/widgets");
    runner.expect(
        &[
            "config",
            "--local",
            "--unset",
            "branch.main.redmine-issue-id",
        ],
        7,
        "",
    );

    let outcome = lifecycle::unbind_closed_issue(&runner, 55, None);
    let warning = outcome.warning().expect("unbind failure warns");
    assert!(warning.contains("closed"), "{warning}");
}
