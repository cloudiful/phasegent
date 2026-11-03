//! Additive SQLite ownership for captured pre-switch checkout baselines.
//!
//! One row records the branch and commit a primary checkout was on before
//! the automatic issue-branch switch, keyed by the durable repository
//! identity plus provider/project/issue/path scope. The table is created
//! lazily with `CREATE TABLE IF NOT EXISTS`, so pre-existing databases gain
//! it on open without altering or deleting any legacy row.

use rusqlite::{OptionalExtension, params};

const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS checkout_baselines (
    repo_key TEXT NOT NULL,
    provider TEXT NOT NULL,
    project TEXT NOT NULL,
    issue_number INTEGER NOT NULL CHECK (issue_number > 0),
    checkout_path TEXT NOT NULL,
    issue_branch TEXT NOT NULL,
    baseline_branch TEXT NOT NULL,
    baseline_head TEXT NOT NULL,
    clean_evidence TEXT NOT NULL,
    default_branch TEXT,
    recorded_at INTEGER NOT NULL CHECK (recorded_at > 0),
    state TEXT NOT NULL CHECK (state IN ('pending', 'active', 'aborted')),
    update_state TEXT NOT NULL CHECK (update_state IN ('none', 'pending', 'updated', 'skipped', 'failed')),
    restored_at INTEGER CHECK (restored_at IS NULL OR restored_at > 0),
    PRIMARY KEY (repo_key, provider, project, issue_number, checkout_path)
);

CREATE INDEX IF NOT EXISTS checkout_baselines_issue_idx
    ON checkout_baselines (provider, project, issue_number);
";

pub(crate) const CLEAN_EVIDENCE: &str = "git status --porcelain empty";

pub(crate) fn ensure_schema(connection: &rusqlite::Connection) -> Result<(), String> {
    connection
        .execute_batch(SCHEMA)
        .map_err(|error| format!("could not initialise checkout baseline table: {error}"))?;
    Ok(())
}

/// Composite scope of one captured checkout baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BaselineScope {
    pub repo_key: String,
    pub provider: String,
    pub project: String,
    pub issue_number: u64,
    pub checkout_path: String,
}

impl BaselineScope {
    pub(crate) fn new(
        repo_key: &str,
        provider: &str,
        project: &str,
        issue_number: u64,
        checkout_path: &str,
    ) -> Result<Self, String> {
        for (value, field) in [
            (repo_key, "repo_key"),
            (provider, "provider"),
            (project, "project"),
            (checkout_path, "checkout_path"),
        ] {
            if value.trim().is_empty() {
                return Err(format!("{field} must be non-empty"));
            }
            if value.chars().any(char::is_control) {
                return Err(format!("{field} must not contain control characters"));
            }
        }
        if issue_number == 0 {
            return Err("issue_number must be greater than zero".to_owned());
        }
        Ok(Self {
            repo_key: repo_key.to_owned(),
            provider: provider.to_owned(),
            project: project.to_owned(),
            issue_number,
            checkout_path: checkout_path.to_owned(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckoutState {
    Pending,
    Active,
    Aborted,
}

impl CheckoutState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Aborted => "aborted",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "pending" => Ok(Self::Pending),
            "active" => Ok(Self::Active),
            "aborted" => Ok(Self::Aborted),
            other => Err(format!("unknown checkout baseline state '{other}'")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpdateState {
    None,
    Pending,
    Updated,
    Skipped,
    Failed,
}

impl UpdateState {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Pending => "pending",
            Self::Updated => "updated",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "none" => Ok(Self::None),
            "pending" => Ok(Self::Pending),
            "updated" => Ok(Self::Updated),
            "skipped" => Ok(Self::Skipped),
            "failed" => Ok(Self::Failed),
            other => Err(format!("unknown checkout baseline update state '{other}'")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BaselineRecord {
    pub scope: BaselineScope,
    pub issue_branch: String,
    pub baseline_branch: String,
    pub baseline_head: String,
    pub clean_evidence: String,
    pub default_branch: Option<String>,
    pub recorded_at: i64,
    pub state: CheckoutState,
    pub update_state: UpdateState,
    pub restored_at: Option<i64>,
}

pub(crate) struct NewBaseline<'a> {
    pub scope: &'a BaselineScope,
    pub issue_branch: &'a str,
    pub baseline_branch: &'a str,
    pub baseline_head: &'a str,
    pub clean_evidence: &'a str,
    pub default_branch: Option<&'a str>,
    pub recorded_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InsertOutcome {
    Created(BaselineRecord),
    Preserved(BaselineRecord),
}

fn validate_new(new: &NewBaseline<'_>) -> Result<(), String> {
    for (value, field) in [
        (new.issue_branch, "issue_branch"),
        (new.baseline_branch, "baseline_branch"),
        (new.baseline_head, "baseline_head"),
        (new.clean_evidence, "clean_evidence"),
    ] {
        if value.trim().is_empty() {
            return Err(format!("{field} must be non-empty"));
        }
        if value.chars().any(char::is_control) {
            return Err(format!("{field} must not contain control characters"));
        }
    }
    if let Some(default) = new.default_branch {
        if default.trim().is_empty() {
            return Err("default_branch must not be empty when present".to_owned());
        }
        if default.chars().any(char::is_control) {
            return Err("default_branch must not contain control characters".to_owned());
        }
    }
    if new.recorded_at <= 0 {
        return Err("recorded_at must be greater than zero".to_owned());
    }
    Ok(())
}

/// Insert the first valid pending baseline for `new.scope`.
///
/// A repeated capture never overwrites the original evidence: the existing
/// row is read back and returned as [`InsertOutcome::Preserved`].
pub(crate) fn insert_pending(
    connection: &rusqlite::Connection,
    new: &NewBaseline<'_>,
) -> Result<InsertOutcome, String> {
    validate_new(new)?;
    let scope = new.scope;
    let inserted = connection
        .execute(
            "INSERT INTO checkout_baselines \
             (repo_key, provider, project, issue_number, checkout_path, issue_branch, \
              baseline_branch, baseline_head, clean_evidence, default_branch, recorded_at, \
              state, update_state, restored_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'pending', 'none', NULL) \
             ON CONFLICT(repo_key, provider, project, issue_number, checkout_path) DO NOTHING",
            params![
                scope.repo_key,
                scope.provider,
                scope.project,
                scope.issue_number as i64,
                scope.checkout_path,
                new.issue_branch,
                new.baseline_branch,
                new.baseline_head,
                new.clean_evidence,
                new.default_branch,
                new.recorded_at
            ],
        )
        .map_err(|error| format!("could not record checkout baseline: {error}"))?;
    let record = read_baseline(connection, scope)?
        .ok_or_else(|| "checkout baseline disappeared after insert".to_owned())?;
    if inserted == 1 {
        Ok(InsertOutcome::Created(record))
    } else {
        Ok(InsertOutcome::Preserved(record))
    }
}

pub(crate) fn read_baseline(
    connection: &rusqlite::Connection,
    scope: &BaselineScope,
) -> Result<Option<BaselineRecord>, String> {
    connection
        .query_row(
            "SELECT issue_branch, baseline_branch, baseline_head, clean_evidence, \
                    default_branch, recorded_at, state, update_state, restored_at \
             FROM checkout_baselines \
             WHERE repo_key = ?1 AND provider = ?2 AND project = ?3 \
               AND issue_number = ?4 AND checkout_path = ?5",
            params![
                scope.repo_key,
                scope.provider,
                scope.project,
                scope.issue_number as i64,
                scope.checkout_path
            ],
            |row| {
                let state: String = row.get(6)?;
                let update_state: String = row.get(7)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                    state,
                    update_state,
                    row.get::<_, Option<i64>>(8)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("could not read checkout baseline: {error}"))?
        .map(|row| {
            Ok(BaselineRecord {
                scope: scope.clone(),
                issue_branch: row.0,
                baseline_branch: row.1,
                baseline_head: row.2,
                clean_evidence: row.3,
                default_branch: row.4,
                recorded_at: row.5,
                state: CheckoutState::parse(&row.6)?,
                update_state: UpdateState::parse(&row.7)?,
                restored_at: row.8,
            })
        })
        .transpose()
}

/// Candidate baseline rows for one exact provider/project/issue scope.
/// The caller verifies each candidate against a shared repository identity
/// before treating it as evidence; this never authorises restoration.
pub(crate) fn baseline_candidates_for_scope(
    connection: &rusqlite::Connection,
    provider: &str,
    project: &str,
    issue_number: u64,
) -> Result<Vec<(String, String)>, String> {
    let mut statement = connection
        .prepare(
            "SELECT repo_key, checkout_path FROM checkout_baselines \
             WHERE provider = ?1 AND project = ?2 AND issue_number = ?3",
        )
        .map_err(|error| format!("could not prepare checkout baseline candidates: {error}"))?;
    let rows = statement
        .query_map(params![provider, project, issue_number as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| format!("could not read checkout baseline candidates: {error}"))?;
    let mut candidates = Vec::new();
    for row in rows {
        candidates.push(
            row.map_err(|error| format!("could not read checkout baseline candidate: {error}"))?,
        );
    }
    Ok(candidates)
}

/// True when a baseline row exists for this repository and exact
/// provider/project/issue scope, regardless of checkout path. Used only to
/// decide whether an unresolvable main checkout should warn instead of
/// staying a quiet no-op.
pub(crate) fn baseline_exists_for_issue(
    connection: &rusqlite::Connection,
    repo_key: &str,
    provider: &str,
    project: &str,
    issue_number: u64,
) -> Result<bool, String> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM checkout_baselines \
             WHERE repo_key = ?1 AND provider = ?2 AND project = ?3 AND issue_number = ?4)",
            params![repo_key, provider, project, issue_number as i64],
            |row| row.get::<_, i64>(0),
        )
        .map(|found| found == 1)
        .map_err(|error| format!("could not read checkout baselines: {error}"))
}

/// Move a pending row to `new_state`. Only the `pending` state may
/// transition, so a late probe can never rewrite a finalized baseline.
/// Record that the baseline branch itself has been restored. The first
/// timestamp is retained on retries so a later close cannot rewrite when the
/// restoration actually happened.
pub(crate) fn mark_restored(
    connection: &rusqlite::Connection,
    scope: &BaselineScope,
    update_state: UpdateState,
    now: i64,
) -> Result<bool, String> {
    if now <= 0 {
        return Err("restored_at must be greater than zero".to_owned());
    }
    let changed = connection
        .execute(
            "UPDATE checkout_baselines \
             SET restored_at = COALESCE(restored_at, ?6), update_state = ?7 \
             WHERE repo_key = ?1 AND provider = ?2 AND project = ?3 \
               AND issue_number = ?4 AND checkout_path = ?5",
            params![
                scope.repo_key,
                scope.provider,
                scope.project,
                scope.issue_number as i64,
                scope.checkout_path,
                now,
                update_state.as_str()
            ],
        )
        .map_err(|error| format!("could not record restored checkout baseline: {error}"))?;
    Ok(changed == 1)
}

/// Set the independent upstream-update state without touching the branch
/// restoration timestamp.
pub(crate) fn set_update_state(
    connection: &rusqlite::Connection,
    scope: &BaselineScope,
    update_state: UpdateState,
) -> Result<bool, String> {
    let changed = connection
        .execute(
            "UPDATE checkout_baselines SET update_state = ?6 \
             WHERE repo_key = ?1 AND provider = ?2 AND project = ?3 \
               AND issue_number = ?4 AND checkout_path = ?5",
            params![
                scope.repo_key,
                scope.provider,
                scope.project,
                scope.issue_number as i64,
                scope.checkout_path,
                update_state.as_str()
            ],
        )
        .map_err(|error| format!("could not update checkout baseline update state: {error}"))?;
    Ok(changed == 1)
}

pub(crate) fn transition_pending(
    connection: &rusqlite::Connection,
    scope: &BaselineScope,
    new_state: CheckoutState,
) -> Result<bool, String> {
    if new_state == CheckoutState::Pending {
        return Err("pending is an initial state, not a transition target".to_owned());
    }
    let changed = connection
        .execute(
            "UPDATE checkout_baselines SET state = ?6 \
             WHERE repo_key = ?1 AND provider = ?2 AND project = ?3 \
               AND issue_number = ?4 AND checkout_path = ?5 AND state = 'pending'",
            params![
                scope.repo_key,
                scope.provider,
                scope.project,
                scope.issue_number as i64,
                scope.checkout_path,
                new_state.as_str()
            ],
        )
        .map_err(|error| format!("could not update checkout baseline state: {error}"))?;
    Ok(changed == 1)
}
