use crate::forgejo_model::ForgejoError;
use crate::redmine_model::{
    RedmineCurrentUserResponse, RedmineErrorResponse, RedmineMembershipCollection,
    RedmineNewUserMembership, RedmineNewUserMembershipFields, RedmineRoleCollection,
    RedmineUpdateMembership, RedmineUpdateMembershipFields, RedmineUserMembershipOutcome,
};
use reqwest::StatusCode;
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use url::Url;

const MAX_ERROR_MESSAGE: usize = 512;
const PAGE_SIZE: usize = 100;
const MAX_PAGES: usize = 10_000;

pub(crate) struct RedmineHttp {
    client: Client,
    api_base: String,
    api_key: String,
}

impl RedmineHttp {
    pub(crate) fn new(api_base: String, api_key: String) -> Result<Self, ForgejoError> {
        let api_key = api_key.trim().to_owned();
        HeaderValue::from_str(&api_key).map_err(|_| {
            ForgejoError::auth("Redmine API key contains invalid header characters")
        })?;
        let client = Client::builder()
            .user_agent(format!("phasegent/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| ForgejoError::request("client build", error.to_string()))?;
        Ok(Self {
            client,
            api_base,
            api_key,
        })
    }

    pub(crate) fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        operation: &str,
    ) -> Result<T, ForgejoError> {
        self.send(
            self.client.get(self.endpoint(path)?).query(query),
            operation,
        )
    }

    pub(crate) fn get_optional<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        operation: &str,
    ) -> Result<Option<T>, ForgejoError> {
        let (status, text) = self.response(
            self.client.get(self.endpoint(path)?).query(query),
            operation,
        )?;
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(self.http_error(status, &text, operation));
        }
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|error| ForgejoError::Decode {
                operation: operation.to_owned(),
                message: self.redact(&error.to_string()),
            })
    }

    pub(crate) fn post<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
        operation: &str,
    ) -> Result<T, ForgejoError> {
        self.send(
            self.client
                .post(self.endpoint(path)?)
                .header(CONTENT_TYPE, "application/json")
                .json(body),
            operation,
        )
    }

    pub(crate) fn post_optional<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
        operation: &str,
    ) -> Result<Option<T>, ForgejoError> {
        self.send_optional(
            self.client
                .post(self.endpoint(path)?)
                .header(CONTENT_TYPE, "application/json")
                .json(body),
            operation,
        )
    }

    pub(crate) fn put<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
        operation: &str,
    ) -> Result<Option<T>, ForgejoError> {
        self.send_optional(
            self.client
                .put(self.endpoint(path)?)
                .header(CONTENT_TYPE, "application/json")
                .json(body),
            operation,
        )
    }

    pub(crate) fn delete<T: DeserializeOwned>(
        &self,
        path: &str,
        operation: &str,
    ) -> Result<Option<T>, ForgejoError> {
        self.send_optional(self.client.delete(self.endpoint(path)?), operation)
    }

    pub(crate) fn issue_url(&self, issue: u64) -> String {
        format!("{}/issues/{issue}", self.api_base.trim_end_matches('/'))
    }

    /// Identify the user bound to this HTTP client's API key via
    /// `/users/current.json`. Used to bind a role-scoped credential to a
    /// concrete Redmine user without logging in.
    pub(crate) fn current_user(
        &self,
    ) -> Result<crate::redmine_model::RedmineCurrentUser, ForgejoError> {
        let response: RedmineCurrentUserResponse =
            self.get("users/current.json", &[], "user current")?;
        Ok(response.user)
    }

    /// Ensure the given user holds the role named `role_name` on the project,
    /// adding the membership when missing and adding the role without
    /// dropping any unrelated roles that were already attached. Returns a
    /// `RedmineUserMembershipOutcome` whose `status` is `added`, `updated`,
    /// `existing`, or `warning`. Caller-provided role and user must be
    /// authoritative; the lookup that resolves a role name to its id is the
    /// only step that may surface a warning for an ambiguous or missing role.
    pub(crate) fn ensure_user_membership(
        &self,
        project_id: u64,
        user: &crate::redmine_model::RedmineCurrentUser,
        role_name: &str,
    ) -> Result<RedmineUserMembershipOutcome, ForgejoError> {
        let roles = self.list_roles()?;
        let roles = roles
            .into_iter()
            .filter(|role| role.name == role_name)
            .collect::<Vec<_>>();
        let role = match roles.as_slice() {
            [role] => role.clone(),
            [] => {
                return Ok(user_warning(user, role_name, 0, "role was not found"));
            }
            _ => {
                return Ok(user_warning(user, role_name, 0, "role is ambiguous"));
            }
        };
        let memberships = self.list_memberships(project_id)?;
        let existing = memberships.into_iter().find(|membership| {
            membership
                .user
                .as_ref()
                .is_some_and(|membership_user| membership_user.id == user.id)
        });
        let Some(existing) = existing else {
            let payload = RedmineNewUserMembership {
                membership: RedmineNewUserMembershipFields {
                    user_id: user.id,
                    role_ids: vec![role.id],
                },
            };
            let _: Option<Value> = self.post_optional(
                &format!("projects/{project_id}/memberships.json"),
                &payload,
                "user membership create",
            )?;
            return Ok(RedmineUserMembershipOutcome {
                user_id: user.id,
                user_login: user.login.clone(),
                role_id: role.id,
                role_name: role.name.clone(),
                status: "added".to_owned(),
                warning: None,
            });
        };
        if existing
            .roles
            .iter()
            .any(|candidate| candidate.id == role.id)
        {
            return Ok(RedmineUserMembershipOutcome {
                user_id: user.id,
                user_login: user.login.clone(),
                role_id: role.id,
                role_name: role.name.clone(),
                status: "existing".to_owned(),
                warning: None,
            });
        }
        let mut role_ids = existing
            .roles
            .into_iter()
            .map(|candidate| candidate.id)
            .collect::<Vec<_>>();
        role_ids.push(role.id);
        let payload = RedmineUpdateMembership {
            membership: RedmineUpdateMembershipFields { role_ids },
        };
        let _: Option<Value> = self.put(
            &format!("memberships/{}.json", existing.id),
            &payload,
            "user membership update",
        )?;
        Ok(RedmineUserMembershipOutcome {
            user_id: user.id,
            user_login: user.login.clone(),
            role_id: role.id,
            role_name: role.name.clone(),
            status: "updated".to_owned(),
            warning: None,
        })
    }

    fn list_roles(&self) -> Result<Vec<crate::redmine_model::RedmineRole>, ForgejoError> {
        self.paginate("role list", |http, offset| {
            let params = [
                ("limit", PAGE_SIZE.to_string()),
                ("offset", offset.to_string()),
            ];
            let page: RedmineRoleCollection = http.get("roles.json", &params, "role list")?;
            let signature = page
                .roles
                .iter()
                .map(|role| role.id.to_string())
                .collect::<Vec<_>>()
                .join(",");
            Ok((page.roles, page.total_count, page.limit, signature))
        })
    }

    fn list_memberships(
        &self,
        project_id: u64,
    ) -> Result<Vec<crate::redmine_model::RedmineMembership>, ForgejoError> {
        self.paginate("membership list", |http, offset| {
            let params = [
                ("limit", PAGE_SIZE.to_string()),
                ("offset", offset.to_string()),
            ];
            let page: RedmineMembershipCollection = http.get(
                &format!("projects/{project_id}/memberships.json"),
                &params,
                "membership list",
            )?;
            let signature = page
                .memberships
                .iter()
                .map(|membership| membership.id.to_string())
                .collect::<Vec<_>>()
                .join(",");
            Ok((page.memberships, page.total_count, page.limit, signature))
        })
    }

    pub(crate) fn paginate<T, F>(
        &self,
        operation: &str,
        mut fetch: F,
    ) -> Result<Vec<T>, ForgejoError>
    where
        F: FnMut(
            &Self,
            usize,
        ) -> Result<(Vec<T>, Option<usize>, Option<usize>, String), ForgejoError>,
    {
        let mut items = Vec::new();
        let mut offset = 0;
        let mut previous_signature = None;
        for _ in 0..MAX_PAGES {
            let (page_items, total_count, limit, signature) = fetch(self, offset)?;
            if previous_signature.as_deref() == Some(signature.as_str()) && !page_items.is_empty() {
                return Err(ForgejoError::pagination(
                    operation,
                    "Redmine returned the same non-empty page repeatedly",
                ));
            }
            let count = page_items.len();
            let response_limit = limit.unwrap_or(PAGE_SIZE).max(1);
            items.extend(page_items);
            let complete = count == 0
                || total_count.is_some_and(|total| offset.saturating_add(count) >= total)
                || (total_count.is_none() && count < response_limit);
            if complete {
                return Ok(items);
            }
            let next_offset = offset.saturating_add(count);
            if next_offset <= offset {
                return Err(ForgejoError::pagination(
                    operation,
                    "Redmine pagination offset did not advance",
                ));
            }
            offset = next_offset;
            previous_signature = Some(signature);
        }
        Err(ForgejoError::pagination(
            operation,
            "pagination exceeded the safety limit",
        ))
    }

    fn endpoint(&self, path: &str) -> Result<Url, ForgejoError> {
        let mut url = Url::parse(&self.api_base).map_err(|error| {
            ForgejoError::config(format!("invalid Redmine API base URL: {error}"))
        })?;
        let base_path = url.path().trim_end_matches('/');
        let endpoint = path.trim_start_matches('/');
        let full_path = if base_path.is_empty() {
            format!("/{endpoint}")
        } else {
            format!("{base_path}/{endpoint}")
        };
        url.set_path(&full_path);
        url.set_query(None);
        url.set_fragment(None);
        Ok(url)
    }

    fn send<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        operation: &str,
    ) -> Result<T, ForgejoError> {
        let (status, text) = self.response(request, operation)?;
        if !status.is_success() {
            return Err(self.http_error(status, &text, operation));
        }
        serde_json::from_str(&text).map_err(|error| ForgejoError::Decode {
            operation: operation.to_owned(),
            message: self.redact(&error.to_string()),
        })
    }

    fn send_optional<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        operation: &str,
    ) -> Result<Option<T>, ForgejoError> {
        let (status, text) = self.response(request, operation)?;
        if !status.is_success() {
            return Err(self.http_error(status, &text, operation));
        }
        if status == StatusCode::NO_CONTENT || text.trim().is_empty() {
            return Ok(None);
        }
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|error| ForgejoError::Decode {
                operation: operation.to_owned(),
                message: self.redact(&error.to_string()),
            })
    }

    fn response(
        &self,
        request: RequestBuilder,
        operation: &str,
    ) -> Result<(StatusCode, String), ForgejoError> {
        let response = request
            .header(ACCEPT, "application/json")
            .header("X-Redmine-API-Key", self.api_key.as_str())
            .send()
            .map_err(|error| ForgejoError::request(operation, self.redact(&error.to_string())))?;
        let status = response.status();
        let text = response
            .text()
            .map_err(|error| ForgejoError::request(operation, self.redact(&error.to_string())))?;
        Ok((status, text))
    }

    fn http_error(&self, status: StatusCode, text: &str, operation: &str) -> ForgejoError {
        let message = serde_json::from_str::<RedmineErrorResponse>(text)
            .ok()
            .map(|error| {
                error
                    .errors
                    .into_iter()
                    .map(error_value)
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| {
                if text.trim().is_empty() {
                    format!("Redmine {operation} failed with HTTP {}", status.as_u16())
                } else {
                    "Redmine returned an error".to_owned()
                }
            });
        ForgejoError::Http {
            operation: operation.to_owned(),
            status: status.as_u16(),
            message: cap(&self.redact(&message)),
        }
    }

    fn redact(&self, value: &str) -> String {
        if self.api_key.is_empty() {
            value.to_owned()
        } else {
            value.replace(&self.api_key, "[redacted]")
        }
    }
}

fn error_value(value: Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| {
            value
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| value.to_string())
}

fn cap(value: &str) -> String {
    value.chars().take(MAX_ERROR_MESSAGE).collect()
}

/// HTTP client for the companion `redmine_git_mirror` Redmine plugin.
///
/// The plugin runs at the Redmine base URL (no `/api/v1`) and authenticates
/// with an `Authorization: Bearer <key>` header instead of the role-scoped
/// `X-Redmine-API-Key` header. This client keeps that flow fully separate
/// from `RedmineHttp` so neither credential leaks into the other's
/// redaction paths or request signatures.
pub(crate) struct RedmineGitMirrorHttp {
    client: Client,
    base_url: String,
    bearer_key: String,
}

/// Outcome of a single plugin call that distinguishes `404 Not Found`
/// (treated as an optional missing entry) from other non-success responses
/// (propagated as actionable errors).
pub(crate) enum RedmineGitMirrorLookup<T> {
    Found(T),
    Missing,
}

impl RedmineGitMirrorHttp {
    pub(crate) fn new(base_url: String, bearer_key: String) -> Result<Self, ForgejoError> {
        let bearer_key = bearer_key.trim().to_owned();
        if bearer_key.is_empty() {
            return Err(ForgejoError::auth("Redmine git mirror plugin key is empty"));
        }
        let mut parsed = Url::parse(&base_url)
            .map_err(|error| ForgejoError::config(format!("invalid Redmine base URL: {error}")))?;
        if parsed.host_str().is_none() {
            return Err(ForgejoError::config(
                "Redmine git mirror plugin URL must include a host",
            ));
        }
        let trimmed_path = parsed.path().trim_end_matches('/').to_owned();
        let new_path = if trimmed_path.is_empty() {
            "/".to_owned()
        } else {
            trimmed_path
        };
        parsed.set_path(&new_path);
        parsed.set_query(None);
        parsed.set_fragment(None);
        let base_url = parsed.to_string().trim_end_matches('/').to_owned();
        let client = Client::builder()
            .user_agent(format!("phasegent/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| ForgejoError::request("mirror client build", error.to_string()))?;
        Ok(Self {
            client,
            base_url,
            bearer_key,
        })
    }

    /// Issue a `GET` against the plugin and decode the response. The
    /// companion `redmine_git_mirror` plugin returns `200 OK` when the
    /// mirror exists and `404 Not Found` when it does not; the helper
    /// surfaces the latter as `Missing` so callers can decide whether to
    /// POST.
    pub(crate) fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        operation: &str,
    ) -> Result<RedmineGitMirrorLookup<T>, ForgejoError> {
        let request = self
            .client
            .get(self.endpoint(path)?)
            .header(AUTHORIZATION, format!("Bearer {}", self.bearer_key));
        let (status, text) = self.response(request, operation)?;
        if status == StatusCode::NOT_FOUND {
            return Ok(RedmineGitMirrorLookup::Missing);
        }
        if !status.is_success() {
            return Err(self.http_error(status, &text, operation));
        }
        let decoded = serde_json::from_str(&text).map_err(|error| ForgejoError::Decode {
            operation: operation.to_owned(),
            message: self.redact(&error.to_string()),
        })?;
        Ok(RedmineGitMirrorLookup::Found(decoded))
    }

    /// Issue a `POST` against the plugin with a JSON body. Accepts both
    /// `200 OK` (synchronous completion) and `202 Accepted` (queued
    /// asynchronous job) so the bootstrap path treats a freshly queued
    /// mirror as a successful outcome.
    pub(crate) fn post<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
        operation: &str,
    ) -> Result<T, ForgejoError> {
        let request = self
            .client
            .post(self.endpoint(path)?)
            .header(AUTHORIZATION, format!("Bearer {}", self.bearer_key))
            .header(CONTENT_TYPE, "application/json")
            .json(body);
        let (status, text) = self.response(request, operation)?;
        if status != StatusCode::OK && status != StatusCode::ACCEPTED {
            return Err(self.http_error(status, &text, operation));
        }
        serde_json::from_str(&text).map_err(|error| ForgejoError::Decode {
            operation: operation.to_owned(),
            message: self.redact(&error.to_string()),
        })
    }

    fn endpoint(&self, path: &str) -> Result<Url, ForgejoError> {
        let mut url = Url::parse(&self.base_url)
            .map_err(|error| ForgejoError::config(format!("invalid Redmine base URL: {error}")))?;
        let base_path = url.path().trim_end_matches('/');
        let endpoint = path.trim_start_matches('/');
        let full_path = if base_path.is_empty() {
            format!("/{endpoint}")
        } else {
            format!("{base_path}/{endpoint}")
        };
        url.set_path(&full_path);
        url.set_query(None);
        url.set_fragment(None);
        Ok(url)
    }

    fn response(
        &self,
        request: RequestBuilder,
        operation: &str,
    ) -> Result<(StatusCode, String), ForgejoError> {
        let response = request
            .header(ACCEPT, "application/json")
            .send()
            .map_err(|error| ForgejoError::request(operation, self.redact(&error.to_string())))?;
        let status = response.status();
        let text = response
            .text()
            .map_err(|error| ForgejoError::request(operation, self.redact(&error.to_string())))?;
        Ok((status, text))
    }

    fn http_error(&self, status: StatusCode, text: &str, operation: &str) -> ForgejoError {
        let message = serde_json::from_str::<RedmineErrorResponse>(text)
            .ok()
            .map(|error| {
                error
                    .errors
                    .into_iter()
                    .map(error_value)
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| {
                if text.trim().is_empty() {
                    format!(
                        "Redmine git mirror {operation} failed with HTTP {}",
                        status.as_u16()
                    )
                } else {
                    "Redmine git mirror plugin returned an error".to_owned()
                }
            });
        ForgejoError::Http {
            operation: operation.to_owned(),
            status: status.as_u16(),
            message: cap(&self.redact(&message)),
        }
    }

    fn redact(&self, value: &str) -> String {
        if self.bearer_key.is_empty() {
            value.to_owned()
        } else {
            value.replace(&self.bearer_key, "[redacted]")
        }
    }
}

fn user_warning(
    user: &crate::redmine_model::RedmineCurrentUser,
    role_name: &str,
    role_id: u64,
    detail: &str,
) -> RedmineUserMembershipOutcome {
    let login = if user.login.is_empty() {
        format!("#{}", user.id)
    } else {
        user.login.clone()
    };
    let warning = format!("Redmine {detail}: user '{login}', role '{role_name}'");
    RedmineUserMembershipOutcome {
        user_id: user.id,
        user_login: login,
        role_id,
        role_name: role_name.to_owned(),
        status: "warning".to_owned(),
        warning: Some(warning),
    }
}
