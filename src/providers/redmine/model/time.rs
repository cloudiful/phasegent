use serde::{Deserialize, Serialize};

/// A time-entry activity exposed by Redmine's enumeration API.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct RedmineTimeEntryActivity {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub is_default: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineTimeEntryActivityCollection {
    #[serde(default)]
    pub(crate) time_entry_activities: Vec<RedmineTimeEntryActivity>,
}

/// A Time Entry returned by Redmine. Optional nested fields let list calls
/// remain compatible with installations that omit one or more projections.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RedmineTimeEntry {
    #[serde(default)]
    pub(crate) id: u64,
    #[serde(default)]
    pub(crate) issue: Option<RedmineTimeEntryIssue>,
    #[serde(default)]
    pub(crate) activity: Option<RedmineTimeEntryActivity>,
    #[serde(default)]
    pub(crate) hours: Option<f64>,
    #[serde(default)]
    pub(crate) comments: Option<String>,
    #[serde(default)]
    pub(crate) spent_on: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RedmineTimeEntryIssue {
    pub(crate) id: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RedmineTimeEntryCollection {
    #[serde(default)]
    pub(crate) time_entries: Vec<RedmineTimeEntry>,
    pub(crate) total_count: Option<usize>,
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineTimeEntryResponse {
    #[serde(default)]
    pub(crate) time_entry: Option<RedmineTimeEntry>,
}

/// Native Redmine Time Entry request payload.
#[derive(Debug, Serialize)]
pub(crate) struct RedmineNewTimeEntry<'a> {
    pub(crate) time_entry: RedmineNewTimeEntryFields<'a>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineNewTimeEntryFields<'a> {
    pub(crate) issue_id: u64,
    pub(crate) hours: f64,
    pub(crate) spent_on: &'a str,
    pub(crate) activity_id: u64,
    pub(crate) comments: &'a str,
}
