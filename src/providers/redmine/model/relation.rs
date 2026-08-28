use crate::providers::api::ForgejoError;
use serde::{Deserialize, Serialize};

/// Redmine issue relation types used by this workflow.
///
/// Redmine stores a relation in one direction (`issue_id` -> `issue_to_id`)
/// with a stored type, but every pair also has an inverse name: `blocks` is
/// seen as `blocked` from the target issue's perspective, and `precedes` as
/// `follows`. `relates` is symmetric. Only the canonical names are accepted
/// as CLI input; the inverse names are only decoded from server responses so
/// list output can render each relation from the queried issue's viewpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RedmineRelationType {
    Blocks,
    Precedes,
    Relates,
    Blocked,
    Follows,
}

impl RedmineRelationType {
    /// Strict parse of the CLI-facing canonical names. Inverse names are
    /// rejected so callers can never create a relation whose direction
    /// contradicts its label.
    pub(crate) fn parse_input(value: &str) -> Result<Self, ForgejoError> {
        match value {
            "blocks" => Ok(Self::Blocks),
            "precedes" => Ok(Self::Precedes),
            "relates" => Ok(Self::Relates),
            other => Err(ForgejoError::config(format!(
                "Redmine relation type must be blocks, precedes, or relates (got '{other}')"
            ))),
        }
    }

    /// Strict decode of a server-side relation type, including inverse
    /// names Redmine reports for relations stored from the opposite side.
    pub(crate) fn parse(value: &str) -> Result<Self, ForgejoError> {
        match value {
            "blocks" => Ok(Self::Blocks),
            "precedes" => Ok(Self::Precedes),
            "relates" => Ok(Self::Relates),
            "blocked" => Ok(Self::Blocked),
            "follows" => Ok(Self::Follows),
            other => Err(ForgejoError::config(format!(
                "unknown Redmine relation type '{other}'"
            ))),
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Blocks => "blocks",
            Self::Precedes => "precedes",
            Self::Relates => "relates",
            Self::Blocked => "blocked",
            Self::Follows => "follows",
        }
    }

    /// The relation as seen from the opposite issue. Redmine records a pair
    /// once (from `issue_id` to `issue_to_id`); `blocks` reads as `blocked`
    /// and `precedes` reads as `follows` from the target, while `relates` is
    /// symmetric. `relation list` renders each relation from the queried
    /// issue's viewpoint using exactly this mapping.
    pub(crate) const fn inverse(self) -> Self {
        match self {
            Self::Blocks => Self::Blocked,
            Self::Blocked => Self::Blocks,
            Self::Precedes => Self::Follows,
            Self::Follows => Self::Precedes,
            Self::Relates => Self::Relates,
        }
    }
}

/// A single Redmine issue relation as returned by
/// `/issues/:id/relations.json`. `relation_type` is the raw server value
/// (one of `blocks`, `precedes`, `relates`, `blocked`, `follows`); list
/// output re-derives the viewpoint-resolved name.
#[derive(Debug, Deserialize)]
pub(crate) struct RedmineRelation {
    pub(crate) id: u64,
    pub(crate) issue_id: u64,
    pub(crate) issue_to_id: u64,
    pub(crate) relation_type: String,
    #[serde(default)]
    pub(crate) delay: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineRelationCollection {
    #[serde(default)]
    pub(crate) relations: Vec<RedmineRelation>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineRelationResponse {
    pub(crate) relation: RedmineRelation,
}

/// Request body for `POST /issues/:id/relations.json`. `relation_type` is one
/// of the canonical CLI names (`blocks`, `precedes`, `relates`); `delay` is
/// only ever set for `precedes` and is omitted otherwise so the payload stays
/// minimal.
#[derive(Debug, Serialize)]
pub(crate) struct RedmineNewRelation<'a> {
    pub(crate) relation: RedmineNewRelationFields<'a>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineNewRelationFields<'a> {
    pub(crate) issue_to_id: u64,
    pub(crate) relation_type: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) delay: Option<u64>,
}

impl<'a> RedmineNewRelation<'a> {
    pub(crate) fn new(issue_to_id: u64, relation_type: &'a str, delay: Option<u64>) -> Self {
        Self {
            relation: RedmineNewRelationFields {
                issue_to_id,
                relation_type,
                delay,
            },
        }
    }
}

/// Public, serializable output for one issue relation. `relation_type` is the
/// name resolved from the queried issue's viewpoint (so a `blocks` stored
/// from the source's side shows as `blocked` when listing the target), and
/// `issue_id` / `issue_to_id` carry the raw endpoints so callers can see the
/// direction.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct RelationSummary {
    pub id: u64,
    pub relation_type: String,
    pub issue_id: u64,
    pub issue_to_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delay: Option<u64>,
}

impl RedmineRelation {
    /// Render this relation from the viewpoint of `queried_issue_id`. When the
    /// queried issue is the relation's `issue_to_id`, the stored type is
    /// inverted (`blocks` -> `blocked`, `precedes` -> `follows`) so the
    /// output matches what a user inspecting that issue expects. Unknown
    /// server types are surfaced verbatim rather than dropped.
    pub(crate) fn into_summary(self, queried_issue_id: u64) -> RelationSummary {
        let relation_type = match RedmineRelationType::parse(&self.relation_type) {
            Ok(parsed) if self.issue_id == queried_issue_id => parsed.as_str().to_owned(),
            Ok(parsed) => parsed.inverse().as_str().to_owned(),
            Err(_) => self.relation_type.clone(),
        };
        RelationSummary {
            id: self.id,
            relation_type,
            issue_id: self.issue_id,
            issue_to_id: self.issue_to_id,
            delay: self.delay,
        }
    }
}
