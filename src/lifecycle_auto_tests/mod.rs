//! Integration tests for the Phase 2 lifecycle auto-accounting path.
//!
//! These tests exercise the public `lifecycle_auto` surface against
//! a real SQLite ledger opened via `Storage::open_at` with a
//! `PHASEGENT_DB_PATH` override. They prove the documented
//! invariants end-to-end:
//!
//! - the auto-`start` opens a `running` row with the expected
//!   `auto-`-prefixed `run_id`;
//! - the auto-`finish` closes the open row with the right
//!   `elapsed_seconds` and `rounded_hours`;
//! - a second status transition closes the first segment and opens
//!   a fresh one;
//! - a crash/legacy ledger with multiple `running` rows for the
//!   same issue is reconciled by finishing every one and opening a
//!   single new row;
//! - the close path finishes every running row for the issue;
//! - cumulative sum across multiple same-state segments equals the
//!   `elapsed_seconds` sum of the individual finished rows;
//! - Forgejo is a no-op and the helper never touches the ledger.

use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::infra::storage::{Storage, TIMER_STATUS_RUNNING, TimerRun, TimerStatusFilter};
use crate::lifecycle_auto::{
    AutoCloseOutcome, AutoTimerOutcome, auto_close_issue_timer, auto_transition_timer,
};
use crate::providers::ProviderKind;
use crate::worktree::WorktreeRunner;
use std::fs;

mod auto_close;
mod auto_recovery;
mod auto_route;
mod auto_timer;
mod auto_transition;
mod close_cleanup;
mod close_release;
mod support;
