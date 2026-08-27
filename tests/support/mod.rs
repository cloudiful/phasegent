//! Shared scaffolding for the `redmine_status_*` integration tests. Every
//! scenario launches the compiled `phasegent` binary and asserts on the
//! request sequence, exit codes, and JSON payloads produced by the full
//! stack (CLI parsing → provider dispatch → HTTP → status verification
//! → JSON output). Helpers centralize canonical status ids, the
//! production SQLite schema, a per-test database at `PHASEGENT_DB_PATH`,
//! a local TCP HTTP mock, and a binary-spawn wrapper that strips
//! `PHASEGENT_*` overrides so each test focuses on its assertion surface.

use std::collections::VecDeque;
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

// Canonical status ids. Ids are stable so the close status seed lines
// up with the actual mock row.
pub const STATUS_NEW: u64 = 1;
pub const STATUS_IN_PROGRESS: u64 = 2;
pub const STATUS_IN_REVIEW: u64 = 3;
pub const STATUS_CHANGES_REQUESTED: u64 = 4;
pub const STATUS_BLOCKED: u64 = 5;
pub const STATUS_RESOLVED: u64 = 6;
pub const STATUS_CLOSED: u64 = 7;
pub const STATUS_CANCELLED: u64 = 8;

/// Configured close status id, resolved against the mock status list.
pub const CLOSE_STATUS_ID: u64 = STATUS_CLOSED;

/// Issue id used by every scenario, unique enough that the request log
/// scans unambiguously.
pub const ISSUE_ID: u64 = 4242;

/// Project id from the seeded `role_redmine_config` row. Tests pin it
/// via `--project-id` so bootstrap is skipped and every request is
/// attributable to the scenario.
pub const PROJECT_ID: &str = "4242";

/// Synthetic API keys. The mock asserts against the orchestrator key so
/// a credential regression cannot pass silently; the rest let denial
/// tests confirm no credential leaks through a permission envelope.
pub const ORCHESTRATOR_KEY: &str = "orchestrator-test-key";
pub const EXECUTOR_KEY: &str = "executor-test-key";
pub const REVIEWER_KEY: &str = "reviewer-test-key";
pub const ADMIN_KEY: &str = "admin-test-key";

/// Production SQLite schema. Kept inline to avoid a `lib.rs` entry
/// point or cross-crate dependency for the test target.
pub const SCHEMA_SQL: &str = "\
CREATE TABLE IF NOT EXISTS role_config (
    role TEXT PRIMARY KEY,
    provider TEXT,
    api_base TEXT,
    repository TEXT
);
CREATE TABLE IF NOT EXISTS role_redmine_config (
    role TEXT PRIMARY KEY,
    api_base TEXT,
    project_id TEXT,
    close_status_id INTEGER
);
CREATE TABLE IF NOT EXISTS role_credential (
    role TEXT NOT NULL,
    provider TEXT NOT NULL,
    credential TEXT NOT NULL,
    PRIMARY KEY (role, provider)
);
CREATE TABLE IF NOT EXISTS global_setting (
    name TEXT PRIMARY KEY,
    value TEXT
);
CREATE TABLE IF NOT EXISTS execution_timer_runs (
    run_id TEXT PRIMARY KEY,
    issue_id INTEGER NOT NULL CHECK (issue_id > 0),
    phase TEXT NOT NULL,
    role TEXT NOT NULL,
    attempt INTEGER NOT NULL CHECK (attempt > 0),
    started_at INTEGER NOT NULL,
    finished_at INTEGER,
    status TEXT NOT NULL,
    elapsed_seconds INTEGER,
    rounded_hours REAL,
    activity_id INTEGER,
    redmine_time_entry_id INTEGER,
    sync_status TEXT NOT NULL DEFAULT 'pending',
    sync_error TEXT,
    owner_session_id TEXT,
    owner_call_id TEXT
);
CREATE INDEX IF NOT EXISTS execution_timer_runs_issue_phase_idx
    ON execution_timer_runs (issue_id, phase, role, attempt);
";

/// Pragmas mirroring production bootstrap so timing and WAL surface
/// cannot drift between test and runtime.
pub const PRAGMA_SQL: &str = "\
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;
";

/// One canned response waiting on the mock's FIFO queue, sent with
/// `Content-Type: application/json` so `reqwest`'s JSON middleware
/// accepts it.
#[derive(Clone)]
pub struct MockResponse {
    pub status: u16,
    pub body: String,
}

impl MockResponse {
    pub fn ok(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            body: body.into(),
        }
    }
}

/// Threaded TCP mock serving a FIFO of canned responses and recording
/// every HTTP request until [`MockServer::drop`] closes it.
pub struct MockServer {
    pub base_url: String,
    pub requests: Arc<Mutex<Vec<String>>>,
    handle: Option<thread::JoinHandle<()>>,
    /// Shared stop flag the worker checks each loop iteration so
    /// [`Drop`] can shut the accept loop down cleanly.
    _stop_signal: Arc<Mutex<bool>>,
}

impl Drop for MockServer {
    fn drop(&mut self) {
        // Flip the stop flag and send a benign connect so `accept` unblocks;
        // any failure is fine because the worker re-checks the flag each loop.
        *self._stop_signal.lock().unwrap() = true;
        if let Some(handle) = self.handle.take() {
            let _ = TcpStream::connect(self.base_url.trim_start_matches("http://"));
            handle.join().ok();
        }
    }
}

impl MockServer {
    /// Snapshot the request log the worker accumulated.
    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

/// Bind a local TCP listener on an OS-assigned port and spawn one worker
/// thread that drains it. The worker exits when the stop flag flips.
pub fn start_mock_server(responses: Vec<MockResponse>) -> MockServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock listener");
    let address = listener.local_addr().expect("mock listener address");
    listener
        .set_nonblocking(false)
        .expect("set listener blocking");

    let queue: Arc<Mutex<VecDeque<MockResponse>>> =
        Arc::new(Mutex::new(responses.into_iter().collect()));
    let requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(Mutex::new(false));

    let queue_thread = Arc::clone(&queue);
    let requests_thread = Arc::clone(&requests);
    let stop_thread = Arc::clone(&stop);

    let handle = thread::spawn(move || {
        loop {
            if *stop_thread.lock().unwrap() {
                break;
            }
            let (mut stream, _) = match listener.accept() {
                Ok(conn) => conn,
                Err(_) => continue,
            };
            let request = read_request(&mut stream);
            requests_thread.lock().unwrap().push(request);
            let response = queue_thread
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| MockResponse::ok("{}"));
            write_response(&mut stream, response);
        }
    });

    MockServer {
        base_url: format!("http://{address}"),
        requests,
        handle: Some(handle),
        _stop_signal: stop,
    }
}

/// Read one HTTP request off `stream` until the headers-terminator and
/// `Content-Length` bytes have both arrived. Tests assert on substrings
/// (`"PUT /issues/4242.json"`, `"x-redmine-api-key: ..."`) which is
/// sufficient for a local-only mock.
fn read_request(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let size = match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        bytes.extend_from_slice(&chunk[..size]);
        if request_complete(&bytes) {
            break;
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn request_complete(bytes: &[u8]) -> bool {
    let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .and_then(|value| value.trim().parse::<usize>().ok())
        })
        .unwrap_or(0);
    bytes.len() >= header_end + 4 + content_length
}

fn write_response(stream: &mut TcpStream, response: MockResponse) {
    let status_text = match response.status {
        200..=299 => "OK",
        404 => "Not Found",
        403 => "Forbidden",
        422 => "Unprocessable Entity",
        _ => "Internal Server Error",
    };
    let body = response.body;
    let headers = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        status_text,
        body.len(),
    );
    stream.write_all(headers.as_bytes()).unwrap();
    stream.write_all(body.as_bytes()).unwrap();
}

/// Per-test SQLite database living inside an isolated temp directory;
/// `Drop` removes the directory so concurrent runs never share a file.
pub struct TestDb {
    pub path: PathBuf,
    _temp_dir: PathBuf,
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self._temp_dir);
    }
}

/// Create a fresh SQLite database inside a process-local temp dir and
/// seed the four role config rows. The dir name mixes `(pid, ns, hash)`
/// so collisions never share a database.
pub fn make_test_db(api_base: &str) -> TestDb {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_dir = std::env::temp_dir().join(format!(
        "phasegent-it-lifecycle-{}-{}-{}",
        std::process::id(),
        nanos,
        (nanos as u64) ^ (std::process::id() as u64),
    ));
    fs::create_dir_all(&temp_dir).expect("create temp dir");
    let path = temp_dir.join("phasegent.sqlite3");
    let conn = Connection::open(&path).expect("open sqlite db");
    conn.execute_batch(PRAGMA_SQL).expect("set sqlite pragmas");
    conn.execute_batch(SCHEMA_SQL)
        .expect("create sqlite schema");

    seed_role(&conn, "orchestrator", api_base, ORCHESTRATOR_KEY);
    seed_role(&conn, "executor", api_base, EXECUTOR_KEY);
    seed_role(&conn, "reviewer", api_base, REVIEWER_KEY);
    seed_role(&conn, "admin", api_base, ADMIN_KEY);

    TestDb {
        path,
        _temp_dir: temp_dir,
    }
}

fn seed_role(conn: &Connection, role: &str, api_base: &str, api_key: &str) {
    conn.execute(
        "INSERT OR REPLACE INTO role_config (role, provider, api_base) VALUES (?1, ?2, ?3)",
        rusqlite::params![role, "redmine", api_base],
    )
    .expect("seed role_config");
    conn.execute(
        "INSERT OR REPLACE INTO role_redmine_config (role, api_base, project_id, close_status_id) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![role, api_base, PROJECT_ID, CLOSE_STATUS_ID],
    )
    .expect("seed role_redmine_config");
    conn.execute(
        "INSERT OR REPLACE INTO role_credential (role, provider, credential) VALUES (?1, ?2, ?3)",
        rusqlite::params![role, "redmine", api_key],
    )
    .expect("seed role_credential");
}

/// JSON body a mock Redmine returns for one issue. `is_closed` mirrors
/// the source `issue_statuses.json` row.
pub fn issue_response_with_status(
    issue_id: u64,
    status_id: u64,
    status_name: &str,
    is_closed: bool,
) -> String {
    serde_json::json!({
        "issue": {
            "id": issue_id,
            "subject": "Test issue",
            "description": "Body",
            "status": {
                "id": status_id,
                "name": status_name,
                "is_closed": is_closed,
            },
            "journals": []
        }
    })
    .to_string()
}

/// Canonical status list returned by the mock `/issue_statuses.json`
/// endpoint. Ids match the constants above.
pub fn statuses_response() -> String {
    serde_json::json!({
        "issue_statuses": [
            {"id": STATUS_NEW, "name": "New", "is_closed": false},
            {"id": STATUS_IN_PROGRESS, "name": "In Progress", "is_closed": false},
            {"id": STATUS_IN_REVIEW, "name": "In Review", "is_closed": false},
            {"id": STATUS_CHANGES_REQUESTED, "name": "Changes Requested", "is_closed": false},
            {"id": STATUS_BLOCKED, "name": "Blocked", "is_closed": false},
            {"id": STATUS_RESOLVED, "name": "Resolved", "is_closed": true},
            {"id": STATUS_CLOSED, "name": "Closed", "is_closed": true},
            {"id": STATUS_CANCELLED, "name": "Cancelled", "is_closed": true},
        ]
    })
    .to_string()
}

/// Absolute path to the compiled `phasegent` binary, resolved by Cargo
/// at compile time so `cargo run` cannot fall back to the wrong binary.
pub fn phasegent_bin() -> &'static str {
    env!("CARGO_BIN_EXE_phasegent")
}

/// Run the compiled binary with isolated `PHASEGENT_DB_PATH` plus a clean
/// Redmine env so a developer's shell cannot leak overrides into the subprocess.
pub fn run_cli(db_path: &Path, api_base: &str, args: &[&str]) -> std::process::Output {
    let mut command = Command::new(phasegent_bin());
    command
        .args(args)
        .env("PHASEGENT_DB_PATH", db_path.as_os_str())
        // Pin to the test api_base so resolver changes cannot drift the mock URL.
        .env("PHASEGENT_REDMINE_API_BASE", api_base)
        .env_remove("PHASEGENT_PROVIDER")
        .env_remove("PHASEGENT_DEFAULT_PROVIDER")
        .env_remove("PHASEGENT_REDMINE_PROJECT_ID")
        .env_remove("PHASEGENT_PROJECT_ID")
        .env_remove("PHASEGENT_REDMINE_CLOSE_STATUS_ID")
        .env_remove("PHASEGENT_CLOSE_STATUS_ID")
        .env_remove("PHASEGENT_API_BASE")
        .env_remove("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY")
        .env_remove("PHASEGENT_REDMINE_REPOSITORY_URL")
        .env("RUST_BACKTRACE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.output().expect("spawn phasegent binary")
}

pub fn stdout_text(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn stderr_text(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
