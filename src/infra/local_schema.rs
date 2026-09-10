//! Local provider storage constants.
//!
//! SQLite file `phasegent-local.sqlite3` is independent from
//! `phasegent.sqlite3` (config/credentials) and
//! `phasegent-index.sqlite3` (lexical index). DDL/seeds live in
//! `local_sql/*.sql` and are embedded via `include_str!` so the file
//! stays reviewable. Status edges mirror
//! `providers::redmine::model::status::STATUS_TRANSITIONS`.

/// Filename of the independent local SQLite inside the config dir.
pub(crate) const DB_FILENAME_LOCAL: &str = "phasegent-local.sqlite3";

/// PRAGMAs for the local connection. Copied from
/// `storage/connection.rs` and `issue_index_schema.rs` (WAL +
/// busy_timeout) so concurrent CLI invocations behave identically.
pub(crate) const PRAGMA_STATEMENTS_LOCAL: &str = "\
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;
";

/// SQLite DDL for the local backend (single source: local_sql/schema.sql).
pub(crate) const SCHEMA_LOCAL: &str = include_str!("local_sql/schema.sql");

/// Idempotent seeds (single source: local_sql/seed.sql).
pub(crate) const SEED_LOCAL: &str = include_str!("local_sql/seed.sql");

/// Canonical status edges mirrored from Redmine `STATUS_TRANSITIONS`.
/// Kept as data (not code) so seed verification and policy checks
/// share one literal without importing the redmine model here.
pub(crate) const STATUS_TRANSITION_SEED: &[(&str, &str)] = &[
    ("New", "In Progress"),
    ("New", "Cancelled"),
    ("In Progress", "In Review"),
    ("In Progress", "Blocked"),
    ("In Progress", "Cancelled"),
    ("In Review", "Resolved"),
    ("In Review", "Changes Requested"),
    ("In Review", "Blocked"),
    ("In Review", "Cancelled"),
    ("Changes Requested", "In Progress"),
    ("Changes Requested", "Blocked"),
    ("Changes Requested", "Cancelled"),
    ("Blocked", "In Progress"),
    ("Blocked", "Cancelled"),
    ("Resolved", "In Progress"),
    ("Resolved", "Closed"),
];

/// Expected seed row count (16 edges; Closed/Cancelled are terminal).
#[cfg(test)]
pub(crate) const STATUS_TRANSITION_SEED_LEN: usize = 16;
