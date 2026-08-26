//! Focused tests for managed hook installation and commit-message behavior.
//!
//! Installation tests run against real temp Git repositories and skip
//! silently when `git` is unavailable; message behavior is exercised through
//! the internal `hooks run` entry points with plain files. No credentials or
//! network access is involved.

use crate::branch_context::{self, GitRunner, ProcessGitRunner};
use crate::command::{self, Command};
use crate::hooks::{self, HookKind, HooksCommand, MANAGED_MARKER, ORIGINAL_BACKUP_DIR};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Temp repository fixture (local to this module so branch-context tests stay
// independent).
// ---------------------------------------------------------------------------

struct TempRepo(PathBuf);

impl TempRepo {
    fn new(tag: &str) -> Option<Self> {
        let dir = std::env::temp_dir().join(format!(
            "phasegent-hooks-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let runner = ProcessGitRunner::in_directory(&dir);
        runner.run(&["init", "-q"]).ok()?;
        Some(Self(dir))
    }

    fn runner(&self) -> ProcessGitRunner {
        ProcessGitRunner::in_directory(self.0.clone())
    }

    fn hooks_dir(&self) -> PathBuf {
        let output = self
            .runner()
            .run(&["rev-parse", "--git-path", "hooks"])
            .expect("git rev-parse works");
        assert_eq!(output.status, 0);
        self.0.join(output.stdout.trim())
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

fn read_file(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

#[cfg(unix)]
fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

fn parse_args(values: &[&str]) -> Result<command::Invocation, String> {
    command::parse(
        &values
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>(),
    )
}

fn install_in(
    repo: &TempRepo,
) -> Result<hooks::InstallOutcome, crate::branch_context::BranchContextError> {
    hooks::install_in(&repo.runner(), &repo.0)
}

fn run_prepare(
    repo: &TempRepo,
    file: &Path,
    source: Option<&str>,
) -> Result<serde_json::Value, crate::branch_context::BranchContextError> {
    hooks::run_with(
        &repo.runner(),
        HookKind::PrepareCommitMsg,
        file.to_str().unwrap(),
        source,
    )
}

fn run_commit(
    repo: &TempRepo,
    file: &Path,
) -> Result<serde_json::Value, crate::branch_context::BranchContextError> {
    hooks::run_with(
        &repo.runner(),
        HookKind::CommitMsg,
        file.to_str().unwrap(),
        None,
    )
}

// ---------------------------------------------------------------------------
// Parser shapes for the hidden `hooks run` forms.
// ---------------------------------------------------------------------------

#[test]
fn hooks_run_parses_without_role_for_generated_scripts() {
    let invocation =
        parse_args(&["hooks", "run", "commit-msg", ".git/COMMIT_EDITMSG"]).expect("parses");
    match invocation.command {
        Command::Hooks(HooksCommand::Run {
            hook,
            message_file,
            source,
        }) => {
            assert_eq!(hook, HookKind::CommitMsg);
            assert_eq!(message_file, ".git/COMMIT_EDITMSG");
            assert_eq!(source, None);
            assert_eq!(invocation.role, None);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let invocation = parse_args(&[
        "hooks",
        "run",
        "prepare-commit-msg",
        ".git/COMMIT_EDITMSG",
        "message",
    ])
    .expect("prepare form parses");
    match invocation.command {
        Command::Hooks(HooksCommand::Run { hook, source, .. }) => {
            assert_eq!(hook, HookKind::PrepareCommitMsg);
            assert_eq!(source.as_deref(), Some("message"));
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn hooks_install_parses_without_role_for_local_only_workflow() {
    let invocation = parse_args(&["hooks", "install"]).expect("install parses without a role");
    match invocation.command {
        Command::Hooks(HooksCommand::Install) => {}
        other => panic!("unexpected command: {other:?}"),
    }
    assert_eq!(invocation.role, None);
}

#[test]
fn hooks_run_rejects_wrong_argument_counts_and_targets() {
    for args in [
        vec!["hooks", "run"],
        vec!["hooks", "run", "pre-commit", "file"],
        vec!["hooks", "run", "commit-msg"],
        vec!["hooks", "run", "commit-msg", "file", "extra"],
        vec!["hooks", "run", "prepare-commit-msg"],
        vec!["hooks", "run", "prepare-commit-msg", "a", "b", "c"],
    ] {
        assert!(parse_args(&args).is_err(), "hooks run accepted {args:?}");
    }
}

#[test]
fn hooks_run_help_still_reaches_the_generic_help_path() {
    let invocation = parse_args(&["hooks", "run", "--help"]).expect("help parses");
    assert!(matches!(
        invocation.command,
        Command::Help(crate::command::HelpTopic::HooksCommand(_))
    ));
}

// ---------------------------------------------------------------------------
// Script rendering invariants.
// ---------------------------------------------------------------------------

#[test]
fn generated_scripts_are_selfcontained_and_path_free() {
    let script = hooks::render_script_for_tests(HookKind::PrepareCommitMsg, None);
    assert!(script.starts_with("#!/bin/sh\n"));
    assert!(script.contains(MANAGED_MARKER));
    assert!(script.contains("exec phasegent hooks run prepare-commit-msg \"$@\""));
    // No absolute binary path may be baked in.
    assert!(!script.contains("/usr/"));
    assert!(!script.contains("/home/"));

    let chained = hooks::render_script_for_tests(
        HookKind::CommitMsg,
        Some(Path::new("/repo/.git/hooks/phasegent-original/commit-msg")),
    );
    assert!(chained.contains("PHASEGENT_ORIGINAL_HOOK="));
    assert!(chained.contains("\"$PHASEGENT_ORIGINAL_HOOK\" \"$@\" || exit $?"));
    assert!(chained.contains("exec phasegent hooks run commit-msg \"$@\""));
}

// ---------------------------------------------------------------------------
// Installation against real temp repositories.
// ---------------------------------------------------------------------------

#[test]
fn install_fresh_repo_writes_executable_managed_scripts() {
    let Some(repo) = TempRepo::new("fresh") else {
        return;
    };
    let outcome = install_in(&repo).expect("install succeeds");
    assert_eq!(outcome.installed, vec!["prepare-commit-msg", "commit-msg"]);
    assert!(outcome.updated.is_empty());

    let hooks_dir = repo.hooks_dir();
    for name in ["prepare-commit-msg", "commit-msg"] {
        let contents = read_file(&hooks_dir.join(name));
        let text = String::from_utf8(contents).unwrap();
        assert!(text.contains(MANAGED_MARKER), "{name} lacks marker");
        assert!(
            text.contains(&format!("phasegent hooks run {name} \"$@\"")),
            "{name} does not call back into phasegent"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(hooks_dir.join(name))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "{name} is not executable");
        }
    }
    assert!(
        !repo.hooks_dir().join(ORIGINAL_BACKUP_DIR).exists(),
        "no backup directory on a fresh install"
    );
}

#[test]
fn install_is_idempotent_for_managed_hooks() {
    let Some(repo) = TempRepo::new("idempotent") else {
        return;
    };
    install_in(&repo).unwrap();
    let hooks_dir = repo.hooks_dir();
    let before = read_file(&hooks_dir.join("commit-msg"));

    let outcome = install_in(&repo).unwrap();
    assert!(outcome.installed.is_empty());
    assert_eq!(outcome.updated, vec!["prepare-commit-msg", "commit-msg"]);
    assert_eq!(read_file(&hooks_dir.join("commit-msg")), before);
    assert!(!hooks_dir.join(ORIGINAL_BACKUP_DIR).exists());
}

#[test]
fn install_backs_up_foreign_hook_and_chains_without_nesting() {
    let Some(repo) = TempRepo::new("foreign") else {
        return;
    };
    let hooks_dir = repo.hooks_dir();
    let foreign_path = hooks_dir.join("prepare-commit-msg");
    write_file(&foreign_path, "#!/bin/sh\necho foreign-hook\n");
    #[cfg(unix)]
    set_executable(&foreign_path);

    let outcome = install_in(&repo).expect("install succeeds");
    assert_eq!(outcome.installed, vec!["prepare-commit-msg"]);
    assert!(!outcome.warnings.is_empty(), "displacement is reported");

    let backup = hooks_dir
        .join(ORIGINAL_BACKUP_DIR)
        .join("prepare-commit-msg");
    let backup_contents = read_file(&backup);
    assert_eq!(backup_contents, b"#!/bin/sh\necho foreign-hook\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&backup).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "backup preserves the executable mode");
    }

    let managed = String::from_utf8(read_file(&foreign_path)).unwrap();
    assert!(managed.contains(MANAGED_MARKER));
    assert!(managed.contains(ORIGINAL_BACKUP_DIR));
    assert!(managed.contains("\"$PHASEGENT_ORIGINAL_HOOK\" \"$@\" || exit $?"));
    // commit-msg had no foreign predecessor and stays a plain wrapper.
    let plain = String::from_utf8(read_file(&hooks_dir.join("commit-msg"))).unwrap();
    assert!(!plain.contains(ORIGINAL_BACKUP_DIR));

    // Repeated installs must not overwrite the backup or nest wrappers.
    let outcome = install_in(&repo).unwrap();
    assert!(outcome.installed.is_empty());
    assert_eq!(outcome.updated.len(), 2);
    assert_eq!(read_file(&backup), backup_contents);
    assert_eq!(
        String::from_utf8(read_file(&foreign_path)).unwrap(),
        managed
    );
}

#[test]
fn install_refuses_symlinked_hook_path() {
    let Some(repo) = TempRepo::new("symlink") else {
        return;
    };
    let hooks_dir = repo.hooks_dir();
    let target = repo.0.join("outside-hook.sh");
    write_file(&target, "#!/bin/sh\nexit 0\n");
    let link = hooks_dir.join("commit-msg");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let error = install_in(&repo).expect_err("symlinked hook must be refused");
    assert_eq!(error.kind, "conflict");
    assert!(error.message.contains("symlink"), "{}", error.message);
    // The symlink target is untouched.
    assert_eq!(read_file(&target), b"#!/bin/sh\nexit 0\n");
}

#[test]
fn install_refuses_to_clobber_foreign_hook_when_backup_already_exists() {
    let Some(repo) = TempRepo::new("backup-conflict") else {
        return;
    };
    let hooks_dir = repo.hooks_dir();
    let backup_dir = hooks_dir.join(ORIGINAL_BACKUP_DIR);
    std::fs::create_dir_all(&backup_dir).unwrap();
    write_file(&backup_dir.join("commit-msg"), "#!/bin/sh\necho precious\n");
    // A foreign hook currently lives at the managed path; this is the
    // restore-then-reinstall shape where overwriting would lose its bytes.
    let foreign_path = hooks_dir.join("commit-msg");
    write_file(&foreign_path, "#!/bin/sh\necho current\n");
    #[cfg(unix)]
    set_executable(&foreign_path);

    let error = install_in(&repo).expect_err("conflicting backup must fail installation");
    assert_eq!(error.kind, "conflict");
    assert!(
        error.message.contains("already preserved"),
        "{}",
        error.message
    );

    // The backup is untouched and the live foreign hook survived
    // byte-for-byte, including its executable mode; nothing was managed.
    assert_eq!(
        read_file(&backup_dir.join("commit-msg")),
        b"#!/bin/sh\necho precious\n"
    );
    assert_eq!(read_file(&foreign_path), b"#!/bin/sh\necho current\n");
    let text = String::from_utf8(read_file(&foreign_path)).unwrap();
    assert!(!text.contains(MANAGED_MARKER), "hook must stay foreign");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&foreign_path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "live foreign hook keeps its mode");
    }
}

// ---------------------------------------------------------------------------
// prepare-commit-msg behavior.
// ---------------------------------------------------------------------------

fn bound_repo(tag: &str, issue_id: u64) -> Option<TempRepo> {
    let repo = TempRepo::new(tag)?;
    branch_context::bind(&repo.runner(), issue_id, false).unwrap();
    Some(repo)
}

#[test]
fn prepare_appends_exactly_one_refs_trailer() {
    let Some(repo) = bound_repo("append", 23) else {
        return;
    };
    let file = repo.0.join("COMMIT_MSG");
    write_file(&file, "Fix login validation\n");

    let value = run_prepare(&repo, &file, Some("")).unwrap();
    assert_eq!(value["action"], "appended");
    assert_eq!(read_file(&file), b"Fix login validation\n\nRefs #23\n");

    // A second run sees the existing reference and never duplicates it.
    let value = run_prepare(&repo, &file, None).unwrap();
    assert_eq!(value["action"], "noop");
    assert_eq!(read_file(&file), b"Fix login validation\n\nRefs #23\n");
}

#[test]
fn prepare_skips_git_generated_sources() {
    let Some(repo) = bound_repo("skip-sources", 23) else {
        return;
    };
    let file = repo.0.join("MERGE_MSG");
    write_file(&file, "Merge branch 'feature'\n");
    for source in ["merge", "squash", "commit"] {
        let value = run_prepare(&repo, &file, Some(source)).unwrap();
        assert_eq!(value["action"], "noop", "source {source}");
        assert_eq!(read_file(&file), b"Merge branch 'feature'\n");
    }
}

#[test]
fn prepare_nops_without_binding_or_existing_reference() {
    let Some(repo) = TempRepo::new("unbound") else {
        return;
    };
    let file = repo.0.join("COMMIT_MSG");
    write_file(&file, "Plain work\n");
    let value = run_prepare(&repo, &file, None).unwrap();
    assert_eq!(value["action"], "noop");
    assert_eq!(read_file(&file), b"Plain work\n");

    // Any existing reference token suppresses appending, whatever the ID.
    write_file(&file, "Work\n\nfixes #99\n");
    let value = run_prepare(&repo, &file, Some("template")).unwrap();
    assert_eq!(value["action"], "noop");
    assert_eq!(read_file(&file), b"Work\n\nfixes #99\n");
}

#[test]
fn prepare_treats_detached_head_as_unbound_noop() {
    let Some(repo) = TempRepo::new("detached-run") else {
        return;
    };
    let runner = repo.runner();
    runner
        .run(&[
            "-c",
            "user.name=phasegent-test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "init",
        ])
        .unwrap();
    runner.run(&["checkout", "-q", "--detach"]).unwrap();
    let file = repo.0.join("COMMIT_MSG");
    write_file(&file, "On detached HEAD\n");
    let value = run_prepare(&repo, &file, None).unwrap();
    assert_eq!(value["action"], "noop");
    assert_eq!(read_file(&file), b"On detached HEAD\n");
}

#[test]
fn prepare_rejects_unknown_source_and_missing_file() {
    let Some(repo) = bound_repo("arg-errors", 23) else {
        return;
    };
    let error = run_prepare(&repo, &repo.0.join("COMMIT_MSG"), Some("amend"))
        .expect_err("unknown source rejected");
    assert_eq!(error.kind, "argument");

    let error = run_prepare(&repo, &repo.0.join("does-not-exist"), None)
        .expect_err("missing file rejected");
    assert_eq!(error.kind, "argument");
    assert!(error.message.contains("missing or unreadable"));
}

// ---------------------------------------------------------------------------
// commit-msg behavior.
// ---------------------------------------------------------------------------

#[test]
fn commit_accepts_bound_reference_and_body_mentions() {
    let Some(repo) = bound_repo("commit-ok", 23) else {
        return;
    };
    let file = repo.0.join("COMMIT_MSG");

    write_file(&file, "Add feature\n\nRefs #23\n");
    let value = run_commit(&repo, &file).unwrap();
    assert_eq!(value["action"], "valid");

    // Free-form body mentions of other numbers are not Redmine tokens.
    write_file(
        &file,
        "See discussion #24 and ticket #25 for context.\n\nRefs #23\n",
    );
    let value = run_commit(&repo, &file).unwrap();
    assert_eq!(value["action"], "valid");

    // Different keyword spellings of the bound ID are fine.
    write_file(&file, "FIXES #23\n");
    let value = run_commit(&repo, &file).unwrap();
    assert_eq!(value["action"], "valid");
}

#[test]
fn commit_rejects_conflicting_issue_references_without_rewriting() {
    let Some(repo) = bound_repo("commit-conflict", 23) else {
        return;
    };
    let file = repo.0.join("COMMIT_MSG");
    write_file(&file, "Wrong branch\n\nRefs #24\n");
    let error = run_commit(&repo, &file).expect_err("conflicting reference rejected");
    assert_eq!(error.kind, "conflict");
    assert!(error.message.contains('2'), "{}", error.message);
    assert!(error.message.contains("4"), "{}", error.message);
    // The message file is never rewritten by commit-msg.
    assert_eq!(read_file(&file), b"Wrong branch\n\nRefs #24\n");
}

#[test]
fn commit_rejects_duplicate_generated_trailers_but_not_mentions() {
    let Some(repo) = bound_repo("commit-dup", 23) else {
        return;
    };
    let file = repo.0.join("COMMIT_MSG");
    write_file(&file, "Subject\n\nRefs #23\n\nRefs #23\n");
    let error = run_commit(&repo, &file).expect_err("duplicate trailer rejected");
    assert_eq!(error.kind, "conflict");
    assert!(error.message.contains("exactly one"), "{}", error.message);

    // Two different keyword forms pointing at the same issue are body
    // mentions, not duplicated generated trailers.
    write_file(&file, "Subject\n\nRefs #23\nAlso fixes #23 in passing.\n");
    let value = run_commit(&repo, &file).unwrap();
    assert_eq!(value["action"], "valid");
}

#[test]
fn commit_nops_without_binding() {
    let Some(repo) = TempRepo::new("commit-unbound") else {
        return;
    };
    let file = repo.0.join("COMMIT_MSG");
    write_file(&file, "Anything goes\n\nRefs #77\n");
    let value = run_commit(&repo, &file).unwrap();
    assert_eq!(value["action"], "noop");
    assert_eq!(read_file(&file), b"Anything goes\n\nRefs #77\n");
}
