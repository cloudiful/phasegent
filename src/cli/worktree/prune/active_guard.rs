//! Current-service running occupancy for `worktree prune` (issue 928 P1).
//!
//! Prune blocks only on sessions the connected shared OpenCode service reports
//! as *currently running*. A lease row and a historical session record say where
//! a session *was*; neither is running-state proof, and a stale historical
//! reference to a closed, clean directory must not keep that directory alive.
//! The service's `session.active` answer is the runtime authority: an id absent
//! from it is inactive, and an inactive root or child record that still points at
//! the directory never blocks removal.
//!
//! Coverage is deliberately single-service: this is the foreground-drain view of
//! the one connected service, not a whole-machine multi-service inventory. Other
//! processes or background work the service does not represent are outside this
//! contract.
//!
//! [`RunningSnapshot::capture`] reads the active ids and each id's location in
//! full. Only a complete, self-consistent answer is usable: a failed or
//! malformed active query, an id the service will not describe, an answer whose
//! id does not match the one asked about, or a non-absolute directory all make
//! the whole answer invalid. The caller then keeps the directory, because an
//! unreadable host is not an empty active set.

use std::collections::BTreeSet;
use std::path::Path;

use crate::worktree::opencode::OpenCodeApi;
use crate::worktree::{bounded, same_directory};

/// A complete, validated view of the connected service's running sessions,
/// reduced to `(id, reported directory)` pairs.
pub(crate) struct RunningSnapshot {
    locations: Vec<(String, String)>,
}

impl RunningSnapshot {
    /// Read the running-session occupancy from `api`. Every failure is a bounded
    /// reason the caller keeps the directory with; a caller must never read an
    /// `Err` as vacancy.
    pub(crate) fn capture(api: &dyn OpenCodeApi) -> Result<Self, String> {
        let ids = api.active_sessions().map_err(|error| {
            format!(
                "the running-session query failed: {}",
                bounded(&error.message)
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
                    bounded(&error.message)
                )
            })?;
            if session.id != id {
                return Err(format!(
                    "the host answered for running session '{id}' with a different session '{}'",
                    bounded(&session.id)
                ));
            }
            let directory = Path::new(&session.directory);
            if !directory.is_absolute() {
                return Err(format!(
                    "running session '{id}' reported a non-absolute directory"
                ));
            }
            locations.push((id, session.directory));
        }
        Ok(Self { locations })
    }

    /// The bounded keep reason when a running session currently sits in
    /// `directory`, or `None` when no running session does.
    pub(crate) fn keep_reason(&self, directory: &Path) -> Option<String> {
        self.locations
            .iter()
            .find(|(_, reported)| same_directory(Path::new(reported), directory))
            .map(|(id, _)| {
                format!("running OpenCode session '{id}' currently occupies this directory")
            })
    }
}
