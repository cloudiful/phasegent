//! Injectable boundary over the OpenCode V2 session API (issue 747 P1).
//!
//! Three operations cover the whole cleanup contract, and nothing else is
//! reachable from this module:
//!
//! * [`OpenCodeApi::session`] — the authoritative directory the host
//!   reports for one session. A missing session is *uncertainty*, never
//!   evidence that a directory is free: the selected instance may simply
//!   not own it.
//! * [`OpenCodeApi::list`] — one page of sessions for a directory. The
//!   caller must follow `next_cursor` to exhaustion before treating the
//!   result as occupancy evidence.
//! * [`OpenCodeApi::move_session`] — the only mutating call. Accepting it
//!   is not proof that the session relocated, so the caller re-reads the
//!   location afterwards.
//!
//! [`ProcessOpenCodeApi`] is the production implementation; tests inject
//! their own, so no test ever moves a real session.

use std::path::{Path, PathBuf};

use super::parse::{parse_session_get, parse_session_page};
use super::process::run_bounded;

/// Page size requested from `session.list`. Bounding the page keeps one
/// response small enough to parse from a single bounded read.
pub(crate) const MAX_PAGE_SIZE: usize = 200;

/// One session reduced to the two fields the occupancy check compares.
/// `id` identifies the lease association, `directory` is the host's
/// authoritative answer about where the session actually is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocatedSession {
    pub(crate) id: String,
    pub(crate) directory: String,
}

/// One `session.list` page plus the opaque cursor of the next page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionPage {
    pub(crate) sessions: Vec<LocatedSession>,
    /// `None` once the server reported the final page. A `Some` cursor the
    /// caller cannot follow makes the page set incomplete.
    pub(crate) next_cursor: Option<String>,
}

/// Query parameters for one `session.list` call.
///
/// An empty `directory` means *no directory filter*: the caller wants
/// every session the instance knows. Sending an empty value instead would
/// be a filter that matches nothing, which reads as an empty result — the
/// one reading a cleanup must never infer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ListRequest {
    pub(crate) directory: String,
    pub(crate) cursor: Option<String>,
}

/// A bounded, never-panicking failure of one API interaction. The message
/// is short enough to embed in a stderr warning and names the failure kind
/// so an operator can tell "the host said no" from "we could not read the
/// host's answer".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApiError {
    pub(crate) message: String,
}

impl ApiError {
    pub(crate) fn plain(message: &str) -> Self {
        Self {
            message: crate::worktree::bounded(message),
        }
    }

    pub(crate) fn new(kind: &str, detail: &dyn std::fmt::Display) -> Self {
        Self {
            message: crate::worktree::bounded(&format!("{kind}: {detail}")),
        }
    }
}

impl From<String> for ApiError {
    fn from(message: String) -> Self {
        Self::plain(&message)
    }
}

/// The injectable API surface the cleanup drives.
pub(crate) trait OpenCodeApi {
    /// `GET /api/session/{id}`.
    fn session(&self, id: &str) -> Result<LocatedSession, ApiError>;

    /// `GET /api/session?[directory=...]&cursor=...`; no `directory`
    /// parameter at all when the request asks for the unfiltered listing.
    fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError>;

    /// `POST /api/session/{id}/move`.
    fn move_session(&self, id: &str, directory: &Path) -> Result<(), ApiError>;
}

/// Production boundary: the installed `opencode` client, reached through
/// its own service discovery and authentication. Arguments are passed as
/// an argv array and the JSON body as a serialized `--data` value, so
/// nothing is ever interpreted by a shell.
pub(crate) struct ProcessOpenCodeApi {
    program: PathBuf,
}

impl ProcessOpenCodeApi {
    pub(crate) fn new() -> Self {
        Self {
            program: PathBuf::from("opencode"),
        }
    }

    fn get(&self, operation: &str, params: &[(&str, String)]) -> Result<String, ApiError> {
        let mut args = vec!["api".to_owned(), operation.to_owned()];
        for (key, value) in params {
            args.push("--param".to_owned());
            args.push(format!("{key}={value}"));
        }
        let output = run_bounded(&self.program, &args)?;
        check_status(operation, output.status, &output.stdout)
    }
}

/// A non-2xx response exits non-zero and prints an encoded error document
/// (`{"_tag": "SessionNotFoundError", ...}`). Surfacing the tag keeps the
/// bounded warning specific enough for an operator to tell a missing
/// session from a transport problem.
fn check_status(operation: &str, status: i32, body: &str) -> Result<String, ApiError> {
    if status == 0 {
        return Ok(body.to_owned());
    }
    let detail = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("_tag")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| format!("exit status {status}"));
    Err(ApiError::new(operation, &detail))
}

impl Default for ProcessOpenCodeApi {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenCodeApi for ProcessOpenCodeApi {
    fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
        let body = self.get("session.get", &[("sessionID", id.to_owned())])?;
        parse_session_get(&body)
    }

    fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
        let body = self.get("session.list", &request_params(request))?;
        parse_session_page(&body)
    }

    fn move_session(&self, id: &str, directory: &Path) -> Result<(), ApiError> {
        let body = serde_json::json!({ "directory": directory.to_string_lossy() });
        let data = serde_json::to_string(&body)
            .map_err(|error| ApiError::new("could not serialize move request", &error))?;
        let output = run_bounded(
            &self.program,
            &[
                "api".to_owned(),
                "session.move".to_owned(),
                "--param".to_owned(),
                format!("sessionID={id}"),
                "--data".to_owned(),
                data,
            ],
        )?;
        // A 204 prints no body; only the exit status proves acceptance,
        // and acceptance is still not proof that the session relocated.
        check_status("session.move", output.status, &output.stdout).map(|_| ())
    }
}

fn request_params(request: &ListRequest) -> Vec<(&'static str, String)> {
    let mut params = vec![("limit", MAX_PAGE_SIZE.to_string())];
    if !request.directory.is_empty() {
        params.push(("directory", request.directory.clone()));
    }
    if let Some(cursor) = request.cursor.clone() {
        params.push(("cursor", cursor));
    }
    params
}
