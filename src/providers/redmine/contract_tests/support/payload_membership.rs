#![allow(unused_imports)]
use crate::infra::storage::test_support::EnvGuard;
use crate::providers::redmine::model::{RedmineCurrentUser, RedmineCurrentUserResponse};
use crate::providers::{RedmineConfig, RedmineProvider};
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

pub(crate) fn role_collection(roles: &[(u64, &str)]) -> String {
    role_collection_page(roles.len(), 100, roles)
}

pub(crate) fn role_collection_page(
    total_count: usize,
    limit: usize,
    roles: &[(u64, &str)],
) -> String {
    serde_json::json!({
        "total_count": total_count,
        "limit": limit,
        "roles": roles.iter().map(|(id, name)| serde_json::json!({
            "id": id,
            "name": name,
        })).collect::<Vec<_>>(),
    })
    .to_string()
}

pub(crate) fn membership_collection(membership: Option<(u64, u64, &str, Vec<u64>)>) -> String {
    membership_collection_page(usize::from(membership.is_some()), 100, membership)
}

pub(crate) fn membership_collection_page(
    total_count: usize,
    limit: usize,
    membership: Option<(u64, u64, &str, Vec<u64>)>,
) -> String {
    let memberships = membership
        .into_iter()
        .map(|(id, user_id, user_login, role_ids)| {
            serde_json::json!({
                "id": id,
                "user": {"id": user_id, "login": user_login},
                "roles": role_ids
                    .into_iter()
                    .map(|role_id| serde_json::json!({"id": role_id}))
                    .collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "total_count": total_count,
        "limit": limit,
        "memberships": memberships,
    })
    .to_string()
}

pub(crate) fn current_user_response(id: u64, login: &str) -> String {
    serde_json::json!({
        "user": {
            "id": id,
            "login": login,
            "firstname": "Test",
            "lastname": "User",
            "mail": format!("{login}@example.test"),
        }
    })
    .to_string()
}

pub(crate) fn user_from_response(body: &str) -> RedmineCurrentUser {
    let response: RedmineCurrentUserResponse =
        serde_json::from_str(body).expect("current user response should deserialize");
    response.user
}
