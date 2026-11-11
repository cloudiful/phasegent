//! Tests for the worktree directory core.
//!
//! Coverage spans the pure Git helpers, the registry upgrade and read/write
//! surface, the per-repository operation lock, and the real-Git worktree
//! helpers. Every registry test uses an isolated temporary SQLite store so the
//! operator's real database is never touched.

use crate::infra::storage::Storage;
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::worktree::{
    DirectoryRecord, GitOutput, ProcessWorktreeRunner, WorktreeError, WorktreeListEntry,
    WorktreeRunner, cache_root_in, compute_fingerprint, generate_branch, is_clean, now_unix_secs,
    parse_worktree_list, repo_identity, slug_from_branch, validate_ref_format, worktree_add,
    worktree_remove,
};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;

mod git_worktree;
mod helpers;
mod opencode_fixture;
mod opencode_modes;
mod operation_lock;
mod registry;
pub(crate) mod support;
