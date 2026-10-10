//! Complete pagination over the OpenCode instance's sessions.
//!
//! Occupancy is only usable as removal evidence when the page walk is
//! complete: every `cursor.next` followed until the server stops offering
//! one. Three things make a walk incomplete and therefore unusable, and
//! all three are rejected here rather than turned into a partial answer:
//!
//! * a failed page — the host could not answer at all;
//! * a repeated cursor — the server keeps handing back the same page, so
//!   the walk would never terminate;
//! * the page ceiling — the listing is larger than this module is willing
//!   to read, which is uncertainty, not an empty directory.
//!
//! The walk is deliberately **unfiltered**: the server is asked for every
//! session it knows and the directory comparison happens here. A
//! server-side `directory` filter is an optimisation the host may spell,
//! scope, or apply inconsistently, and a filter that silently omits a
//! row would otherwise turn "this host cannot list that session" into
//! "this directory is empty" — the exact reading a deletion cannot
//! afford. See [`ListRequest::directory`] for how "no filter" is
//! requested.

use std::path::Path;

use super::api::{ApiError, ListRequest, LocatedSession, OpenCodeApi, SessionPage};
use crate::worktree::same_directory;

/// Ceiling on pages read for one walk. Together with [`super::api::MAX_PAGE_SIZE`]
/// this bounds both the calls and the memory a cleanup pass can spend on
/// occupancy.
pub(crate) const MAX_LIST_PAGES: usize = 25;

/// Every session the selected OpenCode instance reports, or an error
/// when that set cannot be proven complete.
///
/// No directory filter is sent, so the answer cannot be narrowed by the
/// host: what comes back is every session the instance knows, which is
/// the superset a destructive "this directory is free" claim has to be
/// checked against.
pub(crate) fn all_sessions(api: &dyn OpenCodeApi) -> Result<Vec<LocatedSession>, ApiError> {
    let mut collected: Vec<LocatedSession> = Vec::new();
    let mut followed: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_LIST_PAGES {
        let page: SessionPage = api.list(&ListRequest {
            directory: String::new(),
            cursor: cursor.clone(),
        })?;
        collected.extend(page.sessions);
        match page.next_cursor {
            None => return Ok(collected),
            Some(next) => {
                if followed.contains(&next) {
                    return Err(ApiError::plain(
                        "session list repeated a cursor; the page set is incomplete",
                    ));
                }
                followed.push(next.clone());
                cursor = Some(next);
            }
        }
    }
    Err(ApiError::plain(
        "session list did not finish within the page budget; the page set is incomplete",
    ))
}

/// Every session in the complete listing that names `directory`, or an
/// error when that set cannot be proven complete.
///
/// The client-side comparison is deliberate: the directory the host
/// reports for a session and the directory this module compares against
/// may differ in spelling (symlinks, trailing separators), so the answer
/// is reduced here rather than by a filter the host applies.
pub(crate) fn occupants_of_directory(
    api: &dyn OpenCodeApi,
    directory: &Path,
) -> Result<Vec<LocatedSession>, ApiError> {
    Ok(all_sessions(api)?
        .into_iter()
        .filter(|session| same_directory(Path::new(&session.directory), directory))
        .collect())
}
