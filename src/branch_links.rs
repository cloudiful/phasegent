//! Stable repository identity and durable many-to-many branch/issue links.
//!
//! P2 built storage, identity, and read helpers; P3 integrates
//! bind/unbind/status, commit hooks, and close retention on top.

pub mod active;
pub mod compat;
pub mod identity;
pub mod import;
pub mod index_snapshot;
pub mod issue_key;
pub mod reads;
pub mod scope;
pub mod store;

// Re-exports are the P3 CLI/lifecycle surface; P2 has no production
// caller yet, so silence the unused-import lint like `worktree` does.
#[allow(unused_imports)]
pub use active::{ActiveIssue, resolve_active_issue};
#[allow(unused_imports)]
pub use compat::{CompatIssue, resolve_compat_issue};
#[allow(unused_imports)]
pub use identity::{
    LEGACY_SOURCE, LOCAL_KEY_PREFIX, ResolvedRepo, checkout_root, detect_default_branch,
    filesystem_fallback_key, is_default_branch, read_origin_url, repo_key_for_origin,
    resolve_repo_key, validate_not_default_branch,
};
#[allow(unused_imports)]
pub use import::{ImportSummary, LegacyBinding, import_legacy_bindings, list_legacy_bindings};
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
