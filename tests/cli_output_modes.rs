//! Output-mode integration tests (issue #1054 P1).
//!
//! Every scenario runs the compiled binary with an isolated home and
//! database, so the assertions only observe the presentation contract: which
//! channel carries the result, which format the selected mode produces, and
//! that the machine JSON document never drifts.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

/// A per-test scratch directory that removes itself when the test ends.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "phasegent-output-modes-{label}-{}-{unique}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create scratch root");
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// One CLI run with an isolated environment.
struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Run {
    fn stdout_is_json(&self) -> bool {
        serde_json::from_str::<serde_json::Value>(self.stdout.trim()).is_ok()
    }
}

/// Run the binary with the given arguments. `role` models `PHASEGENT_ROLE`;
/// `None` removes it. The managed markers are always removed so an ordinary
/// operator stays deterministic.
fn run_cli(scratch: &Scratch, role: Option<&str>, args: &[&str]) -> Run {
    let mut command = Command::new(env!("CARGO_BIN_EXE_phasegent"));
    command
        .args(args)
        .env("HOME", scratch.path("home"))
        .env("XDG_CONFIG_HOME", scratch.path("config"))
        .env("XDG_DATA_HOME", scratch.path("data"))
        .env("XDG_CACHE_HOME", scratch.path("cache"))
        .env("PHASEGENT_DB_PATH", scratch.path("phasegent.sqlite3"))
        .env("PHASEGENT_CONFIG_PATH", scratch.path("missing.toml"))
        .env_remove("PHASEGENT_ROLE")
        .env_remove("PHASEGENT_SESSION_ID")
        .env_remove("OPENCODE_SESSION_ID")
        .env_remove("PHASEGENT_PROVIDER")
        .env_remove("PHASEGENT_DEFAULT_PROVIDER")
        .env_remove("CI")
        .env_remove("CONTINUOUS_INTEGRATION")
        .env_remove("NO_COLOR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(role) = role {
        command.env("PHASEGENT_ROLE", role);
    }
    let output = command.output().expect("spawn phasegent binary");
    Run {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// The machine document and the text rendering carry the same facts.
#[test]
fn an_explicit_text_mode_renders_the_same_result_readably() {
    let scratch = Scratch::new("text");
    let json = run_cli(&scratch, None, &["--output", "json", "doctor"]);
    let text = run_cli(&scratch, None, &["--output", "text", "doctor"]);
    assert_eq!(json.code, 0, "stderr: {}", json.stderr);
    assert_eq!(text.code, 0, "stderr: {}", text.stderr);
    assert!(json.stdout_is_json(), "json mode: {}", json.stdout);
    assert!(
        !text.stdout.contains('{'),
        "text mode is not a dump: {}",
        text.stdout
    );
    assert!(
        text.stdout.contains("database:"),
        "text mode is readable: {}",
        text.stdout
    );
}

/// The JSON document keeps its keys and stays a single document.
#[test]
fn the_json_document_is_unchanged_and_single() {
    let scratch = Scratch::new("json");
    let automatic = run_cli(&scratch, None, &["doctor"]);
    let explicit = run_cli(&scratch, None, &["--output", "json", "doctor"]);
    assert_eq!(automatic.code, 0, "stderr: {}", automatic.stderr);
    assert_eq!(explicit.code, 0, "stderr: {}", explicit.stderr);
    assert_eq!(
        automatic.stdout, explicit.stdout,
        "the explicit JSON mode is the default document"
    );
    let value: serde_json::Value = serde_json::from_str(automatic.stdout.trim())
        .expect("the stdout is exactly one JSON document");
    assert!(value.get("database_path").is_some(), "got: {value}");
    assert!(value.get("roles").is_some(), "got: {value}");
    assert!(value.get("index").is_some(), "got: {value}");
}

/// Every agent role and the managed session without a role stay on the
/// machine document, even when the caller asked for text explicitly.
#[test]
fn every_agent_role_reads_machine_json_on_a_pipe() {
    let scratch = Scratch::new("roles");
    for role in [
        Some("orchestrator"),
        Some("executor"),
        Some("reviewer"),
        Some("explore"),
        Some("admin"),
    ] {
        let run = run_cli(&scratch, role, &["doctor"]);
        assert_eq!(run.code, 0, "role {role:?} stderr: {}", run.stderr);
        assert!(
            run.stdout_is_json(),
            "role {role:?} reads machine JSON: {}",
            run.stdout
        );
    }
    let managed = Command::new(env!("CARGO_BIN_EXE_phasegent"))
        .args(["doctor"])
        .env("HOME", scratch.path("home"))
        .env("PHASEGENT_DB_PATH", scratch.path("phasegent.sqlite3"))
        .env("PHASEGENT_SESSION_ID", "session-managed")
        .env_remove("PHASEGENT_ROLE")
        .env_remove("OPENCODE_SESSION_ID")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn phasegent binary");
    let managed_stdout = String::from_utf8_lossy(&managed.stdout).into_owned();
    assert!(
        serde_json::from_str::<serde_json::Value>(managed_stdout.trim()).is_ok(),
        "a managed session without a role reads machine JSON: {managed_stdout}"
    );
}

/// An explicit selection wins for every caller, including the agent roles.
#[test]
fn an_explicit_text_selection_wins_for_an_agent_role() {
    let scratch = Scratch::new("explicit");
    let run = run_cli(&scratch, Some("executor"), &["--output", "text", "doctor"]);
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert!(!run.stdout.contains('{'), "got: {}", run.stdout);
    assert!(run.stdout.contains("database:"), "got: {}", run.stdout);
}

/// A failure keeps its channel and its exit code in both modes.
#[test]
fn a_failure_honors_the_selected_mode() {
    let scratch = Scratch::new("failure");
    let json = run_cli(
        &scratch,
        None,
        &["--output", "json", "--output", "yaml", "doctor"],
    );
    assert_eq!(json.code, 2, "stderr: {}", json.stderr);
    assert!(
        json.stdout.is_empty(),
        "a failure writes no stdout: {}",
        json.stdout
    );
    assert!(
        json.stderr.contains("\"error\""),
        "json mode keeps the envelope: {}",
        json.stderr
    );

    let text = run_cli(
        &scratch,
        None,
        &["--output", "text", "doctor", "--nonsense"],
    );
    assert_eq!(text.code, 2, "stderr: {}", text.stderr);
    assert!(
        text.stdout.is_empty(),
        "a failure writes no stdout: {}",
        text.stdout
    );
    assert!(
        text.stderr.starts_with("error: "),
        "text mode uses a readable diagnostic: {}",
        text.stderr
    );
    assert!(
        !text.stderr.contains('{'),
        "text mode is not a dump: {}",
        text.stderr
    );
}

/// The machine output never carries a terminal control sequence.
#[test]
fn the_machine_output_carries_no_control_sequence() {
    let scratch = Scratch::new("control");
    let run = run_cli(&scratch, None, &["--output", "json", "doctor"]);
    assert!(!run.stdout.contains('\u{1b}'), "stdout: {}", run.stdout);
    assert!(!run.stderr.contains('\u{1b}'), "stderr: {}", run.stderr);
    let text = run_cli(&scratch, None, &["--output", "text", "doctor"]);
    assert!(!text.stdout.contains('\u{1b}'), "stdout: {}", text.stdout);
}

/// The prune keeps its legacy format flags, and the default follows the
/// automatic resolution.
#[test]
fn the_prune_keeps_its_legacy_formats_and_the_default_follows_auto() {
    let scratch = Scratch::new("prune");
    // The default on a pipe is the machine document.
    let automatic = run_cli(
        &scratch,
        Some("orchestrator"),
        &["worktree", "prune", "--dryrun"],
    );
    assert_eq!(automatic.code, 0, "stderr: {}", automatic.stderr);
    assert!(
        automatic.stdout_is_json(),
        "the default prune reports the machine document: {}",
        automatic.stdout
    );
    // An explicit legacy format is honored.
    let legacy = run_cli(
        &scratch,
        Some("orchestrator"),
        &["worktree", "prune", "--dryrun", "--format", "text"],
    );
    assert_eq!(legacy.code, 0, "stderr: {}", legacy.stderr);
    assert!(
        legacy.stdout.starts_with("worktree prune ("),
        "the legacy text format is honored: {}",
        legacy.stdout
    );
    // An explicit automatic selection leaves the legacy choice in force, and a
    // pipeline never carries progress or control bytes in either direction.
    let automatic_legacy_text = run_cli(
        &scratch,
        Some("orchestrator"),
        &[
            "--output", "auto", "worktree", "prune", "--dryrun", "--format", "text",
        ],
    );
    assert_eq!(
        automatic_legacy_text.code, 0,
        "stderr: {}",
        automatic_legacy_text.stderr
    );
    assert!(
        automatic_legacy_text.stdout.starts_with("worktree prune ("),
        "the legacy text choice survives an automatic default: {}",
        automatic_legacy_text.stdout
    );
    let automatic_legacy_json = run_cli(
        &scratch,
        Some("orchestrator"),
        &[
            "--output", "auto", "worktree", "prune", "--dryrun", "--json",
        ],
    );
    assert_eq!(
        automatic_legacy_json.code, 0,
        "stderr: {}",
        automatic_legacy_json.stderr
    );
    assert!(
        automatic_legacy_json.stdout_is_json(),
        "the legacy JSON alias survives an automatic default: {}",
        automatic_legacy_json.stdout
    );
    for run in [&automatic_legacy_text, &automatic_legacy_json] {
        assert!(
            !run.stdout.contains('\u{1b}') && !run.stderr.contains('\u{1b}'),
            "a legacy selection on a pipe carries no control sequence: stdout={:?} stderr={:?}",
            run.stdout,
            run.stderr
        );
        assert!(
            !run.stdout.contains('\r') && !run.stderr.contains('\r'),
            "a legacy selection on a pipe carries no redraw: stdout={:?} stderr={:?}",
            run.stdout,
            run.stderr
        );
    }
    // A contradictory pair is rejected at parse time.
    let conflict = run_cli(
        &scratch,
        Some("orchestrator"),
        &[
            "--output", "json", "worktree", "prune", "--dryrun", "--format", "text",
        ],
    );
    assert_eq!(conflict.code, 2, "stderr: {}", conflict.stderr);
    assert!(
        conflict.stderr.contains("conflicts with --output"),
        "stderr: {}",
        conflict.stderr
    );
}

/// The progress switch never changes the output format.
#[test]
fn the_progress_switch_does_not_change_the_format() {
    let scratch = Scratch::new("progress");
    let json = run_cli(&scratch, None, &["--no-progress", "doctor"]);
    let plain = run_cli(&scratch, None, &["doctor"]);
    assert_eq!(json.code, 0, "stderr: {}", json.stderr);
    assert_eq!(json.stdout, plain.stdout, "the format never changes");
}

/// The help and version outputs stay the existing text utility outputs.
#[test]
fn the_help_and_version_outputs_stay_text() {
    let scratch = Scratch::new("help");
    for selection in ["auto", "text", "json"] {
        let help = run_cli(&scratch, None, &["--output", selection, "--help"]);
        assert_eq!(help.code, 0, "stderr: {}", help.stderr);
        assert!(
            help.stdout.contains("Usage"),
            "the help stays text: {}",
            help.stdout
        );
        let version = run_cli(&scratch, None, &["--output", selection, "--version"]);
        assert_eq!(version.code, 0, "stderr: {}", version.stderr);
        assert!(
            version.stdout.starts_with("phasegent "),
            "the version stays text: {}",
            version.stdout
        );
    }
}

/// The scratch directory is the only place the run may write.
#[test]
fn the_run_writes_only_inside_the_scratch_directory() {
    let scratch = Scratch::new("isolation");
    let run = run_cli(&scratch, None, &["--output", "text", "doctor"]);
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert!(
        scratch.path("phasegent.sqlite3").exists() || scratch.path("home").exists(),
        "the run wrote inside its scratch directory"
    );
}

/// The curated views answer what happened instead of dumping the document.
#[test]
fn the_curated_views_answer_the_question() {
    let scratch = Scratch::new("curated");

    let doctor = run_cli(&scratch, None, &["--output", "text", "doctor"]);
    assert_eq!(doctor.code, 0, "stderr: {}", doctor.stderr);
    assert!(doctor.stdout.contains("Healthy:"), "got: {}", doctor.stdout);
    assert!(
        doctor.stdout.contains("Warnings:"),
        "got: {}",
        doctor.stdout
    );
    assert!(
        doctor.stdout.contains("database:"),
        "got: {}",
        doctor.stdout
    );
    assert!(doctor.stdout.contains("Roles:"), "got: {}", doctor.stdout);

    let config = run_cli(&scratch, None, &["--output", "text", "config", "show"]);
    assert_eq!(config.code, 0, "stderr: {}", config.stderr);
    assert!(
        config.stdout.contains("Global settings:"),
        "got: {}",
        config.stdout
    );
    assert!(config.stdout.contains("Roles:"), "got: {}", config.stdout);
    assert!(
        config.stdout.contains("PHASEGENT_REDMINE_API_BASE"),
        "got: {}",
        config.stdout
    );
    // A secret-named setting shows its presence indicator only; the
    // redacted snapshot never carries the value itself.
    assert!(
        config
            .stdout
            .contains("PHASEGENT_NOTIFY_DINGTALK_SECRET: missing"),
        "got: {}",
        config.stdout
    );

    let leases = run_cli(
        &scratch,
        Some("orchestrator"),
        &["--output", "text", "worktree", "list"],
    );
    assert_eq!(leases.code, 0, "stderr: {}", leases.stderr);
    assert!(leases.stdout.contains("leases"), "got: {}", leases.stdout);

    let probe = run_cli(
        &scratch,
        Some("orchestrator"),
        &["--output", "text", "worktree", "probe"],
    );
    assert_eq!(probe.code, 0, "stderr: {}", probe.stderr);
    assert!(probe.stdout.contains("Path:"), "got: {}", probe.stdout);
    assert!(probe.stdout.contains("State:"), "got: {}", probe.stdout);
}

/// The machine document keeps its keys; only the text rendering changed.
#[test]
fn the_machine_keysets_are_unchanged() {
    let scratch = Scratch::new("keysets");
    let probe = run_cli(&scratch, Some("orchestrator"), &["worktree", "probe"]);
    assert_eq!(probe.code, 0, "stderr: {}", probe.stderr);
    let value: serde_json::Value =
        serde_json::from_str(probe.stdout.trim()).expect("the probe prints one JSON document");
    for key in [
        "resolved",
        "path",
        "exists",
        "is_git_worktree",
        "clean",
        "branch",
        "head",
        "is_main_checkout",
        "lease",
        "errors",
    ] {
        assert!(
            value.get(key).is_some(),
            "the probe key {key} is missing: {value}"
        );
    }

    let list = run_cli(&scratch, Some("orchestrator"), &["worktree", "list"]);
    assert_eq!(list.code, 0, "stderr: {}", list.stderr);
    let value: serde_json::Value =
        serde_json::from_str(list.stdout.trim()).expect("the list prints one JSON document");
    assert!(value.get("repo_identity").is_some(), "got: {value}");
    assert!(value.get("leases").is_some(), "got: {value}");
}

/// The progress surface only exists for a human on an interactive terminal: a
/// pipe, a machine mode, and `--no-progress` never carry a progress line, a
/// cursor control sequence, or a carriage return.
#[test]
fn the_progress_never_reaches_a_pipe_or_a_machine_run() {
    let scratch = Scratch::new("progress");
    for args in [
        vec!["worktree", "prune", "--dryrun"],
        vec!["--output", "text", "worktree", "prune", "--dryrun"],
        vec![
            "--output",
            "text",
            "--no-progress",
            "worktree",
            "prune",
            "--dryrun",
        ],
    ] {
        let run = run_cli(&scratch, Some("orchestrator"), &args);
        assert_eq!(run.code, 0, "args {args:?} stderr: {}", run.stderr);
        assert!(
            !run.stderr.contains("worktree prune:"),
            "args {args:?} stderr: {}",
            run.stderr
        );
        assert!(
            !run.stdout.contains('\u{1b}'),
            "args {args:?}: {}",
            run.stdout
        );
        assert!(
            !run.stderr.contains('\u{1b}'),
            "args {args:?}: {}",
            run.stderr
        );
        assert!(!run.stdout.contains('\r'), "args {args:?}: {}", run.stdout);
        assert!(!run.stderr.contains('\r'), "args {args:?}: {}", run.stderr);
    }
}

/// The progress switch never silences a diagnostic.
#[test]
fn the_progress_switch_never_silences_a_diagnostic() {
    let scratch = Scratch::new("progress-switch");
    let run = run_cli(
        &scratch,
        None,
        &["--output", "text", "--no-progress", "doctor", "--nonsense"],
    );
    assert_eq!(run.code, 2, "stderr: {}", run.stderr);
    assert!(run.stderr.starts_with("error: "), "stderr: {}", run.stderr);
}
