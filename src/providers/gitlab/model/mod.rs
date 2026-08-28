//! GitLab REST v4 models aggregator.
//!
//! The structures are deliberately narrow: only the fields the
//! orchestrator CLI actually consumes are decoded. Adding fields is
//! cheap; misinterpreting an unknown GitLab payload field as an
//! authoritative state value is not, so the decoders stay minimal.

#![allow(unused_imports)]

pub mod ci;
pub mod dto;
pub mod duration;
pub mod labels;
pub mod relations;
pub mod time;

pub(crate) use ci::{
    ApiJob, ApiJobPipelineRef, ApiPipeline, pipeline_conclusion_from_gitlab,
    pipeline_status_from_gitlab,
};
pub(crate) use dto::{
    ApiError, ApiIssue, ApiLabel, ApiNamespace, ApiNote, ApiProject, ApiProjectNamespace, NewIssue,
    NewLabel, NewNote, NewProject, UpdateIssue,
};
pub(crate) use duration::format_gitlab_duration;
pub(crate) use labels::{
    TRACKER_LABEL_BUG, TRACKER_LABEL_FEATURE, WORKFLOW_LABEL_BLOCKED, WORKFLOW_LABEL_CANCELLED,
    WORKFLOW_LABEL_CHANGES_REQUESTED, WORKFLOW_LABEL_CLOSED, WORKFLOW_LABEL_IN_PROGRESS,
    WORKFLOW_LABEL_IN_REVIEW, WORKFLOW_LABEL_NEW, WORKFLOW_LABEL_RESOLVED, WORKFLOW_LABELS,
    state_from_gitlab, state_query_filter, tracker_label_from_name, tracker_name_from_label,
    workflow_label_from_status,
};
pub(crate) use relations::{
    ApiIssueLink, ApiIssueLinkEndpoint, ApiIssueLinkIssue, gitlab_create_supports_relation_type,
    gitlab_link_type_from_relation_type, gitlab_link_type_to_relation_type,
};
pub(crate) use time::{ApiIssueTimeStats, ApiSpentTimeSummary, NewSpentTime, NewTimeEstimate};
