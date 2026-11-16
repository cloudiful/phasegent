//! Unit tests for the pure worktree naming and listing helpers.

use crate::worktree::{
    WorktreeListEntry, compute_fingerprint, generate_branch, parse_worktree_list, slug_from_branch,
    validate_ref_format,
};

mod helpers;
