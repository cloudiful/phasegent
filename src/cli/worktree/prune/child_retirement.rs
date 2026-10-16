//! Limited retirement of proven-finished Task children (issue 918 P2).
//!
//! A completed Task child can keep a leased worktree occupied after its parent
//! has already moved on, which previously blocked `worktree prune` as a foreign
//! occupant. The user explicitly authorized prune to relocate such a child to
//! the repository's verified main checkout so the directory can be removed.
//!
//! The permission is deliberately narrow. Only a session that the host reports
//! with an `outcome` of exactly `succeeded`, whose `parentID` chain reaches a
//! persisted `ses_` lease session for this canonical directory, and which is not
//! itself a lease owner may be moved. Every other occupant — running, unknown,
//! non-succeeded, foreign, unreadable, cyclic, or deeper than
//! [`MAX_PARENT_HOPS`] — keeps the whole directory with zero moves. Association
//! is never inferred from a shared path or from idle age, and the move target is
//! only ever the verified main checkout, never a path guessed from the current
//! directory or a parent chain.
//!
//! Planning is read-only and may read through the scan cache; execution
//! ([`retire`]) re-proves each child immediately before moving it and confirms
//! the destination with a fresh read. A move the host merely accepts is never
//! treated as a relocation.

use std::collections::{BTreeSet, HashSet};
use std::path::Path;

use crate::lifecycle::cleanup::TargetResolver;
use crate::worktree::opencode::{ApiError, OpenCodeApi, occupants_of_directory};
use crate::worktree::{LeaseRow, same_directory};

/// Maximum `parentID` hops an eligible child may be from a lease owner. A
/// deeper chain is uncertainty and keeps the directory.
pub(crate) const MAX_PARENT_HOPS: usize = 8;

/// The exact terminal outcome prune may retire a child from.
const SUCCEEDED: &str = "succeeded";

/// Read-only retirement decision for one canonical directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RetirementPlan {
    /// Nothing to retire: no `ses_` lease owner, or every owner is already
    /// elsewhere and no child occupies the directory.
    Vacant,
    /// Every occupant is an individually proven finished descendant; move these
    /// (sorted, deduplicated) child sessions.
    Retire(Vec<String>),
    /// The directory must be kept; the bounded reason naming why.
    Keep(String),
}

impl RetirementPlan {
    /// The planned children, empty for [`RetirementPlan::Vacant`] and
    /// [`RetirementPlan::Keep`].
    pub(crate) fn children(&self) -> &[String] {
        match self {
            Self::Retire(children) => children,
            Self::Vacant | Self::Keep(_) => &[],
        }
    }
}

/// The persisted `ses_` lease sessions that own `directory`, sorted and
/// deduplicated. A directory with no such owner is not managed by the host and
/// this module has nothing to decide.
pub(crate) fn lease_owners(rows: &[LeaseRow], directory: &Path) -> BTreeSet<String> {
    rows.iter()
        .filter(|row| same_directory(Path::new(&row.worktree_path), directory))
        .map(|row| row.session.trim().to_owned())
        .filter(|session| crate::worktree::opencode::is_opencode_session(session))
        .collect()
}

/// Build the read-only retirement plan for one canonical directory. Never
/// writes and never moves a session.
pub(crate) fn plan(
    api: &dyn OpenCodeApi,
    target: &TargetResolver<'_>,
    directory: &Path,
    rows: &[LeaseRow],
) -> RetirementPlan {
    let owners = lease_owners(rows, directory);
    if owners.is_empty() {
        return RetirementPlan::Vacant;
    }
    // The complete, unfiltered listing plus each owner's authoritative direct
    // answer decide who is really here. An incomplete listing is uncertainty.
    let occupants = match occupants_of_directory(api, directory) {
        Ok(occupants) => occupants,
        Err(error) => return RetirementPlan::Keep(incomplete_listing(&error)),
    };
    let mut occupant_ids: BTreeSet<String> =
        occupants.into_iter().map(|session| session.id).collect();
    for owner in &owners {
        match api.session(owner) {
            Ok(session) if same_directory(Path::new(&session.directory), directory) => {
                occupant_ids.insert(owner.clone());
            }
            Ok(_) => {}
            Err(error) => return RetirementPlan::Keep(unlocatable_owner(owner, &error)),
        }
    }
    if occupant_ids.is_empty() {
        return RetirementPlan::Vacant;
    }
    let mut children: Vec<String> = Vec::new();
    for id in &occupant_ids {
        if owners.contains(id) {
            return RetirementPlan::Keep(format!(
                "OpenCode session '{id}' is a lease owner still hosted here"
            ));
        }
        match child_eligibility(api, id, &owners, directory) {
            ChildEligibility::Eligible => children.push(id.clone()),
            ChildEligibility::AlreadyElsewhere => {
                return RetirementPlan::Keep(format!(
                    "OpenCode session '{id}' is no longer hosted here; the occupancy answer is unsettled"
                ));
            }
            ChildEligibility::Reject(reason) => return RetirementPlan::Keep(reason),
        }
    }
    match target.get().resolve() {
        Ok(main) if !same_directory(main, directory) => RetirementPlan::Retire(children),
        Ok(_) => RetirementPlan::Keep("the directory is the repository's main checkout".to_owned()),
        Err(reason) => RetirementPlan::Keep(format!(
            "no verified main checkout to move finished child sessions to: {reason}"
        )),
    }
}

/// The outcome of one retirement attempt.
pub(crate) struct RetirementOutcome {
    /// Child sessions whose relocation to the target was confirmed by a fresh
    /// read.
    pub(crate) moved: Vec<String>,
    /// The bounded reason the attempt stopped early, if it did. Already moved
    /// children stay at the safe target; nothing is rolled back.
    pub(crate) failure: Option<String>,
}

/// Execute a previously planned retirement. Re-proves each child immediately
/// before the move, confirms the destination by a fresh read, and leaves an
/// already-relocated child at the target on any later failure.
pub(crate) fn retire(
    api: &dyn OpenCodeApi,
    target: &TargetResolver<'_>,
    directory: &Path,
    owners: &BTreeSet<String>,
    children: &[String],
) -> RetirementOutcome {
    let destination = match target.get().resolve() {
        Ok(main) => main.to_path_buf(),
        Err(reason) => {
            return RetirementOutcome {
                moved: Vec::new(),
                failure: Some(format!(
                    "no verified main checkout to move finished child sessions to: {reason}"
                )),
            };
        }
    };
    let mut moved: Vec<String> = Vec::new();
    for child in children {
        match child_eligibility(api, child, owners, directory) {
            ChildEligibility::Eligible => {}
            // The child already left between planning and execution: nothing to
            // move, and no failure.
            ChildEligibility::AlreadyElsewhere => continue,
            ChildEligibility::Reject(reason) => {
                return RetirementOutcome {
                    moved,
                    failure: Some(reason),
                };
            }
        }
        if let Err(error) = api.move_session(child, &destination) {
            return RetirementOutcome {
                moved,
                failure: Some(format!(
                    "moving finished child session '{child}' failed: {}",
                    crate::worktree::bounded(&error.message)
                )),
            };
        }
        // Acceptance is not relocation: a queued or still-running move leaves
        // the child where it was, and the directory must survive that.
        match api.session(child) {
            Ok(located) if same_directory(Path::new(&located.directory), &destination) => {
                moved.push(child.clone());
            }
            Ok(_) => {
                return RetirementOutcome {
                    moved,
                    failure: Some(format!(
                        "finished child session '{child}' has not moved to the main checkout yet"
                    )),
                };
            }
            Err(error) => {
                return RetirementOutcome {
                    moved,
                    failure: Some(format!(
                        "finished child session '{child}' could not be confirmed after the move request: {}",
                        crate::worktree::bounded(&error.message)
                    )),
                };
            }
        }
    }
    RetirementOutcome {
        moved,
        failure: None,
    }
}

/// Whether one occupant may be retired, from a fresh host read.
enum ChildEligibility {
    Eligible,
    /// The occupant is no longer reported inside the directory.
    AlreadyElsewhere,
    /// Kept, with the bounded reason that decided it.
    Reject(String),
}

fn child_eligibility(
    api: &dyn OpenCodeApi,
    id: &str,
    owners: &BTreeSet<String>,
    directory: &Path,
) -> ChildEligibility {
    let session = match api.session(id) {
        Ok(session) => session,
        Err(error) => {
            return ChildEligibility::Reject(format!(
                "OpenCode session '{id}' could not be located: {}",
                crate::worktree::bounded(&error.message)
            ));
        }
    };
    if !same_directory(Path::new(&session.directory), directory) {
        return ChildEligibility::AlreadyElsewhere;
    }
    if session.outcome.as_deref() != Some(SUCCEEDED) {
        return ChildEligibility::Reject(not_finished(id, session.outcome.as_deref()));
    }
    let mut current = match session.parent_id.as_deref() {
        Some(parent) => parent.to_owned(),
        None => return ChildEligibility::Reject(not_finished(id, Some("no parent"))),
    };
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(id.to_owned());
    for _ in 0..MAX_PARENT_HOPS {
        if owners.contains(&current) {
            return ChildEligibility::Eligible;
        }
        if !visited.insert(current.clone()) {
            return ChildEligibility::Reject(format!(
                "OpenCode session '{id}' has a cyclic parent chain; kept safely"
            ));
        }
        let parent = match api.session(&current) {
            Ok(parent) => parent,
            Err(error) => {
                return ChildEligibility::Reject(format!(
                    "OpenCode session '{current}' on the parent chain of '{id}' could not be located: {}",
                    crate::worktree::bounded(&error.message)
                ));
            }
        };
        current = match parent.parent_id.as_deref() {
            Some(next) => next.to_owned(),
            None => {
                return ChildEligibility::Reject(format!(
                    "OpenCode session '{id}' does not descend from a lease owner of this directory"
                ));
            }
        };
    }
    ChildEligibility::Reject(format!(
        "OpenCode session '{id}' has a parent chain deeper than {MAX_PARENT_HOPS} hops; kept safely"
    ))
}

fn not_finished(id: &str, outcome: Option<&str>) -> String {
    match outcome {
        Some(outcome) => format!(
            "OpenCode session '{id}' has outcome '{outcome}', which is not a proven finished state"
        ),
        None => format!("OpenCode session '{id}' has no proven finished state"),
    }
}

fn incomplete_listing(error: &ApiError) -> String {
    format!(
        "the OpenCode occupancy listing is incomplete: {}",
        crate::worktree::bounded(&error.message)
    )
}

fn unlocatable_owner(owner: &str, error: &ApiError) -> String {
    format!(
        "OpenCode session '{owner}' could not be located: {}",
        crate::worktree::bounded(&error.message)
    )
}
