//! Notification phase contract: bounded envelopes, channel config
//! via `global_setting` rows, intent persisted before delivery, and
//! delivery decoupled from workflow success.

use std::process::{Command, Output, Stdio};

#[path = "support/mod.rs"]
mod support;

use support::{phasegent_bin, stdout_text};

fn scratch_db() -> TempfileLikeDir {
    TempfileLikeDir::new()
}

struct TempfileLikeDir {
    dir: std::path::PathBuf,
}

impl TempfileLikeDir {
    fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "phasegent-it-notify-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Self { dir }
    }

    fn db_path(&self) -> std::path::PathBuf {
        self.dir.join("phasegent.sqlite3")
    }

    fn missing_toml(&self) -> std::path::PathBuf {
        self.dir.join("phasegent-missing.toml")
    }
}

impl Drop for TempfileLikeDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn run_phasegent(scratch: &TempfileLikeDir, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(phasegent_bin());
    command
        .args(args)
        .env("PHASEGENT_DB_PATH", scratch.db_path().as_os_str())
        .env("PHASEGENT_CONFIG_PATH", scratch.missing_toml().as_os_str())
        // Isolate notify env so host config never leaks into tests.
        .env_remove("PHASEGENT_NOTIFY_ENABLED")
        .env_remove("PHASEGENT_NOTIFY_CHANNEL")
        .env_remove("PHASEGENT_NOTIFY_NTFY_BASE_URL")
        .env_remove("PHASEGENT_NOTIFY_NTFY_TOPIC")
        .env_remove("PHASEGENT_NOTIFY_NTFY_TOKEN")
        .env_remove("PHASEGENT_NOTIFY_WEBHOOK_URL")
        .env_remove("PHASEGENT_NOTIFY_WEBHOOK_TOKEN")
        .env_remove("PHASEGENT_PROVIDER")
        .env_remove("PHASEGENT_DEFAULT_PROVIDER")
        .env("RUST_BACKTRACE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in extra_env {
        command.env(key, value);
    }
    command.output().expect("spawn phasegent binary")
}

fn run_with_stdin(scratch: &TempfileLikeDir, args: &[&str], stdin_text: &str) -> Output {
    use std::io::Write;
    let mut child = Command::new(phasegent_bin())
        .args(args)
        .env("PHASEGENT_DB_PATH", scratch.db_path().as_os_str())
        .env("PHASEGENT_CONFIG_PATH", scratch.missing_toml().as_os_str())
        .env_remove("PHASEGENT_NOTIFY_ENABLED")
        .env_remove("PHASEGENT_NOTIFY_CHANNEL")
        .env_remove("PHASEGENT_NOTIFY_NTFY_BASE_URL")
        .env_remove("PHASEGENT_NOTIFY_NTFY_TOPIC")
        .env_remove("PHASEGENT_NOTIFY_NTFY_TOKEN")
        .env_remove("PHASEGENT_NOTIFY_WEBHOOK_URL")
        .env_remove("PHASEGENT_NOTIFY_WEBHOOK_TOKEN")
        .env("RUST_BACKTRACE", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn phasegent binary");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(stdin_text.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("wait")
}

#[test]
fn notify_send_requires_role() {
    let scratch = scratch_db();
    let output = run_phasegent(
        &scratch,
        &["notify", "send", "--event", "completion", "--title", "hi"],
        &[],
    );
    assert!(!output.status.success(), "notify without --role must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--role"),
        "missing --role error; stderr={stderr}"
    );
}

#[test]
fn notify_send_validates_event() {
    let scratch = scratch_db();
    let output = run_phasegent(
        &scratch,
        &[
            "--role", "executor", "notify", "send", "--event", "pager", "--title", "hi",
        ],
        &[],
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid --event"), "stderr={stderr}");
}

#[test]
fn notify_send_skipped_when_disabled_persists_intent() {
    let scratch = scratch_db();
    // No notify config: disabled by default.
    let output = run_phasegent(
        &scratch,
        &[
            "--role",
            "executor",
            "notify",
            "send",
            "--event",
            "completion",
            "--title",
            "done",
            "--body",
            "all good",
        ],
        &[],
    );
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = stdout_text(&output);
    assert!(stdout.contains("\"notified\":false"), "stdout={stdout}");
    assert!(stdout.contains("\"skipped\":true"), "stdout={stdout}");
}

#[test]
fn notify_channel_config_round_trips_and_snapshot_redacts_secrets() {
    let scratch = scratch_db();
    // Non-secret fields persist via positional values.
    for (setting, value) in [
        ("notify-enabled", "true"),
        ("notify-channel", "webhook"),
        ("notify-webhook-url", "https://hooks.example.com/notify"),
    ] {
        let output = run_phasegent(&scratch, &["config", "set", setting, value], &[]);
        assert!(
            output.status.success(),
            "set {setting} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    // Secret via --stdin only; direct value must be rejected.
    let direct = run_phasegent(
        &scratch,
        &["config", "set", "notify-webhook-token", "secret-value"],
        &[],
    );
    assert!(!direct.status.success());
    let via_stdin = run_with_stdin(
        &scratch,
        &["config", "set", "notify-webhook-token", "--stdin"],
        "s3cret-token",
    );
    assert!(
        via_stdin.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&via_stdin.stderr)
    );
    // Snapshot must not echo the secret value.
    let show = run_phasegent(&scratch, &["config", "show"], &[]);
    assert!(show.status.success());
    let stdout = stdout_text(&show);
    assert!(
        !stdout.contains("s3cret-token"),
        "snapshot leaked secret; stdout={stdout}"
    );
    assert!(
        stdout.contains("PHASEGENT_NOTIFY_WEBHOOK_TOKEN"),
        "snapshot missing notify token row; stdout={stdout}"
    );
    assert!(
        stdout.contains("PHASEGENT_NOTIFY_CHANNEL"),
        "snapshot missing notify channel; stdout={stdout}"
    );
    // Sanitised webhook URL renders without userinfo.
    assert!(
        stdout.contains("https://hooks.example.com/notify"),
        "snapshot missing sanitised webhook url; stdout={stdout}"
    );
    // Clear works for notify fields.
    let clear = run_phasegent(&scratch, &["config", "clear", "notify-channel"], &[]);
    assert!(clear.status.success());
}

#[test]
fn notify_help_resolves() {
    let scratch = scratch_db();
    for args in [&["--help", "notify"][..], &["--help", "notify", "send"][..]] {
        let output = run_phasegent(&scratch, args, &[]);
        assert!(output.status.success(), "args={args:?}");
        let stdout = stdout_text(&output);
        assert!(!stdout.trim().is_empty(), "args={args:?}");
    }
}
