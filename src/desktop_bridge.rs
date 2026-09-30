//! Hidden `desktop-bridge` stdio mode for the packaged desktop shell.
//!
//! The desktop shell spawns this binary with the `desktop-bridge` token and
//! speaks bounded newline-delimited JSON over stdin/stdout: one request object
//! per line, one response object per line, diagnostics on stderr only.
//!
//! ```text
//! request   {"id": 1, "method": "get_tasks", "params": {"request": {"limit": 20}}}
//! response  {"id": 1, "protocol": 1, "ok": true, "result": {...}}
//!           {"id": 1, "protocol": 1, "ok": false, "error": {"kind": ..., "message": ...}}
//! ```
//!
//! Recorded decisions: the method set is versioned ([`PROTOCOL_VERSION`]) and
//! allowlisted by [`METHODS`] with no generic passthrough, so a renderer can
//! never reach an arbitrary CLI command, credential store operation, or
//! provider call through the bridge; `params` keeps the parameter names the
//! Electron preload passes (`request` / `query`) and the responses reuse the
//! same redacted payload structs, so the transport swap does not change
//! payload, redaction, or error semantics; requests are handled by
//! [`WORKER_COUNT`] workers and every response echoes the request `id`, so
//! correlation never depends on order; and a line over [`MAX_LINE_BYTES`] is
//! drained and rejected (`too_large`) instead of being buffered.
//!
//! Error kinds: `parse` (not UTF-8 JSON, or not an object), `method` (missing
//! or unknown method), `argument` (parameter shape/type), `too_large`,
//! `backend` (the reused backend's bounded error string), and `encode`.
//!
//! Lifecycle: EOF on stdin drains in-flight requests, flushes stdout, and
//! exits 0; an I/O failure exits 1 after a bounded stderr diagnostic; an
//! unexpected argument exits 2.

use std::io::{BufRead, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex, MutexGuard};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Hidden argv token that selects this stdio mode before normal CLI parsing.
pub(crate) const COMMAND: &str = "desktop-bridge";

/// Wire protocol version carried by every response.
pub(crate) const PROTOCOL_VERSION: u32 = 1;

/// Largest accepted request line, excluding the terminating newline.
const MAX_LINE_BYTES: usize = 64 * 1024;

/// Bounded request backlog; the reader blocks instead of buffering.
const QUEUE_DEPTH: usize = 64;

/// Concurrent handlers: backend calls block on storage and provider I/O, so a
/// few workers keep one slow request from delaying the others.
const WORKER_COUNT: usize = 4;

/// Versioned method allowlist. Names match the Electron preload method table
/// (`electron/shared/methods.ts`) and there is intentionally no wildcard entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Method {
    AppMetadata,
    ConfigSnapshot,
    BranchContext,
    Tasks,
    Status,
    SetSetting,
    ClearSetting,
    SetCredential,
    ClearCredential,
    Provisioning,
}

/// Every accepted method with its wire name, in protocol order.
const METHODS: &[(Method, &str)] = &[
    (Method::AppMetadata, "get_app_metadata"),
    (Method::ConfigSnapshot, "get_config_snapshot"),
    (Method::BranchContext, "get_branch_context"),
    (Method::Tasks, "get_tasks"),
    (Method::Status, "get_status"),
    (Method::SetSetting, "set_config_setting"),
    (Method::ClearSetting, "clear_config_setting"),
    (Method::SetCredential, "set_credential"),
    (Method::ClearCredential, "clear_credential"),
    (Method::Provisioning, "get_provisioning_status"),
];

impl Method {
    fn parse(name: &str) -> Option<Self> {
        METHODS
            .iter()
            .find(|(_, wire)| *wire == name)
            .map(|(method, _)| *method)
    }

    fn name(self) -> &'static str {
        METHODS
            .iter()
            .find(|(method, _)| *method == self)
            .map(|(_, wire)| *wire)
            .unwrap_or("unknown")
    }
}

/// Bounded protocol error: `kind` is machine-readable, `message` is the reused
/// backend text (already redacted by [`crate::gui`]).
#[derive(Debug)]
struct BridgeError {
    kind: &'static str,
    message: String,
}

impl BridgeError {
    fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

/// Parse and dispatch one request line. Always returns a response object;
/// malformed input is answered with a null id because correlation is
/// impossible without a parseable envelope.
pub(crate) fn handle_line(line: &[u8]) -> Value {
    let Ok(text) = std::str::from_utf8(line) else {
        return error_response(Value::Null, "parse", "request must be UTF-8 encoded JSON");
    };
    match serde_json::from_str::<Value>(text) {
        Ok(request) => handle_request(request),
        Err(error) => error_response(
            Value::Null,
            "parse",
            format!("invalid JSON request: {error}"),
        ),
    }
}

fn handle_request(request: Value) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let Some(object) = request.as_object() else {
        return error_response(id, "parse", "request must be a JSON object");
    };
    let Some(name) = object.get("method").and_then(Value::as_str) else {
        return error_response(id, "method", "request must carry a string 'method'");
    };
    let Some(method) = Method::parse(name) else {
        let allowed = METHODS
            .iter()
            .map(|(_, wire)| *wire)
            .collect::<Vec<_>>()
            .join(", ");
        return error_response(
            id,
            "method",
            format!("unknown method '{name}'; allowed methods: {allowed}"),
        );
    };
    let params = object.get("params").cloned().unwrap_or(Value::Null);
    match dispatch(method, params) {
        Ok(result) => response(id, result),
        Err(error) => error_response(id, error.kind, error.message),
    }
}

fn dispatch(method: Method, params: Value) -> Result<Value, BridgeError> {
    match method {
        Method::AppMetadata => {
            require_no_params(method, &params)?;
            ok_json(crate::gui::app_metadata())
        }
        Method::ConfigSnapshot => {
            require_no_params(method, &params)?;
            map_backend(crate::gui::read_config_snapshot())
        }
        Method::BranchContext => {
            require_no_params(method, &params)?;
            map_backend(crate::gui::read_branch_context())
        }
        Method::Tasks => {
            let request = decode(method, params, "request")?;
            map_backend(crate::gui::read_tasks_blocking(request))
        }
        Method::Status => {
            let request = decode(method, params, "request")?;
            map_backend(crate::gui::read_status_blocking(request))
        }
        Method::SetSetting => {
            let request = decode(method, params, "request")?;
            map_backend(crate::gui::write_setting_blocking(request))
        }
        Method::ClearSetting => {
            let request = decode(method, params, "request")?;
            map_backend(crate::gui::clear_setting_blocking(request))
        }
        Method::SetCredential => {
            let request = decode(method, params, "request")?;
            map_backend(crate::gui::write_credential_blocking(request))
        }
        Method::ClearCredential => {
            let request = decode(method, params, "request")?;
            map_backend(crate::gui::clear_credential_blocking(request))
        }
        Method::Provisioning => {
            let query = decode(method, params, "query")?;
            map_backend(crate::gui::read_provisioning_blocking(query))
        }
    }
}

/// Methods without parameters accept an absent, null, or empty `params`;
/// anything else is a protocol error instead of being silently ignored.
fn require_no_params(method: Method, params: &Value) -> Result<(), BridgeError> {
    if params.is_null() || params.as_object().is_some_and(|map| map.is_empty()) {
        return Ok(());
    }
    Err(BridgeError::new(
        "argument",
        format!("{} takes no parameters", method.name()),
    ))
}

/// Typed parameter decoding: `params` must be a JSON object holding the
/// documented parameter name (`request` / `query`). An absent payload decodes
/// as an empty object so the defaulted request structs keep the existing call
/// shapes, while a wrong shape or type fails loudly.
fn decode<T: DeserializeOwned>(
    method: Method,
    params: Value,
    field: &str,
) -> Result<T, BridgeError> {
    let object = match params {
        Value::Null => serde_json::Map::new(),
        Value::Object(map) => map,
        _ => {
            return Err(BridgeError::new(
                "argument",
                format!("{} params must be a JSON object", method.name()),
            ));
        }
    };
    let payload = match object.get(field) {
        None | Some(Value::Null) => Value::Object(serde_json::Map::new()),
        Some(value) => value.clone(),
    };
    serde_json::from_value(payload).map_err(|error| {
        BridgeError::new(
            "argument",
            format!("{} parameter '{field}' is invalid: {error}", method.name()),
        )
    })
}

fn ok_json<T: Serialize>(value: T) -> Result<Value, BridgeError> {
    serde_json::to_value(value).map_err(|error| BridgeError::new("encode", error.to_string()))
}

fn map_backend<T: Serialize>(result: Result<T, String>) -> Result<Value, BridgeError> {
    ok_json(result.map_err(|message| BridgeError::new("backend", message))?)
}

fn response(id: Value, result: Value) -> Value {
    serde_json::json!({"id": id, "protocol": PROTOCOL_VERSION, "ok": true, "result": result})
}

fn error_response(id: Value, kind: &'static str, message: impl AsRef<str>) -> Value {
    serde_json::json!({
        "id": id,
        "protocol": PROTOCOL_VERSION,
        "ok": false,
        "error": {"kind": kind, "message": crate::gui::bound_message(message)},
    })
}

/// Entry point for the hidden `desktop-bridge` argv token.
pub(crate) fn run(args: &[String]) -> i32 {
    if !args.is_empty() {
        eprintln!(
            "{}",
            serde_json::json!({"error":{"kind":"argument", "message":format!("{COMMAND} takes no arguments")}})
        );
        return 2;
    }
    serve(std::io::stdin().lock(), std::io::stdout(), WORKER_COUNT)
}

#[derive(Debug)]
enum LineOutcome {
    Line(Vec<u8>),
    Oversized,
    Eof,
}

/// Serve one bridge session; returns the process exit code.
fn serve<R, W>(mut reader: R, writer: W, workers: usize) -> i32
where
    R: BufRead,
    W: Write + Send + 'static,
{
    let output = Arc::new(Mutex::new(writer));
    let failed = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = sync_channel::<Vec<u8>>(QUEUE_DEPTH);
    let receiver = Arc::new(Mutex::new(receiver));
    let mut handles = Vec::with_capacity(workers.max(1));
    for _ in 0..workers.max(1) {
        let queue = Arc::clone(&receiver);
        let output = Arc::clone(&output);
        let failed = Arc::clone(&failed);
        handles.push(std::thread::spawn(move || {
            loop {
                let request = {
                    let queue = lock(&queue);
                    queue.recv()
                };
                let Ok(line) = request else { break };
                emit(&output, &handle_line(&line), &failed);
            }
        }));
    }
    let mut read_failed = false;
    loop {
        match read_bounded_line(&mut reader, MAX_LINE_BYTES) {
            Ok(LineOutcome::Eof) => break,
            Ok(LineOutcome::Oversized) => {
                let response = error_response(
                    Value::Null,
                    "too_large",
                    format!("request line exceeds {MAX_LINE_BYTES} bytes"),
                );
                emit(&output, &response, &failed);
            }
            Ok(LineOutcome::Line(line)) => {
                if sender.send(line).is_err() {
                    break;
                }
            }
            Err(error) => {
                eprintln!(
                    "{}",
                    serde_json::json!({"bridge":{"kind":"io", "message":crate::gui::bound_message(format!("stdin read failed: {error}"))}})
                );
                read_failed = true;
                break;
            }
        }
    }
    drop(sender);
    for handle in handles {
        let _ = handle.join();
    }
    let _ = lock(&output).flush();
    if read_failed || failed.load(Ordering::SeqCst) {
        1
    } else {
        0
    }
}

/// Read one newline-terminated line without ever buffering more than `max`
/// content bytes: an overlong line is drained to the next line boundary and
/// reported as oversized instead of being accumulated.
fn read_bounded_line<R: BufRead>(reader: &mut R, max: usize) -> std::io::Result<LineOutcome> {
    let mut line: Vec<u8> = Vec::new();
    // `Take` bounds one read to the limit plus CRLF, leaving remaining
    // buffered bytes in place for the next call.
    let read = reader
        .by_ref()
        .take(max as u64 + 2)
        .read_until(b'\n', &mut line)?;
    if read == 0 {
        return Ok(LineOutcome::Eof);
    }
    let mut terminated = false;
    if line.last() == Some(&b'\n') {
        line.pop();
        terminated = true;
        if line.last() == Some(&b'\r') {
            line.pop();
        }
    }
    if line.len() <= max {
        return Ok(LineOutcome::Line(line));
    }
    // Overlong: the terminator is already consumed when `terminated`, so the
    // remaining bytes drain only when the line is still open.
    if !terminated {
        loop {
            let chunk = reader.fill_buf()?;
            if chunk.is_empty() {
                break;
            }
            let Some(newline) = chunk.iter().position(|byte| *byte == b'\n') else {
                let len = chunk.len();
                reader.consume(len);
                continue;
            };
            reader.consume(newline + 1);
            break;
        }
    }
    Ok(LineOutcome::Oversized)
}

fn write_response<W: Write>(output: &Mutex<W>, response: &Value) -> std::io::Result<()> {
    let encoded = serde_json::to_vec(response)?;
    let mut guard = lock(output);
    guard.write_all(&encoded)?;
    guard.write_all(b"\n")?;
    guard.flush()
}

/// Write one response, reporting the first stdout failure on stderr exactly
/// once. Diagnostics never touch stdout.
fn emit<W: Write>(output: &Mutex<W>, response: &Value, failed: &AtomicBool) {
    if let Err(error) = write_response(output, response)
        && !failed.swap(true, Ordering::SeqCst)
    {
        eprintln!(
            "{}",
            serde_json::json!({"bridge":{"kind":"io", "message":crate::gui::bound_message(format!("stdout write failed: {error}"))}})
        );
    }
}

/// A poisoned lock means a handler panicked mid-write; recover the value so
/// the session keeps reporting instead of cascading the panic.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
#[path = "desktop_bridge_tests.rs"]
mod tests;
