//! The shared family assessment: which sessions belong to one candidate
//! directory's lease family, and which of them may be returned to the
//! repository's verified main checkout.
//!
//! A lease row says where a session *was* when the worktree was acquired; the
//! host says where a session *is* and which session it descends from. The
//! cleanup therefore never asks "is this session free?" but "does this
//! session belong to the family this directory's leases describe?".
//!
//! * The **roots** of a family are the distinct `ses_` sessions the
//!   persisted leases associate with the candidate directory. Nothing else is
//!   ever promoted into a root: an occupant id is evidence about an
//!   occupant, never about the family.
//! * A **related** session is a root, or a session whose authoritative
//!   `parentID` chain reaches a root. The parent may live anywhere — only
//!   the lineage edge matters — and the chain is read from the host, bounded
//!   to [`MAX_PARENT_EDGES`] edges, checked for cycles, and every answer
//!   must name the session that was asked for and report a usable absolute
//!   location. An unknown, deleted, mismatched, cyclic, or foreign chain is
//!   never accepted: it keeps the directory.
//! * An occupant that is not related is somebody else's session. It is never
//!   moved, and it keeps the directory.
//! * A related occupant that is *running* keeps the directory, with exactly
//!   one exception: the close chain must be able to finish its own session,
//!   so the closing session is exempt in [`CleanupMode::Close`] and nowhere
//!   else.
//!
//! The same assessment powers `issue close`, `worktree prune --remove`, and
//! the `worktree prune` preview, so a preview reports exactly the blockers
//! the removal stage would meet and never promises a move it cannot prove.

use std::collections::BTreeSet;
use std::path::Path;

use super::CleanupMode;
use super::target::TargetResolver;
use super::wait::MovePolicy;
use crate::worktree::opencode::{
    ApiError, LocatedSession, OpenCodeApi, is_opencode_session, occupants_of_directory,
};
use crate::worktree::same_directory;

/// The maximum number of parent edges one lineage may have. A longer chain
/// is uncertainty, not a family: the traversal stops and keeps the
/// directory.
pub(crate) const MAX_PARENT_EDGES: usize = 64;

/// The outcome of the shared family assessment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FamilyOutcome {
    /// No persisted lease associates an OpenCode session with the directory:
    /// a pure CLI candidate, decided exactly as before.
    Unmanaged,
    /// No session of the family is hosted in the directory.
    Vacant,
    /// The family's idle sessions were returned to the verified main
    /// checkout and the directory may be removed.
    Released { sessions: Vec<String> },
    /// A preview: the family *would* be returned; nothing was moved.
    Releasable { sessions: Vec<String> },
    /// The directory must be kept, with the bounded reason that decided it.
    Keep(String),
}

impl FamilyOutcome {
    /// The keep reason, or `None` when removal may proceed.
    pub(crate) fn keep_reason(&self) -> Option<String> {
        match self {
            Self::Unmanaged | Self::Vacant | Self::Released { .. } | Self::Releasable { .. } => {
                None
            }
            Self::Keep(reason) => Some(reason.clone()),
        }
    }
}

/// Assess one candidate directory and, when the mode allows it, return the
/// family's idle sessions to the verified main checkout.
///
/// `associated` are the `ses_` sessions the persisted leases point at
/// `directory`; they are the family's roots. `closing` is the close chain's
/// own session and is a root in [`CleanupMode::Close`] only — it never makes
/// a pure CLI candidate managed.
pub(crate) fn finalize_family(
    api: &dyn OpenCodeApi,
    mode: CleanupMode,
    target: &TargetResolver<'_>,
    directory: &Path,
    associated: &[String],
    closing: Option<&str>,
) -> FamilyOutcome {
    finalize_family_with(
        api,
        mode,
        target,
        directory,
        associated,
        closing,
        MovePolicy::NoWait,
    )
}

/// [`finalize_family`] with an explicit move-confirmation policy. Only an
/// actual prune passes [`MovePolicy::Bounded`]; every other caller keeps the
/// historical immediate read.
#[allow(clippy::too_many_arguments)]
pub(crate) fn finalize_family_with(
    api: &dyn OpenCodeApi,
    mode: CleanupMode,
    target: &TargetResolver<'_>,
    directory: &Path,
    associated: &[String],
    closing: Option<&str>,
    confirmation: MovePolicy<'_>,
) -> FamilyOutcome {
    let roots = root_sessions(associated);
    let close_root = close_root(mode, closing);
    // The read-only report pass must not start an API conversation at all;
    // over-reporting a keep is its documented bias.
    if mode == CleanupMode::Report {
        return match roots.is_empty() {
            true => FamilyOutcome::Unmanaged,
            false => FamilyOutcome::Keep(format!(
                "an OpenCode session lease points here ({}); occupancy is not probed in report mode",
                roots.join(", ")
            )),
        };
    }
    // A pure CLI candidate is decided by the local guards alone, so it never
    // reaches the host. The prune pass and its preview still enumerate the
    // directory: an occupant no lease associates is somebody else's session,
    // and it keeps the directory.
    if roots.is_empty() && !mode.enumerates_unmanaged_candidates() {
        return FamilyOutcome::Unmanaged;
    }
    let mut family = roots;
    if let Some(close_root) = &close_root
        && !family.contains(close_root)
    {
        family.push(close_root.clone());
    }
    // Every root must answer for itself: a session the selected instance does
    // not own may live in a standalone or private instance, so "not found" is
    // uncertainty, never absence.
    let located_roots = match locate_roots(api, &family) {
        Ok(located) => located,
        Err(reason) => return FamilyOutcome::Keep(reason),
    };
    let occupants = match occupants_of_directory(api, directory) {
        Ok(occupants) => occupants,
        Err(error) => return FamilyOutcome::Keep(incomplete(&error)),
    };
    // The listing finds occupants this crate never asked about; the roots'
    // own answers keep the ones the listing forgot. Either source alone is
    // an occupant, so a disagreement costs a directory, never a session.
    // The listing also carries each occupant's lineage edge, so the ancestry
    // of an occupant never costs an extra lookup of its own.
    let mut hosted: Vec<(String, Option<String>)> = occupants
        .iter()
        .map(|occupant| (occupant.id.clone(), occupant.parent_id.clone()))
        .collect();
    for root in &located_roots {
        if same_directory(Path::new(&root.directory), directory) {
            hosted.push((root.id.clone(), None));
        }
    }
    hosted.sort();
    hosted.dedup_by(|left, right| left.0 == right.0);
    // A root the listing places here while its own answer places it
    // elsewhere is a disagreement, and a disagreement costs a directory.
    for root in &located_roots {
        let listed_here = occupants.iter().any(|occupant| occupant.id == root.id);
        if listed_here && !same_directory(Path::new(&root.directory), directory) {
            return FamilyOutcome::Keep(format!(
                "OpenCode session '{}' is listed here but the host places it elsewhere",
                root.id
            ));
        }
    }
    // The running evidence is the authority on "idle". A host that cannot
    // answer keeps the directory rather than reading as an empty answer.
    // The check is shared with the per-move re-verification so both callers
    // apply exactly one running policy.
    if mode.reads_running_state()
        && let Err(reason) = running_blocker(api, directory, close_root.as_deref(), None, &[])
    {
        return FamilyOutcome::Keep(reason);
    }
    if hosted.is_empty() {
        return FamilyOutcome::Vacant;
    }
    // Every occupant must be proven to belong to the family before anything
    // may be moved.
    let mut movable: Vec<String> = Vec::with_capacity(hosted.len());
    for (id, parent) in &hosted {
        let related = if family.iter().any(|root| root == id) {
            true
        } else {
            match lineage(api, id, parent.as_deref(), &family) {
                Ok(related) => related,
                Err(reason) => return FamilyOutcome::Keep(reason),
            }
        };
        if !related {
            return FamilyOutcome::Keep(format!(
                "OpenCode session '{id}' is hosted here and is not associated with this repository"
            ));
        }
        movable.push(id.clone());
    }
    // A revalidation pass reads current occupancy and running state, keeps
    // whenever any session is still hosted, and never moves anything.
    if mode == CleanupMode::Verify {
        let ids: Vec<String> = hosted.iter().map(|(id, _)| id.clone()).collect();
        return FamilyOutcome::Keep(hosted_reason(&ids));
    }
    // The reconciliation pass never moves a session: a directory that still
    // hosts one of ours waits for a later pass.
    if mode == CleanupMode::Sync {
        let ids: Vec<String> = hosted.iter().map(|(id, _)| id.clone()).collect();
        return FamilyOutcome::Keep(hosted_reason(&ids));
    }
    if !mode.may_move_sessions() {
        // The preview proposes moving the family, so it must prove the same
        // target the removal would move to: an unverifiable main checkout is
        // the blocker the removal would meet, reported with the same keep
        // reason and zero mutations.
        if !movable.is_empty() {
            return match target.get().resolve() {
                Ok(_) => FamilyOutcome::Releasable { sessions: movable },
                Err(reason) => FamilyOutcome::Keep(format!(
                    "no verified main checkout to move the associated OpenCode session to: {reason}"
                )),
            };
        }
        return FamilyOutcome::Releasable { sessions: movable };
    }
    release_family(
        api,
        target,
        directory,
        movable,
        &family,
        close_root.as_deref(),
        confirmation,
    )
}

/// Return the family's idle sessions to the verified main checkout and prove
/// that the directory is free afterwards.
///
/// Each move is re-verified immediately before it is requested: the exact
/// session must still be a member of this directory's lease family, must
/// still sit in the candidate (or already at the main checkout, in which
/// case it needs no move), and must not be running — checked together with
/// every other candidate occupant and with the sessions already returned by
/// this pass. The host operations are not globally atomic; the check only
/// catches changes observed before the next mutation.
fn release_family(
    api: &dyn OpenCodeApi,
    target: &TargetResolver<'_>,
    directory: &Path,
    movable: Vec<String>,
    family: &[String],
    close_root: Option<&str>,
    confirmation: MovePolicy<'_>,
) -> FamilyOutcome {
    let destination = match target.get().resolve() {
        Ok(path) => path.to_path_buf(),
        Err(reason) => {
            return FamilyOutcome::Keep(format!(
                "no verified main checkout to move the associated OpenCode session to: {reason}"
            ));
        }
    };
    let mut returned: Vec<String> = Vec::with_capacity(movable.len());
    for id in &movable {
        // The exact session, as the host answers for it now.
        let located = match api.session(id) {
            Ok(located) if located.id != *id => {
                return FamilyOutcome::Keep(format!(
                    "the host answered for OpenCode session '{id}' with a different session '{}'",
                    crate::lifecycle::bounded(&located.id)
                ));
            }
            Ok(located) if !Path::new(&located.directory).is_absolute() => {
                return FamilyOutcome::Keep(format!(
                    "the host answered for OpenCode session '{id}' with a non-absolute location '{}'",
                    crate::lifecycle::bounded(&located.directory)
                ));
            }
            Ok(located) => located,
            Err(error) => {
                return FamilyOutcome::Keep(format!(
                    "OpenCode session '{id}' could not be located before its move: {}",
                    crate::lifecycle::bounded(&error.message)
                ));
            }
        };
        // The family proof must still hold for this exact session. A root is
        // proven by identity; a descendant is re-proven through its current
        // lineage edge.
        if !family.iter().any(|root| root == id) {
            match lineage(api, id, located.parent_id.as_deref(), family) {
                Ok(true) => {}
                Ok(false) => {
                    return FamilyOutcome::Keep(format!(
                        "OpenCode session '{id}' is no longer part of this directory's lease family"
                    ));
                }
                Err(reason) => return FamilyOutcome::Keep(reason),
            }
        }
        // The session must still be where it was to be returned from. One
        // that already reached the main checkout needs no move; one that
        // moved anywhere else is not reclaimed from unrelated work.
        if same_directory(Path::new(&located.directory), &destination) {
            continue;
        }
        if !same_directory(Path::new(&located.directory), directory) {
            return FamilyOutcome::Keep(format!(
                "OpenCode session '{id}' is now in '{}', not the directory it was to be returned from",
                crate::lifecycle::bounded(&located.directory)
            ));
        }
        // Fresh running evidence for every candidate occupant, the intended
        // session, and anything this pass already returned: a session that
        // started running after an earlier move blocks the next one.
        if let Err(reason) = running_blocker(api, directory, close_root, Some(id), &returned) {
            return FamilyOutcome::Keep(reason);
        }
        if let Err(error) = api.move_session(id, &destination) {
            return FamilyOutcome::Keep(format!(
                "moving the OpenCode session '{id}' failed: {}",
                crate::lifecycle::bounded(&error.message)
            ));
        }
        // Acceptance is not relocation: a queued or still-running move leaves
        // the session where it was, and the directory must survive that. The
        // first read is immediate; only the bounded prune policy may then
        // poll the exact session, and it never reissues the move.
        if let Err(reason) = confirmation.confirm(api, id, directory, &destination, || {
            running_blocker(api, directory, close_root, Some(id), &returned)
        }) {
            return FamilyOutcome::Keep(reason);
        }
        returned.push(id.clone());
    }
    // One successful lookup is not a vacated directory: a fresh complete
    // listing and the family's own answers must agree that nothing of ours
    // is hosted here any more.
    match occupants_of_directory(api, directory) {
        Ok(occupants) if !occupants.is_empty() => {
            return FamilyOutcome::Keep(format!(
                "{} session(s) are still hosted here",
                occupants.len()
            ));
        }
        Ok(_) => {}
        Err(error) => return FamilyOutcome::Keep(incomplete(&error)),
    }
    match locate_roots(api, family) {
        Ok(located) => {
            if let Some(root) = located
                .iter()
                .find(|root| same_directory(Path::new(&root.directory), directory))
            {
                return FamilyOutcome::Keep(format!(
                    "OpenCode session '{}' is still hosted here after the move request",
                    root.id
                ));
            }
        }
        Err(reason) => return FamilyOutcome::Keep(reason),
    }
    // The directory must still be free of running occupancy, and a session
    // this pass returned must not have started running either.
    if let Err(reason) = running_blocker(api, directory, close_root, None, &returned) {
        return FamilyOutcome::Keep(reason);
    }
    FamilyOutcome::Released { sessions: movable }
}

/// The single running policy every phase of the shared assessment uses.
///
/// `close_root` is the exact closing session and is `Some` only in
/// [`CleanupMode::Close`]; every other mode passes `None`, so the exemption
/// cannot leak into prune or the preview. `intended` is the session about to
/// be moved, and `already_returned` are the sessions this pass relocated
/// earlier. Any host answer that cannot be read fails closed here, before the
/// next mutation.
fn running_blocker(
    api: &dyn OpenCodeApi,
    directory: &Path,
    close_root: Option<&str>,
    intended: Option<&str>,
    already_returned: &[String],
) -> Result<(), String> {
    let running = running_sessions(api)?;
    for (id, reported) in &running {
        if close_root == Some(id.as_str()) {
            continue;
        }
        if intended == Some(id.as_str()) {
            return Err(format!(
                "running OpenCode session '{id}' is the session about to be moved"
            ));
        }
        if already_returned.iter().any(|returned| returned == id) {
            return Err(format!(
                "running OpenCode session '{id}' started running after it was returned"
            ));
        }
        if same_directory(Path::new(reported), directory) {
            return Err(format!(
                "running OpenCode session '{id}' currently occupies this directory"
            ));
        }
    }
    Ok(())
}

/// The distinct `ses_` sessions the persisted leases associate with one
/// directory. They are the family's roots and nothing else is.
pub(crate) fn root_sessions(associated: &[String]) -> Vec<String> {
    let mut roots: Vec<String> = associated
        .iter()
        .map(|session| session.trim().to_owned())
        .filter(|session| is_opencode_session(session))
        .collect();
    roots.sort();
    roots.dedup();
    roots
}

/// The close chain's own session, which is a root of the family in
/// [`CleanupMode::Close`] only.
fn close_root(mode: CleanupMode, closing: Option<&str>) -> Option<String> {
    if mode != CleanupMode::Close {
        return None;
    }
    closing
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .filter(|id| is_opencode_session(id))
        .map(str::to_owned)
}

/// Prove that `id` descends from one of the family's roots.
///
/// `start` is the lineage edge the occupancy listing reported for the
/// occupant. The chain is then read from the host, one authoritative answer
/// per edge. A chain that ends at a session outside the family is a foreign
/// chain, and an unknown, mismatched, cyclic, or over-long chain is
/// uncertainty: both keep the directory.
fn lineage(
    api: &dyn OpenCodeApi,
    id: &str,
    start: Option<&str>,
    family: &[String],
) -> Result<bool, String> {
    let mut parent = match start {
        None => return Ok(false),
        Some(parent) => parent.to_owned(),
    };
    let mut visited: BTreeSet<String> = BTreeSet::new();
    visited.insert(id.to_owned());
    for _ in 0..MAX_PARENT_EDGES {
        if !is_opencode_session(&parent) {
            return Err(format!(
                "the parent lineage of OpenCode session '{id}' names an unusable parent '{parent}'"
            ));
        }
        if family.iter().any(|root| root == &parent) {
            return Ok(true);
        }
        if !visited.insert(parent.clone()) {
            return Err(format!(
                "the parent lineage of OpenCode session '{id}' is cyclic"
            ));
        }
        let located = match api.session(&parent) {
            Ok(located) => located,
            Err(error) => {
                return Err(format!(
                    "the parent '{parent}' of OpenCode session '{id}' could not be located: {}",
                    crate::lifecycle::bounded(&error.message)
                ));
            }
        };
        if located.id != parent {
            return Err(format!(
                "the host answered for parent '{parent}' of OpenCode session '{id}' with a different session '{}'",
                crate::lifecycle::bounded(&located.id)
            ));
        }
        if !Path::new(&located.directory).is_absolute() {
            return Err(format!(
                "the host answered for parent '{parent}' of OpenCode session '{id}' with a non-absolute location '{}'",
                crate::lifecycle::bounded(&located.directory)
            ));
        }
        parent = match located.parent_id {
            None => return Ok(false),
            Some(next) => next,
        };
    }
    Err(format!(
        "the parent lineage of OpenCode session '{id}' exceeds the {MAX_PARENT_EDGES}-edge limit"
    ))
}

/// The authoritative answers of the family's roots. Every answer must name
/// the session that was asked for and report a usable absolute location.
fn locate_roots(api: &dyn OpenCodeApi, roots: &[String]) -> Result<Vec<LocatedSession>, String> {
    let mut located = Vec::with_capacity(roots.len());
    for id in roots {
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

/// The connected service's currently running sessions and their reported
/// locations. Only a complete, self-consistent answer is usable.
fn running_sessions(api: &dyn OpenCodeApi) -> Result<Vec<(String, String)>, String> {
    let ids = api.active_sessions().map_err(|error| {
        format!(
            "the running-session query failed: {}",
            crate::lifecycle::bounded(&error.message)
        )
    })?;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut locations: Vec<(String, String)> = Vec::with_capacity(ids.len());
    for id in ids {
        if !seen.insert(id.clone()) {
            continue;
        }
        let session = api.session(&id).map_err(|error| {
            format!(
                "running session '{id}' could not be located: {}",
                crate::lifecycle::bounded(&error.message)
            )
        })?;
        if session.id != id {
            return Err(format!(
                "the host answered for running session '{id}' with a different session '{}'",
                crate::lifecycle::bounded(&session.id)
            ));
        }
        if !Path::new(&session.directory).is_absolute() {
            return Err(format!(
                "running session '{id}' reported a non-absolute directory"
            ));
        }
        locations.push((id, session.directory));
    }
    Ok(locations)
}

/// The reason a directory that still hosts one of our sessions is kept.
fn hosted_reason(hosted: &[String]) -> String {
    match hosted {
        [single] => format!("OpenCode session '{single}' is still hosted here"),
        many => format!("{} OpenCode sessions are still hosted here", many.len()),
    }
}

fn incomplete(error: &ApiError) -> String {
    format!(
        "the OpenCode occupancy listing is incomplete: {}",
        crate::lifecycle::bounded(&error.message)
    )
}
