mod capabilities;
mod comments;
mod config;
mod http;
mod issues;
mod metadata;
mod model;
mod status;

#[cfg(test)]
mod contract_tests;

use std::cell::RefCell;
use std::collections::HashSet;

use crate::infra::storage::Storage;
use crate::policy::Role;
use crate::providers::api::PhasegentError;
pub use config::GitlabConfig;
use http::GitlabHttp;
use model::Project;
use reqwest::Method;
use reqwest::header::{HeaderMap, HeaderName};
use serde::de::DeserializeOwned;

const NEXT_PAGE: HeaderName = HeaderName::from_static("x-next-page");
const PER_PAGE: usize = 100;
const MAX_PAGES: u64 = 1000;

pub struct GitlabProvider {
    config: GitlabConfig,
    http: GitlabHttp,
    project: RefCell<Option<Project>>,
}

impl GitlabProvider {
    pub(crate) fn repository_scope(&self) -> &str {
        self.config.repository()
    }

    pub(crate) fn api_base(&self) -> &str {
        self.config.api_base()
    }

    pub fn new(config: GitlabConfig, token: &str) -> Result<Self, PhasegentError> {
        let http = GitlabHttp::new(&config, token)?;
        Ok(Self {
            config,
            http,
            project: RefCell::new(None),
        })
    }

    pub fn for_role(config: GitlabConfig, role: Role) -> Result<Self, PhasegentError> {
        let storage = Storage::open().map_err(PhasegentError::config)?;
        Self::with_storage(config, role, &storage)
    }

    fn with_storage(
        config: GitlabConfig,
        role: Role,
        storage: &Storage,
    ) -> Result<Self, PhasegentError> {
        let token = storage
            .load_credential(role, "gitlab")
            .map_err(|_| PhasegentError::auth("could not load GitLab role credential"))?
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| PhasegentError::auth("GitLab role credential is not configured"))?;
        Self::new(config, &token)
    }

    pub(super) fn project(&self) -> Result<Project, PhasegentError> {
        if let Some(project) = self.project.borrow().as_ref() {
            return Ok(project.clone());
        }
        let project: Project = self.http.get(
            self.http
                .request(Method::GET, &["projects", self.config.repository()])?,
            "project get",
        )?;
        self.validate_project(&project)?;
        *self.project.borrow_mut() = Some(project.clone());
        Ok(project)
    }

    pub(super) fn validate_project(&self, project: &Project) -> Result<(), PhasegentError> {
        if project.id == 0
            || project.path_with_namespace != self.config.repository
            || project.name.trim().is_empty()
        {
            return Err(PhasegentError::config(
                "GitLab response project does not match configured scope",
            ));
        }
        Ok(())
    }

    fn collect<T: DeserializeOwned>(
        &self,
        segments: &[&str],
        query: &[(&str, String)],
        operation: &str,
    ) -> Result<Vec<T>, PhasegentError> {
        if let Some(key) = query
            .iter()
            .map(|(key, _)| *key)
            .find(|key| key.eq_ignore_ascii_case("page") || key.eq_ignore_ascii_case("per_page"))
        {
            return Err(PhasegentError::pagination(
                operation,
                &format!("caller must not supply reserved pagination key '{key}'"),
            ));
        }

        let mut result: Vec<T> = Vec::new();
        let mut observed_pages: HashSet<String> = HashSet::new();
        let mut page = 1_u64;
        loop {
            if page > MAX_PAGES {
                return Err(PhasegentError::pagination(
                    operation,
                    "GitLab page limit exceeded",
                ));
            }
            let mut args = query.to_vec();
            args.push(("page", page.to_string()));
            args.push(("per_page", PER_PAGE.to_string()));
            let request = self.http.request(Method::GET, segments)?.query(&args);
            let (mut items, headers, raw) = self.http.get_page::<Vec<T>>(request, operation)?;
            if items.len() > PER_PAGE {
                return Err(PhasegentError::pagination(
                    operation,
                    "server exceeded page limit",
                ));
            }
            let empty = items.is_empty();
            if !empty {
                let fingerprint = page_fingerprint(&raw, operation)?;
                if !observed_pages.insert(fingerprint) {
                    return Err(PhasegentError::pagination(
                        operation,
                        &format!("page {page} repeats an earlier page without progress"),
                    ));
                }
            }
            let next = next_page(&headers, page, items.len(), operation)?;
            result.append(&mut items);
            match next {
                Some(next) => {
                    if empty {
                        return Err(PhasegentError::pagination(
                            operation,
                            &format!("page {page} is empty while a next page was advertised"),
                        ));
                    }
                    if next <= page {
                        return Err(PhasegentError::pagination(
                            operation,
                            &format!("next page {next} does not move forward from {page}"),
                        ));
                    }
                    page = next;
                }
                None => return Ok(result),
            }
        }
    }
}

/// The next page number advertised by GitLab. An empty header is the terminal
/// marker. A missing header is terminal only for a short page; a full page
/// without pagination metadata cannot be proven complete and fails instead of
/// returning a partial result.
fn next_page(
    headers: &HeaderMap,
    current: u64,
    item_count: usize,
    operation: &str,
) -> Result<Option<u64>, PhasegentError> {
    let mut observed: Option<Option<u64>> = None;
    for value in headers.get_all(NEXT_PAGE) {
        let text = value
            .to_str()
            .map_err(|_| PhasegentError::pagination(operation, "invalid X-Next-Page header"))?;
        let text = text.trim();
        let parsed = if text.is_empty() {
            None
        } else {
            let next = text.parse::<u64>().map_err(|_| {
                PhasegentError::pagination(
                    operation,
                    "GitLab returned a non-numeric X-Next-Page value",
                )
            })?;
            if next == 0 || next > MAX_PAGES {
                return Err(PhasegentError::pagination(
                    operation,
                    "GitLab returned an out-of-range X-Next-Page value",
                ));
            }
            if next != current + 1 {
                return Err(PhasegentError::pagination(
                    operation,
                    "X-Next-Page did not advance exactly one page from the current page; \
                     completion cannot be proven",
                ));
            }
            Some(next)
        };
        if observed.is_some_and(|existing| existing != parsed) {
            return Err(PhasegentError::pagination(
                operation,
                "conflicting X-Next-Page headers",
            ));
        }
        observed = Some(parsed);
    }
    match observed {
        Some(next) => Ok(next),
        None if item_count < PER_PAGE => Ok(None),
        None => Err(PhasegentError::pagination(
            operation,
            &format!(
                "page {current} returned a full page without an X-Next-Page header; completion \
                 cannot be proven"
            ),
        )),
    }
}

fn page_fingerprint(raw: &str, operation: &str) -> Result<String, PhasegentError> {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(value) => Ok(value.to_string()),
        Err(_) => Err(PhasegentError::pagination(
            operation,
            "page could not be normalized for progress checking",
        )),
    }
}
