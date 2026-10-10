//! Response shapes of the OpenCode V2 session endpoints.
//!
//! `opencode api` prints the raw response body on stdout and the HTTP
//! status line on stderr, and exits non-zero for any non-2xx status. Only
//! stdout reaches this module, and a non-zero exit is already a structured
//! error, so an encoded error document is never mistaken for a payload.
//!
//! Anything that is not exactly the documented shape — invalid JSON, a
//! missing `data`, a session without the required `location.directory`, a
//! `SessionsResponse` without `data`/`cursor` — is an error. The cleanup
//! treats every error as uncertainty and keeps the directory, so a
//! lenient parser could only ever remove a directory it failed to
//! understand.

use serde_json::Value;

use super::api::{ApiError, LocatedSession, SessionPage};

/// Parse the body of a successful `session.get` response
/// (`{"data": {..., "location": {"directory": "..."}}}`).
pub(super) fn parse_session_get(body: &str) -> Result<LocatedSession, ApiError> {
    located_session(&require_data(body)?)
}

/// Parse one `session.list` response (`{"data": [...], "cursor": {...}}`).
/// Pagination completeness is judged by the caller that owns the walk,
/// because only it sees every cursor the walk has followed.
pub(super) fn parse_session_page(body: &str) -> Result<SessionPage, ApiError> {
    let object = parse_object(body)?;
    let data = object
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::plain("session list response has no data array"))?;
    let next = match object.get("cursor") {
        Some(cursor) => next_cursor(cursor)?,
        None => {
            return Err(ApiError::plain(
                "session list response has no cursor; pagination is unproven",
            ));
        }
    };
    let sessions = data
        .iter()
        .map(located_session)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SessionPage {
        sessions,
        next_cursor: next,
    })
}

fn parse_object(body: &str) -> Result<serde_json::Map<String, Value>, ApiError> {
    let value: Value =
        serde_json::from_str(body).map_err(|error| ApiError::new("invalid JSON", &error))?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| ApiError::plain("session response is not a JSON object"))
}

fn next_cursor(cursor: &Value) -> Result<Option<String>, ApiError> {
    match cursor.get("next") {
        Some(Value::Null) | None => Ok(None),
        Some(Value::String(value)) if value.is_empty() => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(ApiError::plain("session list cursor.next is not a string")),
    }
}

/// Reduce any session document to the `(id, directory)` pair the occupancy
/// check compares. `session.get`'s single `data` object and
/// `session.list`'s per-item objects carry the same required fields.
fn located_session(value: &Value) -> Result<LocatedSession, ApiError> {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| ApiError::plain("session has no id"))?;
    let directory = value
        .get("location")
        .and_then(|location| location.get("directory"))
        .and_then(Value::as_str)
        .filter(|directory| !directory.trim().is_empty())
        .ok_or_else(|| ApiError::plain("session has no location.directory"))?;
    Ok(LocatedSession {
        id: id.to_owned(),
        directory: directory.to_owned(),
    })
}

fn require_data(body: &str) -> Result<Value, ApiError> {
    parse_object(body)?
        .get("data")
        .cloned()
        .ok_or_else(|| ApiError::plain("session response has no data field"))
}
