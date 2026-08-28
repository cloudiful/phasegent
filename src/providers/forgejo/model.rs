use crate::providers::api::{CommentOutput, IssueSummary, RepoSummary};
use serde::{Deserialize, Serialize};

impl CommentOutput {
    pub fn from_api(comment: ApiComment, marker: Option<String>, include_body: bool) -> Self {
        let marker = marker.or_else(|| marker_from_body(&comment.body));
        Self {
            id: comment.id,
            html_url: comment.html_url,
            marker,
            body: include_body.then_some(comment.body),
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiIssue {
    pub id: u64,
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub state: String,
    pub html_url: Option<String>,
}

impl From<ApiIssue> for IssueSummary {
    fn from(issue: ApiIssue) -> Self {
        Self {
            id: issue.id,
            number: issue.number,
            title: issue.title,
            body: issue.body,
            state: issue.state,
            html_url: issue.html_url,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiComment {
    pub id: u64,
    pub body: String,
    pub html_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiRepository {
    pub name: String,
    pub full_name: Option<String>,
    pub owner: Option<ApiRepositoryOwner>,
    #[serde(default)]
    pub private: bool,
    pub clone_url: Option<String>,
    pub ssh_url: Option<String>,
    pub html_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiRepositoryOwner {
    pub login: Option<String>,
    pub username: Option<String>,
    pub name: Option<String>,
}

impl ApiRepository {
    pub(crate) fn into_summary(self, fallback_owner: &str) -> RepoSummary {
        let owner = self
            .owner
            .and_then(|owner| owner.login.or(owner.username).or(owner.name))
            .unwrap_or_else(|| fallback_owner.to_owned());
        let full_name = self
            .full_name
            .unwrap_or_else(|| format!("{owner}/{}", self.name));
        RepoSummary {
            full_name,
            owner,
            name: self.name,
            private: self.private,
            clone_url: self.clone_url,
            ssh_url: self.ssh_url,
            html_url: self.html_url,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct NewIssue<'a> {
    pub title: &'a str,
    pub body: &'a str,
}

#[derive(Debug, Serialize)]
pub(crate) struct UpdateIssue<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<&'a str>,
}

#[derive(Debug, Serialize)]
pub(crate) struct NewComment<'a> {
    pub body: &'a str,
}

#[derive(Debug, Serialize)]
pub(crate) struct NewRepository<'a> {
    pub name: &'a str,
    pub private: bool,
    pub description: &'a str,
    pub auto_init: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiError {
    pub message: Option<String>,
}

fn marker_from_body(body: &str) -> Option<String> {
    let start = body.find("<!--")?;
    let end = body[start..].find("-->")? + start + 3;
    Some(body[start..end].to_owned())
}
