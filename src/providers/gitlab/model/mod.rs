//! GitLab REST v4 models aggregator.
//!
//! The structures are deliberately narrow: only the fields the
//! orchestrator CLI actually consumes are decoded. Adding fields is
//! cheap; misinterpreting an unknown GitLab payload field as an
//! authoritative state value is not, so the decoders stay minimal.

pub mod dto;
pub mod duration;
pub mod labels;
pub mod relations;
pub mod time;
#[cfg(test)]
pub(crate) use dto::ApiProjectNamespace;
pub(crate) use dto::{
    ApiError, ApiIssue, ApiLabel, ApiMilestone, ApiNamespace, ApiNote, ApiProject, NewIssue,
    NewLabel, NewNote, NewProject, UpdateIssue,
};
pub(crate) use duration::format_gitlab_duration;
pub(crate) use labels::{
    TRACKER_LABEL_BUG, TRACKER_LABEL_FEATURE, WORKFLOW_LABELS, state_from_gitlab,
    state_query_filter, tracker_label_from_name, tracker_name_from_label,
    workflow_label_from_status,
};
#[cfg(test)]
pub(crate) use labels::{
    WORKFLOW_LABEL_BLOCKED, WORKFLOW_LABEL_CANCELLED, WORKFLOW_LABEL_CHANGES_REQUESTED,
    WORKFLOW_LABEL_CLOSED, WORKFLOW_LABEL_IN_PROGRESS, WORKFLOW_LABEL_IN_REVIEW,
    WORKFLOW_LABEL_NEW, WORKFLOW_LABEL_RESOLVED,
};
pub(crate) use relations::{
    ApiIssueLink, gitlab_create_supports_relation_type, gitlab_link_type_from_relation_type,
    gitlab_link_type_to_relation_type,
};
pub(crate) use time::{ApiIssueTimeStats, ApiSpentTimeSummary, NewSpentTime, NewTimeEstimate};
