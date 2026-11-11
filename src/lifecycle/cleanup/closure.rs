//! The closure evidence a cleanup needs before it may remove a directory
//! that more than one association is recorded against.
//!
//! The lifecycle never reads a provider: it asks this narrow seam whether the
//! *other* associations a candidate directory is recorded against are
//! confirmed closed. Each association keeps its own complete identity: a
//! durable task is read through its immutable task route, a scoped legacy
//! association through its exact provider/project/external scope, and an
//! unscoped unknown association is never assumed closed. A directory shared
//! with an open or unprovable association belongs to it too, and removing it
//! would take that work with it.

use crate::worktree::{DirectoryLegacyIssue, DirectoryTask, WorktreeScope};

/// One association recorded against a candidate directory.
#[derive(Debug, Clone, Copy)]
pub(crate) enum DirectoryAssociation<'a> {
    Task(&'a DirectoryTask),
    Legacy(&'a DirectoryLegacyIssue),
}

impl DirectoryAssociation<'_> {
    pub(crate) fn issue(&self) -> u64 {
        match self {
            Self::Task(task) => task.issue,
            Self::Legacy(issue) => issue.issue,
        }
    }
}

/// The closure evidence for the associations a candidate directory is
/// recorded against.
pub(crate) trait DirectoryIssueClosure {
    /// Whether every issue in `issues` other than `current_issue` is
    /// confirmed closed.
    ///
    /// `current_issue` is the issue whose closure the caller already knows —
    /// the close succeeded before the cleanup ran. It is never re-read, and
    /// no other issue may be assumed closed. This number-only form is retained
    /// for callers that have no association evidence; destructive cleanup uses
    /// [`Self::confirm_association_closed`].
    #[allow(dead_code)]
    fn confirm_others_closed(&self, current_issue: u64, issues: &[u64]) -> Result<(), String>;

    /// Whether one complete association may be treated as closed.
    ///
    /// The exact current association is the only one that may be accepted
    /// without a provider read. A same-number association with another task
    /// id, instance, provider, project or external id is foreign evidence and
    /// is never hidden behind the current number.
    fn confirm_association_closed(
        &self,
        current_issue: u64,
        association: DirectoryAssociation<'_>,
        current_scope: Option<&WorktreeScope>,
    ) -> Result<(), String> {
        let _ = current_scope;
        self.confirm_others_closed(current_issue, &[association.issue()])
    }

    /// Drop any invocation-scoped memo so the next closure read uses fresh
    /// provider and scope evidence. The default is a no-op: a resolver that
    /// caches nothing has nothing to invalidate.
    ///
    /// A movement re-reads closure *after* its session moves: a cached
    /// `closed` answer must never authorize a removal that the moves made
    /// possible.
    fn refresh(&self) {}
}

/// The conservative answer: a directory another issue is associated with is
/// kept, whatever the caller knows about its own issue.
#[allow(dead_code)]
pub(crate) struct DenyOtherIssues;

impl DirectoryIssueClosure for DenyOtherIssues {
    fn confirm_others_closed(&self, current_issue: u64, issues: &[u64]) -> Result<(), String> {
        let others: Vec<String> = issues
            .iter()
            .filter(|issue| **issue != current_issue)
            .map(u64::to_string)
            .collect();
        match others.is_empty() {
            true => Ok(()),
            false => Err(format!(
                "the directory is also associated with issue(s) {} whose closure is not confirmed here",
                others.join(", ")
            )),
        }
    }
}
