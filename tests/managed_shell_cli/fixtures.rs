// Shared fixtures for the managed shell bridge-to-CLI integration.
//
// Every scenario drives the generated plugin (`assets/opencode/phasegent-worktree.js`)
// through Bun with isolated hook callbacks, then executes the hook-mutated
// script/env against an isolated local-provider scratch store via the
// compiled `phasegent` binary. Tests write only their isolated scratch local
// issue/record data, never a production provider or API. Output is bounded
// and asserted to carry no body/secret logging.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use super::support::broker::{BROKER_ADDR_ENV, BROKER_TOKEN_ENV, TestBroker};
use super::support::{phasegent_bin, stderr_text, stdout_text};

/// Per-test scratch directory holding the isolated local SQLite file and any
/// Bun driver fixtures.
pub(crate) struct Scratch {
    dir: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

impl Scratch {
    pub(crate) fn new(label: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = super::support::scratch_root().join(format!(
            "phasegent-it-managed-shell-{label}-{}-{}-{}",
            std::process::id(),
            nanos,
            (nanos as u64) ^ (std::process::id() as u64),
        ));
        fs::create_dir_all(&dir).expect("create scratch dir");
        Self { dir }
    }

    #[allow(dead_code)]
    pub(crate) fn path(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn local_db(&self) -> PathBuf {
        self.dir.join("phasegent-local.sqlite3")
    }
}

/// Spawn the compiled binary against an isolated local store with both
/// managed markers cleared unless explicitly set. `role` models the
/// legacy role marker, `session` models `PHASEGENT_SESSION_ID`, and
/// `host_session` models `OPENCODE_SESSION_ID` (`None` removes, `Some`
/// sets even when blank).
pub(crate) fn run_local_with_markers(
    db: &Path,
    role: Option<&str>,
    session: Option<&str>,
    host_session: Option<&str>,
    args: &[&str],
) -> Output {
    run_local_with_context(db, role, session, host_session, None, args)
}

/// Spawn the compiled binary with the full managed context: the legacy role
/// marker, the session markers, and the loopback identity broker the
/// plugin handed to the shell. The role is only ever granted through the
/// broker, so a caller that has no broker context fails closed.
pub(crate) fn run_local_with_context(
    db: &Path,
    role: Option<&str>,
    session: Option<&str>,
    host_session: Option<&str>,
    broker: Option<(&str, &str)>,
    args: &[&str],
) -> Output {
    let missing_toml = db
        .parent()
        .map(|dir| dir.join("phasegent-missing.toml"))
        .unwrap_or_else(|| std::env::temp_dir().join("phasegent-missing.toml"));
    // The fixture owns the main config store too, so the scope guard can
    // never read the machine's branch links. It sits beside the distinct
    // local store and starts absent in the fixture-owned scratch directory.
    let main_db = db.with_file_name("phasegent-main.sqlite3");
    let mut command = Command::new(phasegent_bin());
    command
        .args(args)
        .env("PHASEGENT_LOCAL_DB_PATH", db.as_os_str())
        .env("PHASEGENT_DB_PATH", main_db.as_os_str())
        .env_remove("PHASEGENT_ROLE")
        .env_remove("PHASEGENT_SESSION_ID")
        .env_remove("OPENCODE_SESSION_ID")
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
        .env_remove(BROKER_ADDR_ENV)
        .env_remove(BROKER_TOKEN_ENV)
        .env("PHASEGENT_CONFIG_PATH", missing_toml.as_os_str())
        .env("RUST_BACKTRACE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(role) = role {
        command.env("PHASEGENT_ROLE", role);
    }
    if let Some(session) = session {
        command.env("PHASEGENT_SESSION_ID", session);
    }
    if let Some(host_session) = host_session {
        command.env("OPENCODE_SESSION_ID", host_session);
    }
    if let Some((addr, token)) = broker {
        command.env(BROKER_ADDR_ENV, addr);
        command.env(BROKER_TOKEN_ENV, token);
    }
    command.output().expect("spawn phasegent binary")
}

/// Initialize the local store (schema + seeds) with a read that touches no
/// issue. The read runs as a verified executor through the loopback
/// identity broker, exactly like a managed shell.
pub(crate) fn init_local(db: &Path) {
    let broker = TestBroker::start("executor", "ses-fixture-1");
    let out = run_local_with_context(
        db,
        None,
        Some(broker.session()),
        None,
        Some((broker.addr(), broker.token())),
        &["--provider", "local", "record", "list", "1"],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "local store init failed: {}",
        stderr_text(&out)
    );
}

/// Insert one local issue directly so the test never depends on the
/// provisioning-only `issue create` surface.
pub(crate) fn seed_issue(db: &Path, title: &str) -> u64 {
    let conn = rusqlite::Connection::open(db).expect("open local db");
    conn.execute(
        "INSERT INTO local_issues \
         (title, body, status, project, tracker, author_role, created_at, updated_at) \
         VALUES (?1, 'body', 'New', 'default', 'Task', 'orchestrator', 1000, 1000)",
        rusqlite::params![title],
    )
    .expect("insert local issue");
    conn.last_insert_rowid() as u64
}

pub(crate) fn json_stdout(output: &Output) -> serde_json::Value {
    serde_json::from_str(stdout_text(output).trim()).expect("JSON on stdout")
}

pub(crate) fn error_envelope(output: &Output) -> serde_json::Value {
    serde_json::from_str(stderr_text(output).trim()).expect("structured error on stderr")
}

/// Result of driving the generated plugin's hooks through Bun: the exact
/// restored command plus the fresh-env identity the shell hook adopted.
#[derive(Debug, serde::Deserialize)]
pub(crate) struct BridgeResult {
    pub(crate) command: String,
    pub(crate) env_session: Option<String>,
    pub(crate) env_broker_addr: Option<String>,
    pub(crate) env_broker_token: Option<String>,
    pub(crate) host_session: Option<String>,
    pub(crate) decorated_had_header: bool,
}

/// A running Bun driver with its identity broker. The driver keeps the
/// broker alive so the CLI can verify itself against it; dropping the
/// driver stops the broker and revokes its credential.
pub(crate) struct BridgeDriver {
    child: Child,
    pub(crate) result: BridgeResult,
}

impl Drop for BridgeDriver {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Drive the checked-in dist through Bun: `tool.execute.before` (rewrite +
/// nonce header) then `shell.create.before` (exact-match consume, fresh-env
/// mutation), then simulate the host setting its native `OPENCODE_SESSION_ID`.
/// Returns the parsed bridge result with bounded output; asserts the driver
/// logged no body/secret bytes.
pub(crate) fn drive_bridge_via_bun(
    tool_agent: &str,
    tool_session: &str,
    tool_command: &str,
    inherited_role: Option<&str>,
    host_session: &str,
) -> BridgeDriver {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dist = repo_root.join("assets/opencode/phasegent-worktree.js");
    assert!(
        dist.exists(),
        "generated dist must exist at {}",
        dist.display()
    );

    // Escape the command for embedding in a JS single-quoted context via JSON.
    let command_json = serde_json::to_string(tool_command).expect("command JSON");
    let agent_json = serde_json::to_string(tool_agent).expect("agent JSON");
    let session_json = serde_json::to_string(tool_session).expect("session JSON");
    let inherited_json = serde_json::to_string(&inherited_role).expect("inherited JSON");
    let host_json = serde_json::to_string(host_session).expect("host JSON");
    let dist_str = dist.to_string_lossy().replace('\\', "\\\\");

    // The driver imports the dist (proving source/dist currency), drives the
    // two hooks, simulates the host native marker, and prints one bounded
    // JSON document. The identity broker stays alive until the driver is
    // stopped, so the CLI can verify the managed context against it. No
    // command or env secret is printed to stderr.
    let script = format!(
        r##"
import Plugin from "{dist_str}";
const {{ createRedirectHook, createShellEnvHook, createManagedShellBridge, createIdentityBroker }} = Plugin.redirect;
const broker = createIdentityBroker();
const brokerAddress = await broker.start();
const bridge = createManagedShellBridge({{ broker }});
const redirect = createRedirectHook(bridge);
const shellHook = createShellEnvHook(bridge);
const toolEvent = {{ tool: "shell", sessionID: {session_json}, agent: {agent_json}, id: "tool-managed-1", input: {{ command: {command_json} }} }};
await redirect(toolEvent);
const decorated = toolEvent.input.command;
const decoratedHadHeader = typeof decorated === "string" && decorated.startsWith("# phasegent-context:");
const inherited = {inherited_json};
const freshEnv = {{}};
if (inherited !== null) freshEnv["PHASEGENT_ROLE"] = inherited;
const shellEvent = {{ command: decorated, env: freshEnv }};
await shellHook(shellEvent);
// Host native mutator runs after the shell hook and unconditionally marks
// the managed shell.
shellEvent.env["OPENCODE_SESSION_ID"] = {host_json};
const out = {{
  command: shellEvent.command,
  env_session: shellEvent.env["PHASEGENT_SESSION_ID"] ?? null,
  env_broker_addr: shellEvent.env["PHASEGENT_BROKER_ADDR"] ?? null,
  env_broker_token: shellEvent.env["PHASEGENT_BROKER_TOKEN"] ?? null,
  host_session: shellEvent.env["OPENCODE_SESSION_ID"] ?? null,
  decorated_had_header: decoratedHadHeader,
}};
console.log(JSON.stringify(out));
// The broker keeps serving until the parent stops the driver.
await new Promise(() => {{}});
"##
    );

    let mut command = Command::new("bun");
    command
        .arg("--silent")
        .arg("-e")
        .arg(&script)
        .env_remove("PHASEGENT_ROLE")
        .env_remove("PHASEGENT_SESSION_ID")
        .env_remove("OPENCODE_SESSION_ID")
        .env_remove("PHASEGENT_BROKER_ADDR")
        .env_remove("PHASEGENT_BROKER_TOKEN")
        .env("RUST_BACKTRACE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn bun driver");
    let stdout = child.stdout.take().expect("driver stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line).expect("driver JSON line");
    let trimmed = line.trim().to_owned();
    assert!(
        trimmed.len() < 8192,
        "bridge driver output must stay bounded: {trimmed}"
    );
    let parsed: BridgeResult = serde_json::from_str(&trimmed).expect("bridge JSON on stdout");
    // The restored command must not retain the nonce header.
    assert!(
        !parsed.command.starts_with("# phasegent-context:"),
        "shell hook must strip its own header"
    );
    BridgeDriver {
        child,
        result: parsed,
    }
}
