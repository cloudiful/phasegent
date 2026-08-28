#![allow(unused_imports)]
use crate::infra::storage::test_support::EnvGuard;
use crate::providers::redmine::model::{RedmineCurrentUser, RedmineCurrentUserResponse};
use crate::providers::{RedmineConfig, RedmineProvider};
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

pub(crate) fn project_collection(
    total_count: usize,
    limit: usize,
    projects: &[(u64, &str, &str)],
) -> String {
    serde_json::json!({
        "total_count": total_count,
        "limit": limit,
        "projects": projects.iter().map(|(id, name, identifier)| serde_json::json!({
            "id": id,
            "name": name,
            "identifier": identifier,
            "description": "description",
            "is_public": false,
            "inherit_members": false,
        })).collect::<Vec<_>>(),
    })
    .to_string()
}

pub(crate) fn project_response(id: u64, name: &str, identifier: &str, description: &str) -> String {
    serde_json::json!({
        "project": {
            "id": id,
            "name": name,
            "identifier": identifier,
            "description": description,
            "is_public": false,
            "inherit_members": false,
        }
    })
    .to_string()
}

pub(crate) fn version_collection(versions: &[(u64, &str, &str, Option<&str>)]) -> String {
    version_collection_page(versions.len(), 100, versions)
}

pub(crate) fn version_collection_page(
    total_count: usize,
    limit: usize,
    versions: &[(u64, &str, &str, Option<&str>)],
) -> String {
    serde_json::json!({
        "total_count": total_count,
        "limit": limit,
        "versions": versions.iter().map(|(id, name, status, due_date)| serde_json::json!({
            "id": id,
            "name": name,
            "status": status,
            "due_date": due_date,
            "sharing": "none",
        })).collect::<Vec<_>>(),
    })
    .to_string()
}
