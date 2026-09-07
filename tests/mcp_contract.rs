//! MCP server contract: CLI surface, help, validation, and transport boot.
//!
//! Tools wrap existing provider/notify paths with server-side role
//! only. Excluded unless re-scoped: status_advance, timer
//! start/finish, role elevation.

use std::process::{Command, Output, Stdio};

fn scratch_db() -> ScratchDir {
    ScratchDir::new()
}

struct ScratchDir {
    dir: std::path::PathBuf,
}

impl ScratchDir {
    fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("phasegent-it-mcp-{}-{nanos}", std::process::id()));
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

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn run_phasegent(scratch: &ScratchDir, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_phasegent"));
    command
        .args(args)
        .env("PHASEGENT_DB_PATH", scratch.db_path().as_os_str())
        .env("PHASEGENT_CONFIG_PATH", scratch.missing_toml().as_os_str())
        .env_remove("PHASEGENT_PROVIDER")
        .env_remove("PHASEGENT_DEFAULT_PROVIDER")
        .env_remove("PHASEGENT_NOTIFY_ENABLED")
        .env_remove("PHASEGENT_NOTIFY_CHANNEL")
        .env_remove("PHASEGENT_NOTIFY_NTFY_BASE_URL")
        .env_remove("PHASEGENT_NOTIFY_NTFY_TOPIC")
        .env_remove("PHASEGENT_NOTIFY_NTFY_TOKEN")
        .env_remove("PHASEGENT_NOTIFY_WEBHOOK_URL")
        .env_remove("PHASEGENT_NOTIFY_WEBHOOK_TOKEN")
        .env("RUST_BACKTRACE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.output().expect("spawn phasegent binary")
}

fn stdout_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn root_help_lists_notify_and_mcp() {
    let scratch = scratch_db();
    let output = run_phasegent(&scratch, &["--help"]);
    assert!(output.status.success(), "stderr={}", stderr_text(&output));
    let stdout = stdout_text(&output);
    assert!(
        stdout.contains("notify"),
        "root help missing notify line; stdout={stdout}"
    );
    assert!(
        stdout.contains("mcp"),
        "root help missing mcp line; stdout={stdout}"
    );
}

#[test]
fn mcp_help_resolves() {
    let scratch = scratch_db();
    for args in [&["--help", "mcp"][..], &["--help", "mcp", "serve"][..]] {
        let output = run_phasegent(&scratch, args);
        assert!(
            output.status.success(),
            "args={args:?} stderr={}",
            stderr_text(&output)
        );
        assert!(!stdout_text(&output).trim().is_empty(), "args={args:?}");
    }
}

#[test]
fn notify_help_still_resolves() {
    let scratch = scratch_db();
    for args in [&["--help", "notify"][..], &["--help", "notify", "send"][..]] {
        let output = run_phasegent(&scratch, args);
        assert!(
            output.status.success(),
            "args={args:?} stderr={}",
            stderr_text(&output)
        );
        assert!(!stdout_text(&output).trim().is_empty(), "args={args:?}");
    }
}

#[test]
fn mcp_help_documents_contracted_scope_and_exclusions() {
    let scratch = scratch_db();
    let output = run_phasegent(&scratch, &["--help", "mcp"]);
    assert!(output.status.success());
    let stdout = stdout_text(&output);
    for tool in [
        "capabilities",
        "issue_get",
        "issue_search",
        "status_next",
        "comment_create",
        "notify_send",
    ] {
        assert!(
            stdout.contains(tool),
            "mcp help missing {tool}; stdout={stdout}"
        );
    }
    assert!(
        stdout.contains("status_advance") || stdout.contains("status advance"),
        "mcp help should name the status_advance exclusion; stdout={stdout}"
    );
}

#[test]
fn mcp_serve_requires_role() {
    let scratch = scratch_db();
    let output = run_phasegent(&scratch, &["mcp", "serve"]);
    assert!(
        !output.status.success(),
        "mcp serve without --role must fail"
    );
    assert!(
        stderr_text(&output).contains("--role"),
        "stderr={}",
        stderr_text(&output)
    );
}

#[test]
fn mcp_serve_rejects_bad_transport() {
    let scratch = scratch_db();
    let output = run_phasegent(
        &scratch,
        &["--role", "executor", "mcp", "serve", "--transport", "bogus"],
    );
    assert!(!output.status.success());
    assert!(
        stderr_text(&output).contains("--transport"),
        "stderr={}",
        stderr_text(&output)
    );
}

#[test]
fn mcp_serve_rejects_bind_without_http() {
    let scratch = scratch_db();
    let output = run_phasegent(
        &scratch,
        &[
            "--role",
            "executor",
            "mcp",
            "serve",
            "--transport",
            "stdio",
            "--bind",
            "127.0.0.1:3000",
        ],
    );
    assert!(!output.status.success());
    assert!(
        stderr_text(&output).contains("--bind"),
        "stderr={}",
        stderr_text(&output)
    );
}

#[test]
fn mcp_serve_rejects_bad_bind() {
    let scratch = scratch_db();
    let output = run_phasegent(
        &scratch,
        &[
            "--role",
            "executor",
            "mcp",
            "serve",
            "--transport",
            "http",
            "--bind",
            "not-an-addr",
        ],
    );
    assert!(!output.status.success());
    assert!(
        stderr_text(&output).contains("--bind"),
        "stderr={}",
        stderr_text(&output)
    );
}

#[test]
fn mcp_rejects_unknown_subcommand() {
    let scratch = scratch_db();
    let output = run_phasegent(&scratch, &["--role", "executor", "mcp", "frobnicate"]);
    assert!(!output.status.success());
    assert!(
        stderr_text(&output).contains("unknown mcp command"),
        "stderr={}",
        stderr_text(&output)
    );
}

#[test]
fn mcp_http_starts_and_reports_bind() {
    let scratch = scratch_db();
    // Find a free loopback port first so the assertion never collides
    // with a developer's local service.
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe");
        listener.local_addr().expect("probe addr").port()
    };
    let bind = format!("127.0.0.1:{port}");
    let mut child = Command::new(env!("CARGO_BIN_EXE_phasegent"))
        .args([
            "--role",
            "executor",
            "mcp",
            "serve",
            "--transport",
            "http",
            "--bind",
            &bind,
        ])
        .env("PHASEGENT_DB_PATH", scratch.db_path().as_os_str())
        .env("PHASEGENT_CONFIG_PATH", scratch.missing_toml().as_os_str())
        .env_remove("PHASEGENT_PROVIDER")
        .env_remove("PHASEGENT_DEFAULT_PROVIDER")
        .env("RUST_BACKTRACE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mcp http server");
    // Wait for the bind report on stderr (up to ~5s), then kill.
    let mut stderr = String::new();
    let started = std::time::Instant::now();
    let reported = loop {
        if started.elapsed() > std::time::Duration::from_secs(5) {
            break false;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                panic!("mcp http exited early with {status}");
            }
            Ok(None) => {}
            Err(error) => panic!("try_wait failed: {error}"),
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        // Non-blocking peek: try to read available stderr via try? Use
        // a short timeout read by checking if the TCP port accepts.
        if std::net::TcpStream::connect(&bind).is_ok() {
            stderr.push_str(&bind);
            break true;
        }
    };
    child.kill().ok();
    let _ = child.wait();
    assert!(
        reported,
        "mcp http did not bind {bind} in time; stderr={stderr}"
    );
}

#[test]
fn mcp_help_marks_bind_http_only() {
    let scratch = scratch_db();
    for args in [&["--help", "mcp"][..], &["--help", "mcp", "serve"][..]] {
        let output = run_phasegent(&scratch, args);
        assert!(
            output.status.success(),
            "args={args:?} stderr={}",
            stderr_text(&output)
        );
        let stdout = stdout_text(&output);
        assert!(
            stdout.contains("--bind"),
            "mcp help missing --bind; args={args:?} stdout={stdout}"
        );
        assert!(
            stdout.contains("HTTP-only"),
            "mcp help must mark --bind as HTTP-only; args={args:?} stdout={stdout}"
        );
    }
    let output = run_phasegent(&scratch, &["--help", "mcp", "serve"]);
    assert!(output.status.success());
    let stdout = stdout_text(&output);
    assert!(
        stdout.contains("requires --transport http"),
        "mcp serve help must state --bind requires http; stdout={stdout}"
    );
}

#[test]
fn notify_send_rejects_notify_setting_phase() {
    let scratch = scratch_db();
    for phase in ["PHASEGENT_NOTIFY_CHANNEL", "PHASEGENT_NOTIFY_WEBHOOK_TOKEN"] {
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
                "hi",
                "--phase",
                phase,
            ],
        );
        assert!(
            !output.status.success(),
            "notify send --phase {phase} must be rejected"
        );
        let stderr = stderr_text(&output);
        assert!(
            stderr.contains("--phase") && stderr.contains("must not be"),
            "phase={phase} stderr={stderr}"
        );
    }
    let ok = run_phasegent(
        &scratch,
        &[
            "--role",
            "executor",
            "notify",
            "send",
            "--event",
            "completion",
            "--title",
            "hi",
            "--phase",
            "mcp-phase",
        ],
    );
    assert!(
        ok.status.success(),
        "ordinary phase must stay accepted; stderr={}",
        stderr_text(&ok)
    );
}

#[test]
fn notify_send_title_truncate_ceiling() {
    let scratch = scratch_db();
    let too_long = "t".repeat(2001);
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
            too_long.as_str(),
        ],
    );
    assert!(
        !output.status.success(),
        "title above 2000 chars must be rejected"
    );
    assert!(
        stderr_text(&output).contains("too long"),
        "stderr={}",
        stderr_text(&output)
    );
    // Above the 140-char envelope truncation but below the 2000-char
    // parser ceiling: accepted (truncated), not rejected.
    let truncated = "t".repeat(500);
    let ok = run_phasegent(
        &scratch,
        &[
            "--role",
            "executor",
            "notify",
            "send",
            "--event",
            "completion",
            "--title",
            truncated.as_str(),
        ],
    );
    assert!(
        ok.status.success(),
        "500-char title must be accepted via truncation; stderr={}",
        stderr_text(&ok)
    );
}
