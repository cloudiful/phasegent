#![allow(unused_imports)]
use crate::infra::storage::test_support::EnvGuard;
use crate::providers::redmine::model::{RedmineCurrentUser, RedmineCurrentUserResponse};
use crate::providers::{RedmineConfig, RedmineProvider};
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

pub(crate) fn time_entry_activities(activities: &[(u64, &str, bool)]) -> String {
    json!({
        "time_entry_activities": activities
            .iter()
            .map(|(id, name, is_default)| json!({"id": id, "name": name, "is_default": is_default}))
            .collect::<Vec<_>>(),
    })
    .to_string()
}

pub(crate) fn time_entry_response(
    id: u64,
    issue: u64,
    activity: u64,
    hours: f64,
    comments: &str,
    spent_on: &str,
) -> String {
    json!({
        "time_entry": {
            "id": id,
            "issue": {"id": issue},
            "activity": {"id": activity, "name": "Automation"},
            "hours": hours,
            "comments": comments,
            "spent_on": spent_on,
        }
    })
    .to_string()
}

pub(crate) fn time_entry_collection(entries: &[(u64, u64, u64, f64, &str, &str)]) -> String {
    let entries = entries
        .iter()
        .map(|(id, issue, activity, hours, comments, spent_on)| {
            json!({
                "id": id,
                "issue": {"id": issue},
                "activity": {"id": activity, "name": "Automation"},
                "hours": hours,
                "comments": comments,
                "spent_on": spent_on,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "total_count": entries.len(),
        "limit": 100,
        "time_entries": entries,
    })
    .to_string()
}
