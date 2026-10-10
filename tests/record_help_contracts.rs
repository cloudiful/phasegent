//! Black-box help contract for the `record` command group.
//!
//! The record protocol is only usable if an agent can discover the fixed
//! command shape without a chat transcript: the group page, the create
//! page (kind binding, stable request key, `--authorized`, body-file
//! lifecycle), and the role gate that hides a denied page behind the
//! stable denial line all have to keep working.

// This shared fixture module serves several integration tests; this test
// intentionally uses only its binary and stdout helpers.
#[path = "support/mod.rs"]
mod support;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use support::{phasegent_bin, stdout_text};

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
        let dir = support::scratch_root().join(format!(
            "phasegent-it-record-help-{}-{}-{}",
            std::process::id(),
            nanos,
            (nanos as u64) ^ (std::process::id() as u64),
        ));
        fs::create_dir_all(&dir).expect("create scratch dir");
        Self { dir }
    }
}

fn run_help(role: Option<&str>, args: &[&str]) -> Output {
    let db = ScratchDb::new();
    let mut command = Command::new(phasegent_bin());
    command
        .args(args)
        .env("PHASEGENT_DB_PATH", db.dir.as_os_str())
        .env_remove("PHASEGENT_PROVIDER")
        .env_remove("PHASEGENT_DEFAULT_PROVIDER")
        .env_remove("PHASEGENT_ROLE")
        .env_remove("PHASEGENT_API_BASE")
        .env_remove("PHASEGENT_REDMINE_API_BASE")
        .env_remove("PHASEGENT_REPOSITORY")
        .env_remove("PHASEGENT_PROJECT_ID")
        .env_remove("PHASEGENT_REDMINE_PROJECT_ID")
        .env_remove("PHASEGENT_CLOSE_STATUS_ID")
        .env(
            "PHASEGENT_CONFIG_PATH",
            db.dir.join("missing.toml").as_os_str(),
        )
        .env("RUST_BACKTRACE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(role) = role {
        command.env("PHASEGENT_ROLE", role);
    }
    command.output().expect("spawn phasegent binary")
}

#[test]
fn record_group_lists_every_subcommand_and_points_at_the_detail_pages() {
    let stdout = stdout_text(&run_help(None, &["--help", "record"]));
    for row in ["create", "get", "list"] {
        assert!(
            stdout
                .lines()
                .any(|line| line.trim_start().starts_with(row)),
            "record help missing {row:?}:\n{stdout}"
        );
    }
    assert!(
        stdout.contains("phasegent --help record <command>"),
        "record help must point at the detail pages:\n{stdout}"
    );
}

#[test]
fn record_create_help_documents_the_fixed_contract() {
    let stdout = stdout_text(&run_help(None, &["--help", "record", "create"]));
    for needle in [
        "Usage: record create",
        "kind is bound to the session role",
        "--key is a stable request token",
        "--authorized",
        "--body-file",
        "conflict",
    ] {
        assert!(
            stdout.contains(needle),
            "record create help missing {needle:?}:\n{stdout}"
        );
    }
}

#[test]
fn record_get_and_list_help_document_usage() {
    let get = stdout_text(&run_help(None, &["--help", "record", "get"]));
    assert!(get.contains("Usage: record get"), "got:\n{get}");
    // issue #754 P3: the native reference is documented so a parent cites the
    // record id instead of transcribing the note.
    assert!(get.contains("#change-<id>"), "got:\n{get}");
    assert!(get.contains("#note-<id>"), "got:\n{get}");
    let list = stdout_text(&run_help(None, &["--help", "record", "list"]));
    assert!(list.contains("Usage: record list"), "got:\n{list}");
    // And the read-only recon path/pointer is documented on the list page.
    assert!(list.contains("recon record"), "got:\n{list}");
    assert!(
        list.contains("record id/url/key/provider/issue"),
        "got:\n{list}"
    );
}

#[test]
fn record_help_respects_the_role_gate() {
    // admin is never an agent role: the whole group and every page deny.
    for args in [
        &["--help", "record"][..],
        &["--help", "record", "create"][..],
        &["--help", "record", "get"][..],
    ] {
        let stdout = stdout_text(&run_help(Some("admin"), args));
        assert_eq!(
            stdout.trim_end(),
            "No command available for admin.",
            "{args:?} must deny admin:\n{stdout}"
        );
    }

    // A child agent role keeps the pages for its own kind and the read
    // surface; explore may publish recon, so it keeps `create` too.
    for role in ["executor", "reviewer", "orchestrator", "explore"] {
        let group = stdout_text(&run_help(Some(role), &["--help", "record"]));
        assert!(
            group.contains("create") && group.contains("get") && group.contains("list"),
            "record group for {role} must list every subcommand:\n{group}"
        );
        let create = stdout_text(&run_help(Some(role), &["--help", "record", "create"]));
        assert!(
            create.contains("Usage: record create"),
            "record create for {role} must render its page:\n{create}"
        );
    }
}

#[test]
fn an_unknown_record_help_topic_is_rejected() {
    let output = run_help(None, &["--help", "record", "frobnicate"]);
    assert!(
        !output.status.success(),
        "an unknown record help topic must be rejected"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unknown record help topic 'frobnicate'"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
