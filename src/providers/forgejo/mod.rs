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
pub use config::ForgejoConfig;
pub(crate) use config::validate_repository;
use http::ForgejoHttp;
use model::Repository;
use reqwest::Method;
use reqwest::header::{HeaderMap, HeaderName, LINK};
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

    pub(crate) fn api_base(&self) -> &str {
        self.config.api_base()
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
        if let Some(key) = query
            .iter()
            .map(|(key, _)| *key)
            .find(|key| key.eq_ignore_ascii_case("page") || key.eq_ignore_ascii_case("limit"))
        {
            return Err(PhasegentError::pagination(
                operation,
                &format!("caller must not supply reserved pagination key '{key}'"),
            ));
        }

        let mut result: Vec<T> = Vec::new();
        let mut observed_pages: HashSet<String> = HashSet::new();
        let mut expected_total: Option<u64> = None;
        let mut expected_more = false;
        for page in 1..=1000 {
            let mut args = query.to_vec();
            args.push(("page", page.to_string()));
            args.push(("limit", "100".to_owned()));
            let request = self.http.request(Method::GET, segments)?.query(&args);
            let (mut items, headers, raw) = self.http.get_page::<Vec<T>>(request, operation)?;
            if items.len() > 100 {
                return Err(PhasegentError::pagination(
                    operation,
                    "server exceeded page limit",
                ));
            }
            if expected_more && items.is_empty() {
                return Err(PhasegentError::pagination(
                    operation,
                    &format!("page {page} is empty after an earlier page reported more"),
                ));
            }
            let metadata = page_metadata(&headers, operation)?;
            if let Some(total) = metadata.total {
                if expected_total.is_some_and(|expected| expected != total) {
                    return Err(PhasegentError::pagination(
                        operation,
                        &format!(
                            "server changed the reported total from {expected_total:?} to {total}"
                        ),
                    ));
                }
                expected_total = Some(total);
            }
            let established_total = expected_total;
            let accumulated = result.len() as u64 + items.len() as u64;
            if let Some(total) = established_total {
                if accumulated > total {
                    return Err(PhasegentError::pagination(
                        operation,
                        &format!("page {page} exceeds the reported total {total}"),
                    ));
                }
                if items.is_empty() && accumulated < total {
                    return Err(PhasegentError::pagination(
                        operation,
                        &format!("page {page} is empty before the reported total {total}"),
                    ));
                }
                if let Some(next) = metadata.linked_next {
                    let more = accumulated < total;
                    if next != more {
                        return Err(PhasegentError::pagination(
                            operation,
                            &format!(
                                "page {page} total {total} and Link next={next} disagree about completion"
                            ),
                        ));
                    }
                }
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
            result.append(&mut items);
            match established_total {
                Some(total) if accumulated == total => return Ok(result),
                Some(_) => {
                    if empty {
                        return Err(PhasegentError::pagination(
                            operation,
                            &format!("page {page} is empty before completion"),
                        ));
                    }
                }
                None => match metadata.linked_next {
                    Some(true) => {
                        if empty {
                            return Err(PhasegentError::pagination(
                                operation,
                                &format!("page {page} is empty while Link advertises more"),
                            ));
                        }
                    }
                    Some(false) => return Ok(result),
                    None => {
                        if empty {
                            return Ok(result);
                        }
                    }
                },
            }
            expected_more = match established_total {
                Some(total) => accumulated < total,
                None => metadata.linked_next == Some(true),
            };
        }
        Err(PhasegentError::pagination(
            operation,
            "Forgejo page limit exceeded",
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PageMetadata {
    total: Option<u64>,
    linked_next: Option<bool>,
}

const TOTAL_COUNT: HeaderName = HeaderName::from_static("x-total-count");

fn page_metadata(headers: &HeaderMap, operation: &str) -> Result<PageMetadata, PhasegentError> {
    let mut total = None;
    for value in headers.get_all(TOTAL_COUNT) {
        let text = value
            .to_str()
            .map_err(|_| PhasegentError::pagination(operation, "invalid X-Total-Count header"))?;
        let parsed = text.trim().parse::<u64>().map_err(|_| {
            PhasegentError::pagination(operation, &format!("invalid X-Total-Count value '{text}'"))
        })?;
        if total.is_some_and(|existing| existing != parsed) {
            return Err(PhasegentError::pagination(
                operation,
                "conflicting X-Total-Count headers",
            ));
        }
        total = Some(parsed);
    }
    let linked_next = parse_link_next(headers, operation)?;
    Ok(PageMetadata { total, linked_next })
}

fn parse_link_next(headers: &HeaderMap, operation: &str) -> Result<Option<bool>, PhasegentError> {
    let values: Vec<_> = headers.get_all(LINK).iter().collect();
    if values.is_empty() {
        return Ok(None);
    }
    let mut next = false;
    let mut saw_relation = false;
    for value in values {
        let text = value
            .to_str()
            .map_err(|_| PhasegentError::pagination(operation, "invalid Link header"))?;
        for entry in link_entries(text) {
            let (target, parameters) = entry.split_once(';').ok_or_else(|| {
                PhasegentError::pagination(operation, "malformed Link header entry")
            })?;
            let target = target.trim();
            if !target.starts_with('<') || !target.ends_with('>') {
                return Err(PhasegentError::pagination(
                    operation,
                    "malformed Link header target",
                ));
            }
            let uri = target[1..target.len() - 1].trim();
            if uri.is_empty()
                || uri.chars().any(|character| {
                    character.is_whitespace() || character == '<' || character == '>'
                })
                || url::Url::parse(uri).is_err()
            {
                return Err(PhasegentError::pagination(
                    operation,
                    "invalid Link header target URI",
                ));
            }
            let mut relation_value = None;
            for parameter in parameters.split(';') {
                let parameter = parameter.trim();
                if parameter.is_empty() {
                    continue;
                }
                let (name, value) = parameter.split_once('=').ok_or_else(|| {
                    PhasegentError::pagination(operation, "malformed Link header parameter")
                })?;
                let name = name.trim();
                if name.is_empty() || !is_link_token(name) {
                    return Err(PhasegentError::pagination(
                        operation,
                        "invalid Link header parameter name",
                    ));
                }
                let value = link_parameter_value(value, operation)?;
                if name.eq_ignore_ascii_case("rel") {
                    if relation_value.is_some() {
                        return Err(PhasegentError::pagination(
                            operation,
                            "duplicate Link header rel parameter",
                        ));
                    }
                    relation_value = Some(value);
                }
            }
            let Some(relation_value) = relation_value else {
                return Err(PhasegentError::pagination(
                    operation,
                    "Link header entry has no relation",
                ));
            };
            let mut found = false;
            for relation in relation_value.split_ascii_whitespace() {
                found = true;
                if relation.eq_ignore_ascii_case("next") {
                    next = true;
                } else if !matches!(
                    relation.to_ascii_lowercase().as_str(),
                    "last" | "prev" | "first"
                ) {
                    return Err(PhasegentError::pagination(
                        operation,
                        &format!("unsupported Link relation '{relation}'"),
                    ));
                }
            }
            if !found {
                return Err(PhasegentError::pagination(operation, "empty Link relation"));
            }
            saw_relation = true;
        }
    }
    if !saw_relation {
        return Err(PhasegentError::pagination(
            operation,
            "Link header has no usable relation",
        ));
    }
    Ok(Some(next))
}

/// A non-rel Link parameter value is either a token or one balanced quoted
/// string. No trim_matches shortcut: an unmatched or embedded quote fails.
fn link_parameter_value<'a>(value: &'a str, operation: &str) -> Result<&'a str, PhasegentError> {
    let value = value.trim();
    if value.starts_with('"') {
        if value.len() < 2 || !value.ends_with('"') {
            return Err(PhasegentError::pagination(
                operation,
                "unbalanced quoted Link header parameter",
            ));
        }
        let inner = &value[1..value.len() - 1];
        if inner.contains('"') || inner.contains('\\') {
            return Err(PhasegentError::pagination(
                operation,
                "invalid quoted Link header parameter",
            ));
        }
        Ok(inner)
    } else {
        if value.is_empty() || !is_link_token(value) {
            return Err(PhasegentError::pagination(
                operation,
                "invalid Link header parameter value",
            ));
        }
        Ok(value)
    }
}

fn is_link_token(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(
                    character,
                    '!' | '#'
                        | '$'
                        | '%'
                        | '&'
                        | '\''
                        | '*'
                        | '+'
                        | '-'
                        | '.'
                        | '^'
                        | '_'
                        | '`'
                        | '|'
                        | '~'
                )
        })
}

fn link_entries(text: &str) -> Vec<&str> {
    let mut entries = Vec::new();
    let mut start = 0;
    let mut inside_target = false;
    let mut inside_quotes = false;
    for (index, character) in text.char_indices() {
        match character {
            '<' if !inside_quotes => inside_target = true,
            '>' if !inside_quotes => inside_target = false,
            '"' => inside_quotes = !inside_quotes,
            ',' if !inside_target && !inside_quotes => {
                entries.push(&text[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    entries.push(&text[start..]);
    entries
        .into_iter()
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .collect()
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
