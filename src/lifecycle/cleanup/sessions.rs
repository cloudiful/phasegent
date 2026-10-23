//! The OpenCode guard: is a candidate directory still hosting a session?
//!
//! A lease row says where a session was when the worktree was acquired.
//! It does not say where the session is now, because a plugin, a
//! `session.move`, or the operator can relocate a live session at any
//! time. Deleting a directory a live session still points at strands
//! that session, so this guard treats the host — not the lease — as the
//! authority, and is deliberately unable to answer "it is fine" unless
//! the evidence is complete:
//!
//! * a candidate is *managed* only when a persisted lease associates an
//!   OpenCode session with it. The close's own optional session argument
//!   names who may move a session, never what this directory is, so it
//!   can never turn a pure CLI candidate into an API one.
//! * every associated session id must answer `session.get`. A session the
//!   selected instance does not know may simply live in a standalone or
//!   private instance, so "not found" is uncertainty, never absence.
//! * `session.get`'s directory is the authority, and it is *kept*. The
//!   listing cannot overrule it: a host whose own directory scoping
//!   under-reports answers `session.get` for a session its listing omits,
//!   and an empty listing must never read as "this directory is free".
//! * the occupancy listing is paginated to completion and unfiltered —
//!   every session the instance knows, compared here. A failed page, a
//!   repeated cursor, an exhausted page budget, or a host that narrows
//!   the answer make the listing incomplete, which is not an empty
//!   directory.
//! * an occupant that no lease of this repository associates is somebody
//!   else's session and blocks removal without being moved;
//! * only the closing session may be asked to move, only out of the
//!   directory that still hosts it, and only to a verified main checkout;
//! * a move request is not proof it took effect, so every direct location
//!   is read again afterwards and a fresh complete listing must agree
//!   that the session is at the target and the directory is empty.
//!   Otherwise the candidate stays and a later pass retries.
//!
//! `worktree prune` reuses that evidence to *return* an associated idle
//! session to the verified main checkout before it removes a directory: only
//! a lease-associated `ses_` session may be moved, a running or foreign
//! occupant keeps the directory, and every answer that names the session and
//! reports a usable location is checked before it authorises a move.

use std::path::Path;

use super::CleanupMode;
use super::target::TargetResolver;
use crate::worktree::opencode::{
    ApiError, LocatedSession, OpenCodeApi, is_opencode_session, occupants_of_directory,
};
use crate::worktree::same_directory;

/// The guard's answer for one candidate directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionVerdict {
    /// No persisted lease associates an OpenCode session with the
    /// directory: a pure CLI candidate, decided exactly as before.
    Unmanaged,
    /// Every associated session is authoritatively elsewhere, so the
    /// directory may be removed.
    Vacant,
    /// The directory is kept, with the bounded reason that decided it.
    Keep(String),
}

impl SessionVerdict {
    /// The keep reason, or `None` when removal may proceed.
    pub(super) fn keep_reason(self) -> Option<String> {
        match self {
            Self::Unmanaged | Self::Vacant => None,
            Self::Keep(reason) => Some(reason),
        }
    }
}

pub(super) fn evaluate(
    api: &dyn OpenCodeApi,
    mode: CleanupMode,
    target: &TargetResolver<'_>,
    directory: &Path,
    associated: &[String],
    closing: Option<&str>,
) -> SessionVerdict {
    let closing = closing
        .map(str::trim)
        .filter(|session| !session.is_empty())
        .filter(|session| is_opencode_session(session));
    // Only a persisted lease makes this candidate managed. The closing
    // argument is the one session this pass may *move*, and treating it
    // as an association would send a pure CLI close through the API.
    if !associated
        .iter()
        .any(|session| is_opencode_session(session.trim()))
    {
        return SessionVerdict::Unmanaged;
    }
    let known = known_sessions(associated, closing);
    // The read-only report pass must not start an API conversation at all;
    // over-reporting a keep is its documented bias.
    if mode == CleanupMode::Report {
        return SessionVerdict::Keep(format!(
            "an OpenCode session lease points here ({}); occupancy is not probed in report mode",
            known.join(", ")
        ));
    }
    let located = match confirm_associated(api, &known) {
        Ok(located) => located,
        Err(reason) => return SessionVerdict::Keep(reason),
    };
    let occupants = match occupants_of_directory(api, directory) {
        Ok(occupants) => occupants,
        Err(error) => return SessionVerdict::Keep(incomplete(error)),
    };
    // The listing finds occupants this crate never asked about; the direct
    // answers keep the ones the listing forgot. Either source alone is an
    // occupant, so a disagreement costs a directory, never a session.
    let mut hosted: Vec<String> = occupants
        .iter()
        .map(|occupant| occupant.id.clone())
        .collect();
    hosted.extend(hosted_by_direct_answer(&located, directory));
    hosted.sort();
    hosted.dedup();
    if let Some(foreign) = hosted
        .iter()
        .find(|id| !known.iter().any(|known| known == *id))
    {
        return SessionVerdict::Keep(format!(
            "OpenCode session '{foreign}' is hosted here and is not associated with this repository"
        ));
    }
    match hosted.len() {
        0 => SessionVerdict::Vacant,
        // The single occupant is this close's own session and this pass
        // may move sessions: hand it to the move arm.
        1 if mode.may_move_sessions() && closing == Some(hosted[0].as_str()) => {
            return_release(api, target, directory, closing.unwrap_or_default(), &known)
        }
        1 => SessionVerdict::Keep(format!(
            "OpenCode session '{}' is still hosted here",
            hosted[0]
        )),
        _ => SessionVerdict::Keep(format!(
            "{} OpenCode sessions are still hosted here; only the closing session is ever moved",
            hosted.len()
        )),
    }
}

/// The move arm: the single occupant is this close's own session, and the
/// request must be confirmed by fresh evidence before the directory is
/// treated as free.
fn return_release(
    api: &dyn OpenCodeApi,
    target: &TargetResolver<'_>,
    directory: &Path,
    closing: &str,
    known: &[String],
) -> SessionVerdict {
    let destination = match target.get().resolve() {
        Ok(path) => path,
        Err(reason) => {
            return SessionVerdict::Keep(format!(
                "no verified main checkout to move the closing OpenCode session to: {reason}"
            ));
        }
    };
    if let Err(error) = api.move_session(closing, destination) {
        return SessionVerdict::Keep(format!(
            "moving the closing OpenCode session failed: {}",
            crate::lifecycle::bounded(&error.message)
        ));
    }
    // Acceptance is not relocation: a queued or still-running move leaves
    // the session where it was, and the directory must survive that.
    match api.session(closing) {
        Ok(located) if same_directory(Path::new(&located.directory), destination) => {}
        Ok(_) => {
            return SessionVerdict::Keep(format!(
                "the closing OpenCode session has not left '{closing}' yet"
            ));
        }
        Err(error) => {
            return SessionVerdict::Keep(format!(
                "the closing OpenCode session could not be confirmed after the move request: {}",
                crate::lifecycle::bounded(&error.message)
            ));
        }
    }
    // One successful lookup is not a vacated directory: every associated
    // session is read again, because any one of them still hosted here
    // strands a live session when the directory goes.
    let located = match confirm_associated(api, known) {
        Ok(located) => located,
        Err(reason) => return SessionVerdict::Keep(reason),
    };
    if let Some(still_here) = hosted_by_direct_answer(&located, directory).first() {
        return SessionVerdict::Keep(format!(
            "OpenCode session '{still_here}' is still hosted here after the move request"
        ));
    }
    match occupants_of_directory(api, directory) {
        Ok(occupants) if occupants.is_empty() => SessionVerdict::Vacant,
        Ok(occupants) => SessionVerdict::Keep(format!(
            "the closing OpenCode session moved but {} session(s) are still hosted here",
            occupants.len()
        )),
        Err(error) => SessionVerdict::Keep(incomplete(error)),
    }
}

/// Certainty pass: every associated session must answer, and the answer is
/// returned rather than dropped. Anything else leaves the candidate
/// associated with an unknown session.
fn confirm_associated(
    api: &dyn OpenCodeApi,
    known: &[String],
) -> Result<Vec<LocatedSession>, String> {
    let mut located = Vec::with_capacity(known.len());
    for id in known {
        match api.session(id) {
            Ok(session) => located.push(session),
            Err(error) => {
                return Err(format!(
                    "OpenCode session '{id}' could not be located: {}",
                    crate::lifecycle::bounded(&error.message)
                ));
            }
        }
    }
    Ok(located)
}

/// The certainty pass for a pass that may *move* a session.
///
/// A response that names another session is a mismatched answer: it says
/// nothing about the session that was asked about, so it is not evidence for
/// an occupant and it never authorises a move of the id it happens to carry.
/// A response whose location is not absolute is a malformed answer for the
/// same reason: an unusable location is not a location, and comparing it with
/// a candidate would read as "not there" where the truth is "not known".
/// The close chain moves the session it was given by name and is unaffected;
/// this helper is the strict counterpart the removal stage uses.
fn locate_associated(
    api: &dyn OpenCodeApi,
    known: &[String],
) -> Result<Vec<LocatedSession>, String> {
    let mut located = Vec::with_capacity(known.len());
    for id in known {
        match api.session(id) {
            Ok(session) if session.id != *id => {
                return Err(format!(
                    "the host answered for OpenCode session '{id}' with a different session '{}'",
                    crate::lifecycle::bounded(&session.id)
                ));
            }
            Ok(session) if !Path::new(&session.directory).is_absolute() => {
                return Err(format!(
                    "the host answered for OpenCode session '{id}' with a non-absolute location '{}'",
                    crate::lifecycle::bounded(&session.directory)
                ));
            }
            Ok(session) => located.push(session),
            Err(error) => {
                return Err(format!(
                    "OpenCode session '{id}' could not be located: {}",
                    crate::lifecycle::bounded(&error.message)
                ));
            }
        }
    }
    Ok(located)
}

/// The associated sessions the host authoritatively reports inside
/// `directory`, from the direct lookups alone.
fn hosted_by_direct_answer(located: &[LocatedSession], directory: &Path) -> Vec<String> {
    located
        .iter()
        .filter(|session| same_directory(Path::new(&session.directory), directory))
        .map(|session| session.id.clone())
        .collect()
}

fn incomplete(error: ApiError) -> String {
    format!(
        "the OpenCode occupancy listing is incomplete: {}",
        crate::lifecycle::bounded(&error.message)
    )
}

fn known_sessions(associated: &[String], closing: Option<&str>) -> Vec<String> {
    let mut known: Vec<String> = associated
        .iter()
        .map(|session| session.trim().to_owned())
        .filter(|session| is_opencode_session(session))
        .collect();
    if let Some(closing) = closing
        && !known.iter().any(|id| id == closing)
    {
        known.push(closing.to_owned());
    }
    known.sort();
    known.dedup();
    known
}

/// The host's fresh evidence about one candidate directory.
///
/// Two sources are combined and neither may overrule the other: the
/// complete occupancy listing finds occupants this crate never asked
/// about, and the direct answer of every associated session keeps the
/// ones the listing forgot.
pub(crate) struct Occupancy {
    /// Every session the complete listing reports in the directory.
    listed: Vec<String>,
    /// The associated sessions whose own authoritative answer places them
    /// in the directory: the only sessions a pass may return.
    returnable: Vec<String>,
    /// Associated sessions the listing places in the directory while their
    /// own answer places them elsewhere.
    conflicting: Vec<String>,
}

impl Occupancy {
    /// The sessions the complete listing reports in the directory.
    fn listed(&self) -> &[String] {
        &self.listed
    }

    /// The associated sessions this pass may return to the main checkout.
    fn returnable(&self) -> &[String] {
        &self.returnable
    }

    /// The associated sessions whose listing and own answer disagree.
    fn conflicting(&self) -> &[String] {
        &self.conflicting
    }
}

/// Read the fresh occupancy of `directory`.
///
/// The listing is complete or the evidence is unusable, and every
/// associated session must answer for itself: a session the selected
/// instance does not own may live in a standalone or private instance, so
/// "not found" is uncertainty, never absence, and an answer that names
/// another session or reports an unusable location is not an answer about
/// the session that was asked for.
pub(crate) fn discover_occupancy(
    api: &dyn OpenCodeApi,
    directory: &Path,
    associated: &[String],
) -> Result<Occupancy, String> {
    let occupants = occupants_of_directory(api, directory).map_err(incomplete)?;
    let known = known_sessions(associated, None);
    // Every answer is checked against the session that was asked about, so
    // the ids derived below are the requested ones and never an id the host
    // happened to mention.
    let located = locate_associated(api, &known)?;
    let mut listed: Vec<String> = occupants
        .iter()
        .map(|occupant| occupant.id.clone())
        .collect();
    listed.sort();
    listed.dedup();
    let returnable = hosted_by_direct_answer(&located, directory);
    // A session the listing places here and its own answer places
    // elsewhere is a disagreement, and a disagreement costs a directory,
    // never a session.
    let conflicting: Vec<String> = listed
        .iter()
        .filter(|id| known.contains(id) && !returnable.contains(id))
        .cloned()
        .collect();
    Ok(Occupancy {
        listed,
        returnable,
        conflicting,
    })
}

/// Return the associated idle sessions hosted in `directory` to the
/// verified main checkout, so the directory is not removed while one of our
/// own sessions still sits in it.
///
/// Only a lease-associated session is ever moved, and only out of the
/// directory that still hosts it. A foreign occupant, an associated
/// session the host places elsewhere, an answer that names another session
/// or reports an unusable location, an unreadable host, an incomplete
/// listing, a rejected or queued move, and a move that has not taken
/// effect yet all keep the directory with a bounded reason.
pub(crate) fn return_associated_sessions(
    api: &dyn OpenCodeApi,
    target: &TargetResolver<'_>,
    directory: &Path,
    associated: &[String],
    occupancy: &Occupancy,
) -> SessionVerdict {
    let known = known_sessions(associated, None);
    // An occupant no lease of this repository associates is somebody
    // else's session: it is never moved, and its directory stays.
    if let Some(foreign) = occupancy.listed().iter().find(|id| !known.contains(id)) {
        return SessionVerdict::Keep(format!(
            "OpenCode session '{foreign}' is hosted here and is not associated with this repository"
        ));
    }
    if let Some(conflicting) = occupancy.conflicting().first() {
        return SessionVerdict::Keep(format!(
            "OpenCode session '{conflicting}' is listed here but the host places it elsewhere"
        ));
    }
    if occupancy.returnable().is_empty() {
        // Nothing of ours is hosted here: the directory is free and no
        // session has to be moved.
        return SessionVerdict::Vacant;
    }
    let destination = match target.get().resolve() {
        Ok(path) => path,
        Err(reason) => {
            return SessionVerdict::Keep(format!(
                "no verified main checkout to move the associated OpenCode session to: {reason}"
            ));
        }
    };
    for id in occupancy.returnable() {
        if let Err(error) = api.move_session(id, destination) {
            return SessionVerdict::Keep(format!(
                "moving the associated OpenCode session failed: {}",
                crate::lifecycle::bounded(&error.message)
            ));
        }
        // Acceptance is not relocation: a queued or still-running move
        // leaves the session where it was, and the directory must survive
        // that. The confirmation must also answer for the session that was
        // moved and report a usable location: an answer naming another
        // session, or one whose location cannot be read, proves nothing
        // about this move.
        match api.session(id) {
            Ok(located) if located.id != *id => {
                return SessionVerdict::Keep(format!(
                    "the host answered for the moved OpenCode session '{id}' with a different session '{}'",
                    crate::lifecycle::bounded(&located.id)
                ));
            }
            Ok(located) if !Path::new(&located.directory).is_absolute() => {
                return SessionVerdict::Keep(format!(
                    "the host answered for the moved OpenCode session '{id}' with a non-absolute location '{}'",
                    crate::lifecycle::bounded(&located.directory)
                ));
            }
            Ok(located) if same_directory(Path::new(&located.directory), destination) => {}
            Ok(_) => {
                return SessionVerdict::Keep(format!(
                    "the associated OpenCode session has not left '{id}' yet"
                ));
            }
            Err(error) => {
                return SessionVerdict::Keep(format!(
                    "the associated OpenCode session could not be confirmed after the move request: {}",
                    crate::lifecycle::bounded(&error.message)
                ));
            }
        }
    }
    // One successful lookup is not a vacated directory: every associated
    // session is read again, and a fresh complete listing must agree that
    // nothing is hosted here any more.
    let located = match locate_associated(api, &known) {
        Ok(located) => located,
        Err(reason) => return SessionVerdict::Keep(reason),
    };
    if let Some(still_here) = hosted_by_direct_answer(&located, directory).first() {
        return SessionVerdict::Keep(format!(
            "OpenCode session '{still_here}' is still hosted here after the move request"
        ));
    }
    match occupants_of_directory(api, directory) {
        Ok(occupants) if occupants.is_empty() => SessionVerdict::Vacant,
        Ok(occupants) => SessionVerdict::Keep(format!(
            "{} session(s) are still hosted here",
            occupants.len()
        )),
        Err(error) => SessionVerdict::Keep(incomplete(error)),
    }
}
