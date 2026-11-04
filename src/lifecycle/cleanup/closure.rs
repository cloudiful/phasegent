//! The closure evidence a cleanup needs before it may remove a directory
//! that more than one issue is associated with.
//!
//! The lifecycle never reads a provider: it asks this narrow seam whether the
//! *other* issues a candidate directory is associated with are confirmed
//! closed. A directory shared with an open issue belongs to that issue too,
//! and removing it would take the other issue's worktree with it.
//!
//! The seam is deliberately narrow so the CLI can supply a scope-aware
//! resolver while the lifecycle stays free of provider and CLI knowledge.

/// The closure evidence for the issues a candidate directory is associated
/// with.
pub(crate) trait DirectoryIssueClosure {
    /// Whether every issue in `issues` other than `current_issue` is
    /// confirmed closed.
    ///
    /// `current_issue` is the issue whose closure the caller already knows —
    /// the close succeeded before the cleanup ran. It is never re-read, and
    /// no other issue may be assumed closed.
    fn confirm_others_closed(&self, current_issue: u64, issues: &[u64]) -> Result<(), String>;

    /// Drop any invocation-scoped memo so the next [`Self::confirm_others_closed`]
    /// reads provider and scope evidence afresh. The default is a no-op: a
    /// resolver that caches nothing has nothing to invalidate.
    ///
    /// A movement re-reads closure *after* its session moves: a cached
    /// `closed` answer must never authorize a removal that the moves made
    /// possible.
    fn refresh(&self) {}
}

/// The conservative answer: a directory another issue is associated with is
/// kept, whatever the caller knows about its own issue.
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
