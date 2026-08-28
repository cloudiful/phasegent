#![allow(unused_imports)]
use crate::infra::storage::test_support::EnvGuard;
use crate::providers::redmine::model::{RedmineCurrentUser, RedmineCurrentUserResponse};
use crate::providers::{RedmineConfig, RedmineProvider};
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

pub(crate) fn git_mirror_response(
    id: u64,
    project_id: u64,
    identifier: &str,
    status: &str,
    remote_url: Option<&str>,
    local_path: Option<&str>,
    error: Option<&str>,
) -> String {
    serde_json::json!({
        "id": id,
        "project_id": project_id,
        "identifier": identifier,
        "status": status,
        "remote_url": remote_url,
        "local_path": local_path,
        "error": error,
    })
    .to_string()
}
