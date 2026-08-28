#![allow(unused_imports)]
use crate::infra::storage::test_support::EnvGuard;
use crate::providers::redmine::model::{RedmineCurrentUser, RedmineCurrentUserResponse};
use crate::providers::{RedmineConfig, RedmineProvider};
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

pub(crate) fn issue_response(
    id: u64,
    subject: &str,
    description: &str,
    closed: bool,
    journals: &[(u64, &str)],
) -> String {
    serde_json::json!({
        "issue": {
            "id": id,
            "subject": subject,
            "description": description,
            "status": {"name": if closed {"Closed"} else {"New"}, "is_closed": closed},
            "journals": journals.iter().map(|(journal_id, notes)| serde_json::json!({
                "id": journal_id,
                "notes": notes,
            })).collect::<Vec<_>>(),
        }
    })
    .to_string()
}

pub(crate) fn issue_collection(
    total_count: usize,
    limit: usize,
    issues: &[(u64, &str, bool)],
) -> String {
    serde_json::json!({
        "total_count": total_count,
        "limit": limit,
        "issues": issues.iter().map(|(id, subject, closed)| serde_json::json!({
            "id": id,
            "subject": subject,
            "description": "body",
            "status": {"name": if *closed {"Closed"} else {"New"}, "is_closed": *closed},
        })).collect::<Vec<_>>(),
    })
    .to_string()
}
