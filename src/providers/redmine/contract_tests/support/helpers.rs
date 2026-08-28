#![allow(unused_imports)]
use crate::infra::storage::test_support::EnvGuard;
use crate::providers::redmine::model::{RedmineCurrentUser, RedmineCurrentUserResponse};
use crate::providers::{RedmineConfig, RedmineProvider};
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

pub(crate) fn strings<const N: usize>(values: [&str; N]) -> Vec<String> {
    values.into_iter().map(str::to_owned).collect()
}

pub(crate) fn mirror_env() -> (EnvGuard, EnvGuard) {
    let key = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    let url = EnvGuard::set(
        "PHASEGENT_REDMINE_REPOSITORY_URL",
        "https://git.example.com/owner/repo.git",
    );
    (key, url)
}
