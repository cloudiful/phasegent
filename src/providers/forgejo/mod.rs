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

use crate::infra::storage::Storage;
use crate::policy::Role;
use crate::providers::api::PhasegentError;
pub use config::ForgejoConfig;
pub(crate) use config::validate_repository;
use http::ForgejoHttp;
use model::Repository;
use reqwest::Method;
use serde::de::DeserializeOwned;

pub struct ForgejoProvider {
    config: ForgejoConfig,
    http: ForgejoHttp,
    repository: RefCell<Option<Repository>>,
}

impl ForgejoProvider {
    pub(crate) fn repository_scope(&self) -> &str {
        self.config.repository()
    }

    pub fn new(config: ForgejoConfig, token: &str) -> Result<Self, PhasegentError> {
        let http = ForgejoHttp::new(&config, token)?;
        Ok(Self {
            config,
            http,
            repository: RefCell::new(None),
        })
    }

    pub fn for_role(config: ForgejoConfig, role: Role) -> Result<Self, PhasegentError> {
        let storage = Storage::open().map_err(PhasegentError::config)?;
        Self::with_storage(config, role, &storage)
    }

    fn with_storage(
        config: ForgejoConfig,
        role: Role,
        storage: &Storage,
    ) -> Result<Self, PhasegentError> {
        let token = storage
            .load_credential(role, "forgejo")
            .map_err(|_| PhasegentError::auth("could not load Forgejo role credential"))?
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| PhasegentError::auth("Forgejo role credential is not configured"))?;
        Self::new(config, &token)
    }

    fn repository(&self) -> Result<Repository, PhasegentError> {
        if let Some(repo) = self.repository.borrow().as_ref() {
            return Ok(repo.clone());
        }
        let repo: Repository = self
            .http
            .get(self.http.request(Method::GET, &[])?, "repository get")?;
        self.validate_repository(&repo)?;
        *self.repository.borrow_mut() = Some(repo.clone());
        Ok(repo)
    }

    fn validate_repository(&self, repo: &Repository) -> Result<(), PhasegentError> {
        if repo.id == 0
            || repo.full_name != self.config.repository
            || repo.name != self.config.repo()
        {
            return Err(PhasegentError::config(
                "Forgejo response repository does not match configured scope",
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
        let mut result = Vec::new();
        for page in 1..=1000 {
            let mut args = query.to_vec();
            args.push(("page", page.to_string()));
            args.push(("limit", "100".to_owned()));
            let request = self.http.request(Method::GET, segments)?.query(&args);
            let mut items: Vec<T> = self.http.get(request, operation)?;
            if items.len() > 100 {
                return Err(PhasegentError::pagination(
                    operation,
                    "server exceeded page limit",
                ));
            }
            let done = items.is_empty();
            result.append(&mut items);
            if done {
                return Ok(result);
            }
        }
        Err(PhasegentError::pagination(
            operation,
            "Forgejo page limit exceeded",
        ))
    }
}
