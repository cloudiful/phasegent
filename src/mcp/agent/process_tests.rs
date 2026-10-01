//! Child-process lifecycle regression tests.
//!
//! A checkpoint review found a spawned `mcode acp` process could survive
//! the error path that spawned it: a rejected handshake dropped the
//! `tokio::process::Child` without killing or reaping it, and the spawn
//! did not set `kill_on_drop`. These use a real process — an executable
//! stub that records its pid and then goes silent — so the assertion is
//! about the OS, not about a mock.
//!
//! The last two cases here are the other half of the redaction
//! contract: the child's diagnostics are captured so a crash stays
//! diagnosable, and both they and the child's own JSON-RPC error
//! message are agent-owned text, so a secret in either must not reach
//! the error this adapter persists and returns.

use std::time::Duration;

use super::run::RunManager;
use super::session::AcpSession;
use super::types::{AcpSpawnConfig, ResearchPrompt};

use super::tests::{open_storage, temp_db_path, wait_terminal};

const STUB_SLEEP: &str = "120";

/// Write an executable stub that records its pid and runs `body`, so a
/// test can drive a real child process. Returns the stub path and the
/// pid file it writes.
#[cfg(unix)]
fn stub(label: &str, body: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_db_path(label)
        .parent()
        .expect("scratch dir")
        .to_path_buf();
    std::fs::create_dir_all(&dir).expect("create stub dir");
    let pid_file = dir.join("stub.pid");
    let script = dir.join("acp-stub");
    std::fs::write(
        &script,
        format!("#!/bin/sh\necho $$ > {}\n{body}\n", pid_file.display()),
    )
    .expect("write stub");
    let mut permissions = std::fs::metadata(&script).expect("stat stub").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).expect("chmod stub");
    (script, pid_file)
}

/// A stub that records its pid and then goes silent, so a test can prove
/// the process is gone once the adapter gives up on the handshake.
#[cfg(unix)]
fn silent_stub(label: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    stub(label, &format!("sleep {STUB_SLEEP}"))
}

/// Whether a recorded pid is still a live process. A reaped child is
/// gone; a zombie is not: it holds no file descriptor and cannot
/// outlive the test, so it does not count as a survivor. On a host
/// without `/proc` the `kill -0` fallback also accepts a zombie, which
/// only makes the assertion conservative.
#[cfg(unix)]
fn process_alive(pid_file: &std::path::Path) -> Option<u32> {
    let pid = recorded_pid(pid_file)?;
    if exited(&pid) {
        return None;
    }
    let alive = std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    alive.then_some(pid)
}

/// The pid a stub recorded, whether or not it is still running.
#[cfg(unix)]
fn recorded_pid(pid_file: &std::path::Path) -> Option<u32> {
    let raw = std::fs::read_to_string(pid_file).ok()?;
    raw.trim().parse().ok()
}

/// Whether the pid is gone or reduced to a zombie.
#[cfg(target_os = "linux")]
fn exited(pid: &u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return true;
    };
    // The comm field can contain spaces and parentheses, so the state
    // is the field after the last ')'.
    stat.rsplit_once(") ")
        .is_some_and(|(_, rest)| rest.starts_with('Z'))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn exited(_pid: &u32) -> bool {
    false
}

/// Wait for the stub to record a live pid, with a budget generous
/// enough to survive a loaded parallel test run.
#[cfg(unix)]
async fn wait_for_pid(pid_file: &std::path::Path) -> u32 {
    for _ in 0..750 {
        if let Some(found) = process_alive(pid_file) {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the stub never recorded a live pid");
}

#[cfg(unix)]
async fn assert_process_gone(pid: u32, pid_file: &std::path::Path, stage: &str) {
    for _ in 0..500 {
        if process_alive(pid_file).is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("process {pid} outlived {stage}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_failed_handshake_kills_the_process_it_spawned() {
    let (stub, pid_file) = silent_stub("handshake-stub");
    // The stub never answers `initialize`, so the bounded handshake
    // fails and the process must not survive it. A leaked child would
    // be an `mcode acp` process still holding the scratch cwd.
    let config = AcpSpawnConfig {
        program: stub.display().to_string(),
        cwd: std::env::temp_dir(),
        handshake_timeout_secs: Some(1),
    };
    let start = tokio::spawn({
        let config = config.clone();
        async move { AcpSession::start(&config).await }
    });
    let pid = wait_for_pid(&pid_file).await;
    let error = match start.await.expect("start task") {
        Err(error) => error,
        Ok(_) => panic!("a silent agent must not complete the handshake"),
    };
    assert_eq!(error.kind.as_str(), "timeout", "{}", error.message);
    assert_process_gone(pid, &pid_file, "the failed handshake").await;
}

#[cfg(unix)]
#[tokio::test]
async fn an_aborted_start_leaves_no_child_process_behind() {
    // The launch task is aborted mid-handshake, which is what a cancel
    // escalation does to a run whose agent never answers. The spawn is
    // `kill_on_drop`, so the process must die with the task rather than
    // outlive it holding the scratch cwd.
    let (stub, pid_file) = silent_stub("abort-stub");
    let config = AcpSpawnConfig {
        program: stub.display().to_string(),
        cwd: std::env::temp_dir(),
        handshake_timeout_secs: Some(600),
    };
    let start = tokio::spawn(async move { AcpSession::start(&config).await });
    let pid = wait_for_pid(&pid_file).await;
    start.abort();
    assert_process_gone(pid, &pid_file, "the aborted start").await;
}

#[cfg(unix)]
#[tokio::test]
async fn agent_stderr_cannot_put_a_secret_in_the_persisted_error() {
    // The stub dies with credentials in its diagnostics, which is the
    // one place agent-owned text reaches this adapter. The error it
    // produces is persisted in the run ledger and returned to a caller,
    // so the secret must be gone from the row and from anything a
    // caller can read out of it.
    const SECRET: &str = "sk-live-VALUE0123456789";
    let (stub, pid_file) = stub(
        "stderr-secret",
        &format!(
            "echo 'fatal: upstream refused Authorization: Bearer {SECRET} \
             api_key=abcd1234efgh5678 https://user:s3cr3t@example.com/api' >&2\n\
             sleep 0.3\n\
             exit 1"
        ),
    );
    let path = temp_db_path("stderr-secret");
    let manager = RunManager::new(open_storage(&path)).expect("manager");
    manager
        .start_run(
            "stderr-run",
            "ses_stderr",
            AcpSpawnConfig {
                program: stub.display().to_string(),
                cwd: std::env::temp_dir(),
                handshake_timeout_secs: Some(30),
            },
            ResearchPrompt::new("recon"),
        )
        .await
        .expect("run accepted");

    let reader = open_storage(&path);
    let settled = wait_terminal(&reader, "stderr-run").await;
    assert_eq!(settled.status, "failed", "the stub cannot answer");
    let reported = format!(
        "{:?} {} {} {:?}",
        settled.error,
        settled.output.clone().unwrap_or_default(),
        settled.prompt,
        settled
    );
    assert!(!reported.contains(SECRET), "{reported}");
    assert!(!reported.contains("s3cr3t"), "{reported}");
    assert!(!reported.contains("abcd1234"), "{reported}");
    // The diagnostics stay usable: the failure is still explained.
    let error = settled.error.expect("a failure reason");
    assert!(error.contains("upstream refused"), "{error}");
    assert!(error.contains("[redacted]"), "{error}");
    let pid = recorded_pid(&pid_file).expect("the stub recorded its pid");
    assert_process_gone(pid, &pid_file, "the failed run").await;
}

#[cfg(unix)]
#[tokio::test]
async fn an_agent_authored_error_cannot_put_a_secret_in_the_persisted_error() {
    // The other agent-owned channel into an error message: not stderr,
    // but the `error.message` the child puts in a JSON-RPC error
    // response. It reaches the same persisted row, so it needs the same
    // bound. The stub answers `initialize` with a refusal whose message
    // embeds credentials.
    const SECRET: &str = "sk-live-VALUE0123456789";
    // The stub echoes the request id instead of hardcoding one: request
    // ids come from a process-global counter, so a parallel test holds
    // the low numbers and a fixed id would be answered into the void.
    let (stub, pid_file) = stub(
        "rpc-error-secret",
        &format!(
            r#"while IFS= read -r line; do
  id=$(echo "$line" | sed 's/.*"id"://; s/,.*//')
  echo "{{\"jsonrpc\":\"2.0\",\"id\":$id,\"error\":{{\"code\":-32000,\"message\":\"upstream refused api_key=abcd1234efgh5678 https://user:{SECRET}@example.com/api\"}}}}"
  break
done
sleep 5"#
        ),
    );
    let path = temp_db_path("rpc-error-secret");
    let manager = RunManager::new(open_storage(&path)).expect("manager");
    manager
        .start_run(
            "rpc-error-run",
            "ses_rpc_error",
            AcpSpawnConfig {
                program: stub.display().to_string(),
                cwd: std::env::temp_dir(),
                handshake_timeout_secs: Some(30),
            },
            ResearchPrompt::new("recon"),
        )
        .await
        .expect("run accepted");

    let reader = open_storage(&path);
    let settled = wait_terminal(&reader, "rpc-error-run").await;
    assert_eq!(settled.status, "failed", "the stub refuses initialize");
    let reported = format!(
        "{:?} {} {} {:?}",
        settled.error,
        settled.output.clone().unwrap_or_default(),
        settled.prompt,
        settled
    );
    assert!(!reported.contains(SECRET), "{reported}");
    assert!(!reported.contains("abcd1234"), "{reported}");
    // The refusal stays diagnosable: only the credential is gone.
    let error = settled.error.expect("a failure reason");
    assert!(error.contains("upstream refused"), "{error}");
    assert!(error.contains("[redacted]"), "{error}");
    let pid = recorded_pid(&pid_file).expect("the stub recorded its pid");
    assert_process_gone(pid, &pid_file, "the failed run").await;
}
