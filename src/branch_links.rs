//! Stable repository identity and durable many-to-many branch/issue links.
//!
//! Durable rows are the sole branch/issue association: the resolver, the
//! commit hooks, and the desktop snapshots read these rows (plus the
//! branch-name fallback) and never consult local Git config.

pub mod active;
pub mod compat;
pub mod identity;
pub mod index_snapshot;
pub mod issue_key;
pub mod reads;
pub mod scope;
pub mod store;

// Re-exports are the CLI/lifecycle surface; some entries have no
// production caller in every build, so silence the unused-import lint
// like `worktree` does.
#[allow(unused_imports)]
pub use active::{ActiveIssue, resolve_active_issue};
#[allow(unused_imports)]
pub use compat::{
    CompatIssue, ScopedIssueRef, named_branch_issue, resolve_branch_issue, resolve_compat_issue,
    resolve_scoped_compat_issue,
};
#[allow(unused_imports)]
pub use identity::{
    LOCAL_KEY_PREFIX, ResolvedRepo, checkout_root, detect_default_branch, filesystem_fallback_key,
    is_default_branch, is_protected_branch, read_origin_url, repo_key_for_origin, resolve_repo_key,
    validate_not_default_branch, validate_not_protected_branch,
};
#[allow(unused_imports)]
pub use index_snapshot::{IndexSnapshot, SNAPSHOT_SOURCE, read_snapshot};
#[allow(unused_imports)]
pub use issue_key::IssueKey;
#[allow(unused_imports)]
pub use reads::{
    IssueStateLookup, LinkedBranch, LinkedIssue, StateSnapshot, UnknownState, branches_for_issue,
    issues_for_branch,
};
#[allow(unused_imports)]
pub use scope::{LinkScope, resolve_link_scope};
#[allow(unused_imports)]
pub use store::{DetachOutcome, LinkOutcome, LinkParams, detach, ensure_schema, link};
