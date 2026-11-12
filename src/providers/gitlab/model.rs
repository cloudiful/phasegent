use serde::Deserialize;

#[derive(Clone, Deserialize)]
pub(crate) struct Project {
    pub id: u64,
    pub name: String,
    pub path_with_namespace: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
}

#[derive(Clone, Deserialize)]
pub(super) struct Issue {
    pub id: u64,
    pub iid: u64,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    pub state: String,
    #[serde(default)]
    pub web_url: Option<String>,
}

#[derive(Clone, Deserialize)]
pub(super) struct Note {
    pub id: u64,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub noteable_type: Option<String>,
    #[serde(default)]
    pub noteable_iid: Option<u64>,
    #[serde(default)]
    pub web_url: Option<String>,
}

#[derive(Clone, Deserialize)]
pub(super) struct Milestone {
    pub id: u64,
    pub title: String,
    pub state: String,
    #[serde(default)]
    pub due_date: Option<String>,
}
