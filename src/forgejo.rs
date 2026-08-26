use crate::auth;
use crate::forgejo_http::{Page, decode, decode_page};
use crate::forgejo_model::{
    ApiComment, ApiIssue, ApiRepository, NewComment, NewIssue, NewRepository, UpdateIssue,
};
pub use crate::forgejo_model::{CommentOutput, ForgejoError, IssueSummary, RepoSummary};
use crate::policy::Role;
use crate::provider::ProviderKind;
use crate::remote;
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::ACCEPT;
use serde::{Serialize, de::DeserializeOwned};

const PAGE_SIZE: usize = 50;
const MAX_PAGES: usize = 10_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForgejoConfig {
    pub base_url: String,
    pub owner: String,
    pub repository: String,
}

impl ForgejoConfig {
    pub fn new(
        base_url: impl Into<String>,
        owner: impl Into<String>,
        repository: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            owner: owner.into(),
            repository: repository.into(),
        }
    }

    pub const fn provider(&self) -> ProviderKind {
        ProviderKind::Forgejo
    }

    pub fn resolve(
        role: Role,
        api_base: Option<&str>,
        repository: Option<&str>,
    ) -> Result<Self, ForgejoError> {
        let stored = auth::load_config(role).map_err(ForgejoError::config)?;
        let explicit_base = api_base
            .map(str::to_owned)
            .or_else(|| std::env::var("PHASEGENT_API_BASE").ok());
        let explicit_repository = repository
            .map(str::to_owned)
            .or_else(|| std::env::var("PHASEGENT_REPOSITORY").ok());
        let stored_base = stored.as_ref().and_then(|config| config.api_base.clone());
        let stored_repository = stored.as_ref().and_then(|config| config.repository.clone());
        let needs_remote = explicit_base.is_none() && stored_base.is_none()
            || explicit_repository.is_none() && stored_repository.is_none();
        let remote = needs_remote.then(remote::resolve_origin);
        let remote = remote.transpose().map_err(ForgejoError::config)?;

        let base = explicit_base
            .or(stored_base)
            .or_else(|| remote.as_ref().map(|value| value.api_base.clone()))
            .ok_or_else(|| {
                ForgejoError::config("API base is not configured; use --api-base or auth setup")
            })?;
        let repository = explicit_repository
            .or(stored_repository)
            .or_else(|| remote.as_ref().map(|value| value.repository.clone()))
            .ok_or_else(|| {
                ForgejoError::config("repository is not configured; use --repository or auth setup")
            })?;
        let base = remote::normalize_api_base(&base).map_err(ForgejoError::config)?;
        let repository = remote::validate_repository(&repository).map_err(ForgejoError::config)?;
        let (owner, repository) = repository.split_once('/').expect("validated repository");
        Ok(Self::new(base, owner, repository))
    }
}

pub struct ForgejoProvider {
    pub(crate) config: ForgejoConfig,
    pub(crate) client: Client,
    pub(crate) token: String,
}

impl ForgejoProvider {
    pub fn for_role(role: Role, config: ForgejoConfig) -> Result<Self, ForgejoError> {
        let token = auth::token(role).map_err(ForgejoError::auth)?;
        Self::new(config, token)
    }

    pub fn new(config: ForgejoConfig, token: String) -> Result<Self, ForgejoError> {
        let client = Client::builder()
            .user_agent(format!("phasegent/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| ForgejoError::request("client build", error.to_string()))?;
        Ok(Self {
            config,
            client,
            token,
        })
    }

    pub fn get_issue(&self, number: u64) -> Result<IssueSummary, ForgejoError> {
        let issue: ApiIssue = self.get(&self.issue_path(number), &[], "issue get")?;
        Ok(issue.into())
    }

    pub fn search_issues(
        &self,
        query: Option<&str>,
        state: &str,
    ) -> Result<Vec<IssueSummary>, ForgejoError> {
        let mut issues = Vec::new();
        let mut page = 1;
        let mut previous_signature = None;
        let mut total_count = None;
        loop {
            if page > MAX_PAGES {
                return Err(ForgejoError::pagination(
                    "issue search",
                    "pagination exceeded the safety limit",
                ));
            }
            // `type=issues` keeps Forgejo's `/repos/{owner}/{repo}/issues` search
            // endpoint strictly on issues. Without it, the same endpoint may also
            // surface pull requests, which a tracking-orchestrator could mistake
            // for the dedicated tracking issue.
            let mut query_params = vec![
                ("state", state.to_owned()),
                ("type", "issues".to_owned()),
                ("limit", PAGE_SIZE.to_string()),
                ("page", page.to_string()),
            ];
            if let Some(query) = query {
                query_params.push(("q", query.to_owned()));
            }
            let response: Page<ApiIssue> =
                self.get_page(&self.issues_path(), &query_params, "issue search")?;
            if previous_signature.as_deref() == Some(response.signature.as_str())
                && !response.items.is_empty()
            {
                return Err(ForgejoError::pagination(
                    "issue search",
                    "Forgejo returned the same non-empty page repeatedly",
                ));
            }
            let count = response.items.len();
            total_count = response.total.or(total_count);
            let complete = response.is_complete(issues.len() + count, total_count);
            let signature = response.signature;
            issues.extend(response.items.into_iter().map(Into::into));
            if complete || count == 0 {
                return Ok(issues);
            }
            previous_signature = Some(signature);
            page += 1;
        }
    }

    pub fn create_issue(&self, title: &str, body: &str) -> Result<IssueSummary, ForgejoError> {
        let issue: ApiIssue = self.post(
            &self.issues_path(),
            &NewIssue { title, body },
            "issue create",
        )?;
        Ok(issue.into())
    }

    pub fn create_repo(
        &self,
        target: &str,
        private: bool,
        description: &str,
        auto_init: bool,
    ) -> Result<RepoSummary, ForgejoError> {
        if !private {
            return Err(ForgejoError::config(
                "repo create requires a private repository",
            ));
        }
        let target =
            remote::validate_repository_create_target(target).map_err(ForgejoError::config)?;
        let (owner, name) = target.split_once('/').expect("validated repository target");
        let path = if owner == self.config.owner {
            format!("{}/user/repos", self.config.base_url)
        } else {
            format!("{}/orgs/{}/repos", self.config.base_url, encode(owner))
        };
        let repository: ApiRepository = self.post(
            &path,
            &NewRepository {
                name,
                private,
                description,
                auto_init,
            },
            "repo create",
        )?;
        Ok(repository.into_summary(owner))
    }

    pub fn update_body(&self, number: u64, body: &str) -> Result<IssueSummary, ForgejoError> {
        let issue: ApiIssue = self.patch(
            &self.issue_path(number),
            &UpdateIssue {
                body: Some(body),
                state: None,
            },
            "issue update-body",
        )?;
        Ok(issue.into())
    }

    pub fn close_issue(&self, number: u64) -> Result<IssueSummary, ForgejoError> {
        let issue: ApiIssue = self.patch(
            &self.issue_path(number),
            &UpdateIssue {
                body: None,
                state: Some("closed"),
            },
            "issue close",
        )?;
        Ok(issue.into())
    }

    pub fn create_comment(
        &self,
        issue: u64,
        body: &str,
        marker: &str,
    ) -> Result<CommentOutput, ForgejoError> {
        let comment: ApiComment = self.post(
            &self.comments_path(issue),
            &NewComment { body },
            "comment create",
        )?;
        Ok(CommentOutput::from_api(
            comment,
            Some(marker.to_owned()),
            false,
        ))
    }

    pub fn get_comment(&self, issue: u64, comment: u64) -> Result<CommentOutput, ForgejoError> {
        self.list_comments(issue)?
            .into_iter()
            .find(|candidate| candidate.id == comment)
            .map(|candidate| CommentOutput::from_api(candidate, None, true))
            .ok_or_else(|| {
                ForgejoError::not_found(
                    "comment get",
                    "comment was not found in the specified issue",
                )
            })
    }

    pub fn find_marker(&self, issue: u64, marker: &str) -> Result<CommentOutput, ForgejoError> {
        if marker.is_empty() {
            return Err(ForgejoError::config("marker cannot be empty"));
        }
        let comments = self.list_comments(issue)?;
        comments
            .into_iter()
            .find(|comment| comment.body.contains(marker))
            .map(|comment| CommentOutput::from_api(comment, Some(marker.to_owned()), false))
            .ok_or_else(|| ForgejoError::not_found("comment find-marker", "marker was not found"))
    }

    fn list_comments(&self, issue: u64) -> Result<Vec<ApiComment>, ForgejoError> {
        let mut comments = Vec::new();
        let mut page = 1;
        let mut previous_signature = None;
        let mut total_count = None;
        loop {
            if page > MAX_PAGES {
                return Err(ForgejoError::pagination(
                    "comment list",
                    "pagination exceeded the safety limit",
                ));
            }
            let query = [("limit", PAGE_SIZE.to_string()), ("page", page.to_string())];
            let response: Page<ApiComment> =
                self.get_page(&self.comments_path(issue), &query, "comment list")?;
            if previous_signature.as_deref() == Some(response.signature.as_str())
                && !response.items.is_empty()
            {
                return Err(ForgejoError::pagination(
                    "comment list",
                    "Forgejo returned the same non-empty page repeatedly",
                ));
            }
            let count = response.items.len();
            total_count = response.total.or(total_count);
            let complete = response.is_complete(comments.len() + count, total_count);
            let signature = response.signature;
            comments.extend(response.items);
            if complete || count == 0 {
                return Ok(comments);
            }
            previous_signature = Some(signature);
            page += 1;
        }
    }

    fn issue_path(&self, number: u64) -> String {
        format!("{}/issues/{number}", self.repository_path())
    }

    fn issues_path(&self) -> String {
        format!("{}/issues", self.repository_path())
    }

    fn comments_path(&self, issue: u64) -> String {
        format!("{}/issues/{issue}/comments", self.repository_path())
    }

    pub(crate) fn repository_path(&self) -> String {
        format!(
            "{}/repos/{}/{}",
            self.config.base_url,
            encode(&self.config.owner),
            encode(&self.config.repository)
        )
    }

    pub(crate) fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        operation: &str,
    ) -> Result<T, ForgejoError> {
        self.send(self.client.get(path).query(query), operation)
    }

    fn get_page<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        operation: &str,
    ) -> Result<Page<T>, ForgejoError> {
        self.send_page(self.client.get(path).query(query), operation)
    }

    fn post<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
        operation: &str,
    ) -> Result<T, ForgejoError> {
        self.send(self.client.post(path).json(body), operation)
    }

    fn patch<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
        operation: &str,
    ) -> Result<T, ForgejoError> {
        self.send(self.client.patch(path).json(body), operation)
    }

    fn send<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        operation: &str,
    ) -> Result<T, ForgejoError> {
        let response = request
            .header(ACCEPT, "application/json")
            .bearer_auth(&self.token)
            .send()
            .map_err(|error| ForgejoError::request(operation, error.to_string()))?;
        decode(response, operation)
    }

    fn send_page<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        operation: &str,
    ) -> Result<Page<T>, ForgejoError> {
        let response = request
            .header(ACCEPT, "application/json")
            .bearer_auth(&self.token)
            .send()
            .map_err(|error| ForgejoError::request(operation, error.to_string()))?;
        decode_page(response, operation)
    }
}

pub(crate) fn encode(value: &str) -> String {
    value.bytes().fold(String::new(), |mut output, byte| {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            output.push(byte as char);
        } else {
            output.push_str(&format!("%{byte:02X}"));
        }
        output
    })
}
