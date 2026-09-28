//! Composite issue key: provider/project/external-ID triple.
//!
//! Mirrors the issue-index key shape so link rows join cleanly onto the
//! last-known local-index snapshot. Bounds match
//! `crate::providers::index` without depending on it.

//! P2 foundation API; production CLI wiring lands in P3.
#![allow(dead_code)]

use std::fmt;

pub const MAX_PROVIDER_LEN: usize = 64;
pub const MAX_PROJECT_LEN: usize = 200;
pub const MAX_EXTERNAL_ID_LEN: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct IssueKey {
    pub provider: String,
    pub project: String,
    pub external_id: String,
}

impl IssueKey {
    pub fn new(
        provider: impl Into<String>,
        project: impl Into<String>,
        external_id: impl Into<String>,
    ) -> Result<Self, String> {
        let provider = provider.into();
        let project = project.into();
        let external_id = external_id.into();
        validate_part(&provider, "provider", MAX_PROVIDER_LEN)?;
        validate_part(&project, "project", MAX_PROJECT_LEN)?;
        validate_part(&external_id, "external_id", MAX_EXTERNAL_ID_LEN)?;
        Ok(Self {
            provider: provider.trim().to_owned(),
            project: project.trim().to_owned(),
            external_id: external_id.trim().to_owned(),
        })
    }

    /// Build a key from a numeric provider issue id.
    pub fn from_number(
        provider: impl Into<String>,
        project: impl Into<String>,
        issue: u64,
    ) -> Result<Self, String> {
        if issue == 0 {
            return Err("issue number must be greater than zero".to_owned());
        }
        Self::new(provider, project, issue.to_string())
    }

    pub fn issue_number(&self) -> Option<u64> {
        self.external_id.parse::<u64>().ok().filter(|id| *id > 0)
    }
}

impl fmt::Display for IssueKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{}:{}",
            self.provider, self.project, self.external_id
        )
    }
}

fn validate_part(value: &str, field: &str, max_chars: usize) -> Result<(), String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!("{field} must be non-empty"));
    }
    if trimmed.chars().count() > max_chars {
        return Err(format!("{field} must be at most {max_chars} characters"));
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return Err(format!("{field} must not contain control characters"));
    }
    Ok(())
}
