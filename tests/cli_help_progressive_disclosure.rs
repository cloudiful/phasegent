//! Black-box regression coverage for the root and nested help
//! surface. The contract is intentionally small: root help must be
//! short and task-oriented, must not duplicate the deep resolver or
//! role/security essays, and must point operators at the canonical
//! nested help pages. Deep help pages must continue to carry the
//! details they own (`config provider` for the resolver chain,
//! `auth` for the role/security guidance, `workflow bootstrap` for
//! `--close-status-name`). Routing must remain intact so that every
//! documented pointer resolves to a non-error page.

#[path = "support/mod.rs"]
mod support;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use support::{phasegent_bin, stdout_text};

/// Per-test scratch directory holding the throwaway SQLite the help
/// commands would otherwise touch. Help-only invocations never open
/// the database, but the runner pins `PHASEGENT_DB_PATH` so the test
/// environment matches production isolation rules.
struct ScratchDb {
    dir: PathBuf,
}

impl Drop for ScratchDb {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

impl ScratchDb {
    fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "phasegent-it-help-{}-{}-{}",
            std::process::id(),
            nanos,
            (nanos as u64) ^ (std::process::id() as u64),
        ));
        fs::create_dir_all(&dir).expect("create scratch dir");
        Self { dir }
    }

    fn path(&self) -> &std::path::Path {
        &self.dir
    }
}

fn run_help(args: &[&str]) -> Output {
    let db = ScratchDb::new();
    let mut command = Command::new(phasegent_bin());
    command
        .args(args)
        .env("PHASEGENT_DB_PATH", db.path().as_os_str())
        .env_remove("PHASEGENT_PROVIDER")
        .env_remove("PHASEGENT_DEFAULT_PROVIDER")
        .env_remove("PHASEGENT_API_BASE")
        .env_remove("PHASEGENT_REDMINE_API_BASE")
        .env_remove("PHASEGENT_REPOSITORY")
        .env_remove("PHASEGENT_PROJECT_ID")
        .env_remove("PHASEGENT_REDMINE_PROJECT_ID")
        .env_remove("PHASEGENT_CLOSE_STATUS_ID")
        .env_remove("PHASEGENT_REDMINE_CLOSE_STATUS_ID")
        .env_remove("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY")
        .env_remove("PHASEGENT_REDMINE_REPOSITORY_URL")
        .env("RUST_BACKTRACE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.output().expect("spawn phasegent binary")
}

/// Root help must be short, must not leak the resolver chain or the
/// role disclaimer, and must not advertise bootstrap-only flags.
#[test]
fn root_help_is_short_and_points_at_deep_pages() {
    let output = run_help(&["--help"]);
    assert!(output.status.success(), "--help exited non-zero");
    let stdout = stdout_text(&output);

    assert!(
        !stdout.contains("Provider resolution precedence"),
        "root help must not embed the resolver chain; got:\n{stdout}",
    );
    assert!(
        !stdout.contains("Role selects a capability policy"),
        "root help must not repeat the role disclaimer; got:\n{stdout}",
    );
    assert!(
        !stdout.contains("--close-status-name"),
        "root help must not advertise bootstrap-only --close-status-name; got:\n{stdout}",
    );

    // Visible command index entries that operators rely on for one-glance routing.
    for command in [
        "issue",
        "comment",
        "auth",
        "config",
        "hooks",
        "--help <command>",
    ] {
        assert!(
            stdout.contains(command),
            "root help missing command/pointer {command:?}; got:\n{stdout}",
        );
    }

    // Canonical next-help pointers to the deep pages that now own the
    // detail previously duplicated at root.
    assert!(
        stdout.contains("phasegent --help config provider"),
        "root help must point operators at the resolver chain page; got:\n{stdout}",
    );
    assert!(
        stdout.contains("phasegent --help auth"),
        "root help must point operators at the role/credential page; got:\n{stdout}",
    );

    // Universal global options stay advertised at root.
    for option in [
        "--role",
        "--provider",
        "--api-base",
        "--repository",
        "--project-id",
        "--close-status-id",
        "-h, --help",
        "-V, --version",
    ] {
        assert!(
            stdout.contains(option),
            "root help missing global option {option:?}; got:\n{stdout}",
        );
    }
}

/// Root help rendered with a Redmine provider filter must still
/// satisfy the contract and additionally expose the Redmine-scoped
/// commands without re-introducing the resolver or role disclaimer.
#[test]
fn root_help_remains_short_with_provider_filter() {
    let output = run_help(&["--provider", "redmine", "--help"]);
    assert!(
        output.status.success(),
        "--provider redmine --help exited non-zero"
    );
    let stdout = stdout_text(&output);

    assert!(
        !stdout.contains("Provider resolution precedence"),
        "filtered root help must not embed the resolver chain; got:\n{stdout}",
    );
    assert!(
        !stdout.contains("Role selects a capability policy"),
        "filtered root help must not repeat the role disclaimer; got:\n{stdout}",
    );
    assert!(
        !stdout.contains("--close-status-name"),
        "filtered root help must not advertise bootstrap-only --close-status-name; got:\n{stdout}",
    );

    for command in ["workflow", "timer", "status", "project"] {
        assert!(
            stdout.contains(command),
            "redmine-filtered root help missing {command:?}; got:\n{stdout}",
        );
    }
}

/// The resolver chain must still be reachable, exactly once, through
/// the canonical nested page. This is the deep help page that the
/// root pointer now points at.
#[test]
fn config_provider_help_carries_the_resolver_chain() {
    let output = run_help(&["--help", "config", "provider"]);
    assert!(
        output.status.success(),
        "--help config provider exited non-zero"
    );
    let stdout = stdout_text(&output);
    assert!(
        stdout.contains("PHASEGENT_PROVIDER")
            && stdout.contains("PHASEGENT_DEFAULT_PROVIDER")
            && stdout.contains("role_config.provider")
            && stdout.contains("forgejo fallback"),
        "config provider help must carry the resolver chain; got:\n{stdout}",
    );
}

/// The role/security guidance remains owned by `auth` and is no
/// longer duplicated at root.
#[test]
fn auth_help_carries_role_and_credential_guidance() {
    let output = run_help(&["--help", "auth"]);
    assert!(output.status.success(), "--help auth exited non-zero");
    let stdout = stdout_text(&output);
    assert!(
        stdout.contains("capability policy")
            && stdout.contains("least-privilege")
            && stdout.contains("never accepted as command-line arguments"),
        "auth help must carry the role/credential guidance; got:\n{stdout}",
    );
}

/// `--close-status-name` remains documented under the command that
/// actually accepts it.
#[test]
fn workflow_bootstrap_help_documents_close_status_name() {
    let output = run_help(&["--help", "workflow", "bootstrap"]);
    assert!(
        output.status.success(),
        "--help workflow bootstrap exited non-zero"
    );
    let stdout = stdout_text(&output);
    assert!(
        stdout.contains("--close-status-name"),
        "workflow bootstrap help must still document --close-status-name; got:\n{stdout}",
    );
    assert!(
        stdout.contains("--close-status-id"),
        "workflow bootstrap help must still document --close-status-id; got:\n{stdout}",
    );
}

/// Command routing must remain intact: every documented next-help
/// pointer resolves to a non-error page. This protects against
/// accidental renames in `command::HelpTopic` or `print_help`.
#[test]
fn documented_next_help_pointers_all_resolve() {
    let pointers: &[&[&str]] = &[
        &["--help", "issue"],
        &["--help", "comment"],
        &["--help", "auth"],
        &["--help", "config"],
        &["--help", "config", "provider"],
        &["--help", "config", "provider", "set"],
        &["--help", "hooks"],
        &["--help", "workflow", "bootstrap"],
    ];
    for pointer in pointers {
        let output = run_help(pointer);
        assert!(
            output.status.success(),
            "pointer {:?} exited non-zero: stderr={}",
            pointer,
            String::from_utf8_lossy(&output.stderr),
        );
        let stdout = stdout_text(&output);
        assert!(
            !stdout.trim().is_empty(),
            "pointer {:?} produced empty help",
            pointer,
        );
    }
}
