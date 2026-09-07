//! Redacted IPC shapes shared by the GUI backend and the frontend.
//!
//! All structs stay `Serialize`/`Deserialize` so the Tauri `invoke`
//! boundary remains typed. No struct carries a credential value:
//! secrets are write-only inputs and presence/length-only outputs.

use serde::{Deserialize, Serialize};

/// Current branch binding. `warning` carries a bounded non-secret
/// reason when Git is unavailable; callers keep last-known data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchContextPayload {
    pub branch: Option<String>,
    pub issue_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// Bounded task-list request. All fields optional; defaults keep the
/// query small and provider-friendly.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TasksRequest {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub state: Option<String>,
}

/// Single task row. `url` is sanitised (no userinfo/query/fragment).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEntry {
    pub number: u64,
    pub title: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Bounded task list with branch context for stale-friendly UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TasksPayload {
    pub branch: Option<String>,
    pub bound_issue: Option<u64>,
    pub provider: String,
    pub role: String,
    pub items: Vec<TaskEntry>,
    pub total_count: Option<usize>,
    pub has_more: bool,
    pub data_source: String,
    pub fetched_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// Status request (role/provider optional, resolved via existing chain).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StatusRequest {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
}

/// Minimal timer row (no secrets, no projection tokens).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimerDto {
    pub run_id: String,
    pub issue: u64,
    pub phase: String,
    pub role: String,
    pub status: String,
    pub sync_status: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

/// Status payload: branch, bound issue, sanitised endpoint, timers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusPayload {
    pub branch: Option<String>,
    pub bound_issue: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bound_issue_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bound_issue_state: Option<String>,
    pub provider: String,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    pub connection: String,
    pub running_timers: usize,
    pub recent_timers: Vec<TimerDto>,
    pub fetched_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statuses_unsupported: Option<String>,
    /// Short content hash of the embedded `frontend/dist` tree. Lets the
    /// operator confirm the running binary embeds the same bundle their
    /// build produced (`dist:hash`). Always present in gui builds so it
    /// also keeps the dist hash a live compile input for freshness.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frontend_dist_hash: Option<String>,
}

/// Non-secret setting mutation (secrets rejected here).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetSettingRequest {
    #[serde(default)]
    pub role: Option<String>,
    pub setting: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetSettingResponse {
    pub setting: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub updated: bool,
}

/// Non-secret setting clear.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClearSettingRequest {
    #[serde(default)]
    pub role: Option<String>,
    pub setting: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClearSettingResponse {
    pub setting: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub cleared: bool,
}

/// Write-only credential set (response never echoes the value).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetCredentialRequest {
    pub role: String,
    pub provider: String,
    pub credential: String,
}

/// Redacted credential presence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialPresence {
    pub role: String,
    pub provider: String,
    pub present: bool,
    pub length: usize,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClearCredentialRequest {
    pub role: String,
    pub provider: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClearCredentialResponse {
    pub role: String,
    pub provider: String,
    pub cleared: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisioningQuery {
    pub role: String,
}

/// Admin-provisioned Redmine identity (non-secret only).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisioningStatus {
    pub role: String,
    pub user_id: Option<u64>,
    pub login: Option<String>,
    pub credential_present: bool,
    pub credential_length: usize,
}
