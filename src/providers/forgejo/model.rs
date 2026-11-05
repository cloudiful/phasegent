use serde::Deserialize;

#[derive(Clone, Deserialize)]
pub(super) struct Repository {
    pub id: u64,
    pub name: String,
    pub full_name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub private: bool,
}

#[derive(Clone, Deserialize)]
pub(super) struct Issue {
    pub id: u64,
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    pub state: String,
    #[serde(default)]
    pub labels: Vec<Label>,
    #[serde(default)]
    pub html_url: Option<String>,
    #[serde(default)]
    pub pull_request: Option<serde_json::Value>,
    #[serde(default)]
    pub repository: Option<Repository>,
}

#[derive(Deserialize)]
pub(super) struct Comment {
    pub id: u64,
    pub body: String,
    pub issue_url: String,
    #[serde(default)]
    pub html_url: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct Dependency {
    pub state: String,
}

#[derive(Deserialize)]
pub(super) struct Milestone {
    pub id: u64,
    pub title: String,
    pub state: String,
    #[serde(default)]
    pub due_on: Option<String>,
}

#[derive(Clone, Deserialize)]
pub(super) struct Label {
    pub id: u64,
    pub name: String,
}
