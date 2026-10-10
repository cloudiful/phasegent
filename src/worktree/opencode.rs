//! Injectable boundary over the OpenCode V2 API (issue 747 P1).
//!
//! A closed issue's worktree is only safe to delete once the OpenCode
//! session that lives in it is provably somewhere else, so the cleanup
//! needs to ask the host where a session actually is instead of trusting a
//! cached worktree path. The host is authoritative and this module stays
//! thin: it shells out to the already-installed `opencode api` client
//! (which owns service discovery and authentication), never to a raw
//! provider API, and never interpolates a shell.
//!
//! Three V2 operations are used, all of them existing contracts rather
//! than schemas this crate defines:
//!
//! * `session.get` — the authoritative `location.directory` of one
//!   session. A missing session is *uncertainty*, never evidence that the
//!   directory is free: the selected instance may simply not own it.
//! * `session.list` — complete pagination over one directory's occupants.
//!   A page sequence with a remaining `cursor.next`, or a repeated cursor,
//!   is incomplete and therefore unusable as occupancy evidence.
//! * `session.move` — the only mutating call in the module. A 204 is a
//!   *request* the host accepted, not proof that the session relocated, so
//!   callers must re-read the location afterwards.
//!
//! Everything is bounded: child processes cannot outlive a timeout, and
//! captured output cannot grow without limit. No credential, config, or
//! environment value is read here.

#[path = "opencode/api.rs"]
mod api;
#[path = "opencode/occupancy.rs"]
mod occupancy;
#[path = "opencode/parse.rs"]
mod parse;
#[path = "opencode/process.rs"]
mod process;

#[cfg(test)]
#[path = "opencode/test_support.rs"]
pub(crate) mod test_support;

#[cfg(test)]
#[path = "opencode/tests.rs"]
mod tests;

// The re-exports are the module's crate-internal surface: the cleanup
// guard and the injected test double both name them from here, so a glob
// import in a child module is invisible to the unused-import lint.
#[allow(unused_imports)]
pub(crate) use api::{
    ApiError, ListRequest, LocatedSession, MAX_PAGE_SIZE, OpenCodeApi, ProcessOpenCodeApi,
    SessionPage,
};
#[allow(unused_imports)]
pub(crate) use occupancy::{MAX_LIST_PAGES, occupants_of_directory};

/// Prefix every OpenCode session id carries (`GET /api/session/{id}`
/// validates the same pattern). A lease session that does not start with
/// it belongs to a plain CLI caller, which keeps the pre-existing cleanup
/// behaviour and never reaches the API at all.
pub(crate) const OPENCODE_SESSION_PREFIX: &str = "ses_";

/// True when `session` is an OpenCode-managed session id and therefore a
/// cleanup candidate for API-backed occupancy checks.
pub(crate) fn is_opencode_session(session: &str) -> bool {
    session.starts_with(OPENCODE_SESSION_PREFIX) && session.len() > OPENCODE_SESSION_PREFIX.len()
}
