//! Bounded operational diagnostics for `worktree prune` (issue 51 P2).
//!
//! An ordinary prune run leaves a typed, private trace of what it did, so an
//! incident can be diagnosed after the fact without a debug flag and without a
//! rerun. The trace is metadata only: no path, no session or issue identity,
//! no credential, no URL, no request or response body, and no raw error text.
//!
//! The module owns two narrow surfaces:
//!
//! * the writer ([`DiagLogger`]) appends typed events to a bounded, rotated,
//!   private NDJSON file set;
//! * the reader ([`query`]) reports those events without writing anything.
//!
//! Logging is best effort and never an authority. A busy lock, an unwritable
//! directory, or a failed append drops the event and reports one bounded
//! warning; the work the caller was doing is never affected, and the caller's
//! standard output is never touched. The logs carry no permission: a guard
//! never reads them, and a log line never authorizes a removal.

#[path = "diag_log/event.rs"]
mod event;
#[path = "diag_log/query.rs"]
mod query;
#[path = "diag_log/writer.rs"]
mod writer;

pub(crate) use event::{
    DiagEvent, EventKind, MODE_DRY_RUN, MODE_REMOVE, RunCounts, RunTimingEntry, RunTimings,
    is_safe_token,
};
#[cfg(test)]
pub(crate) use query::query_in;
pub(crate) use query::{LogQuery, query};
pub(crate) use writer::{
    ACTIVE_LOG, ARCHIVE_COUNT, DiagLogger, LOCK_FILE, MAX_FILE_BYTES, MAX_TOTAL_BYTES,
    RETENTION_DAYS, log_dir,
};

#[cfg(test)]
#[path = "diag_log/tests.rs"]
mod tests;
