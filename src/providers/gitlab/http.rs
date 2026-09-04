//! HTTP plumbing for the GitLab REST v4 provider.
//!
//! The GitlabHttp struct owns a `reqwest::blocking::Client`, the
//! resolved `/api/v4` base URL, and the role-scoped PRIVATE-TOKEN
//! credential. It exposes typed helpers for the requests the
//! orchestrator actually issues (issue, note, label, and label
//! listing) and a paginated helper for the endpoints that return a
//! JSON array, matching the pagination rules used elsewhere in this
//! CLI:
//!   - per_page is a stable 50 so multi-page reads are predictable
//!   - page starts at 1
//!   - a repeated identical non-empty page aborts pagination
//!   - a hard cap of `MAX_PAGES` prevents runaway loops
//!
//! The token is held by value but redacted in every error message via
//! a single `redact` helper. The token is never logged, never
//! formatted via `Debug`, and never appears in the returned
//! `ForgejoError::Http` payload.

use crate::providers::api::ForgejoError;
use crate::providers::gitlab::model::ApiError;
use reqwest::StatusCode;
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap};
use serde::Serialize;
use serde::de::DeserializeOwned;
use url::Url;

/// Per-page request size for paginated list endpoints. GitLab caps
/// `per_page` at 100, and 50 keeps request bodies comfortably under
/// 4 KiB for issue / note / label listings while still amortising the
/// round trip cost.
pub(crate) const PAGE_SIZE: usize = 50;

/// Hard upper bound on the number of pages the paginated helper will
/// walk before bailing out. Mirrors the Forgejo / Redmine defaults
/// (each provider uses the same constant for the same reason: a
/// pathological server response that never advances pagination must
/// not turn into an infinite loop).
pub(crate) const MAX_PAGES: usize = 10_000;

/// HTTP client for the GitLab REST v4 provider.
pub(crate) struct GitlabHttp {
    pub(crate) client: Client,
    api_base: String,
    pub(crate) token: String,
}

impl std::fmt::Debug for GitlabHttp {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never leak the token through the `Debug` impl; the rest of
        // the struct is request plumbing that an operator does not
        // need to see in a panic message. The api_base is safe to
        // surface so a developer can still tell which GitLab host a
        // failing request targeted.
        formatter
            .debug_struct("GitlabHttp")
            .field("api_base", &self.api_base)
            .field("token", &"[redacted]")
            .finish()
    }
}

impl GitlabHttp {
    pub(crate) fn new(api_base: String, token: String) -> Result<Self, ForgejoError> {
        let api_base = api_base.trim_end_matches('/').to_owned();
        let token = token.trim().to_owned();
        if token.is_empty() {
            return Err(ForgejoError::auth("GitLab PRIVATE-TOKEN is empty"));
        }
        reqwest::header::HeaderValue::from_str(&token).map_err(|_| {
            // Never include the offending bytes in the error message:
            // the value is the credential we are about to reject, and
            // surfacing it would leak the token to whatever caller
            // (or test) renders the error.
            ForgejoError::auth("GitLab PRIVATE-TOKEN contains invalid header characters")
        })?;
        let client = crate::infra::http_client::build_client()
            .map_err(|error| ForgejoError::request("client build", error))?;
        Ok(Self {
            client,
            api_base,
            token,
        })
    }

    /// Issue a `GET` against the GitLab API and decode the JSON body.
    pub(crate) fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        operation: &str,
    ) -> Result<T, ForgejoError> {
        // Safe GET: retry on transient transport failures and
        // 429/502/503/504.
        let (status, text) = self.response_with_retry(
            self.client.get(self.endpoint(path)?).query(query),
            operation,
        )?;
        if !status.is_success() {
            return Err(self.http_error(status, &text, operation));
        }
        serde_json::from_str(&text).map_err(|error| ForgejoError::Decode {
            operation: operation.to_owned(),
            message: self.redact(&error.to_string()),
        })
    }

    /// `GET` helper that tolerates a 404 by returning `Ok(None)` so
    /// the label-existence probe can short-circuit cleanly without
    /// inventing a not-found error.
    #[allow(dead_code)]
    pub(crate) fn get_optional<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        operation: &str,
    ) -> Result<Option<T>, ForgejoError> {
        // Safe GET with optional 404: retry policy applies, 404 is terminal.
        let (status, text) = self.response_with_retry(
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

    /// Issue a `POST` against the GitLab API and decode the response.
    pub(crate) fn post<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
        operation: &str,
    ) -> Result<T, ForgejoError> {
        let (status, text) = self.response(
            self.client
                .post(self.endpoint(path)?)
                .header(CONTENT_TYPE, "application/json")
                .json(body),
            operation,
        )?;
        if !status.is_success() {
            return Err(self.http_error(status, &text, operation));
        }
        serde_json::from_str(&text).map_err(|error| ForgejoError::Decode {
            operation: operation.to_owned(),
            message: self.redact(&error.to_string()),
        })
    }

    /// `POST` helper that forwards the request parameters as URL
    /// query parameters instead of (or in addition to) a JSON body.
    /// The live `https://gitlab.example.com/19.2` instance
    /// rejects `POST /projects/:id/issues/:iid/links` when the
    /// payload is sent as a JSON object: it expects the target
    /// coordinates and the optional `link_type` to arrive as query
    /// parameters (`target_project_id`, `target_issue_iid`,
    /// `link_type`). Other endpoints can still route through here
    /// by passing a non-`None` body; when `body` is `None`, the
    /// helper sends the request with no body and skips the
    /// `Content-Type` header so the request line stays minimal.
    ///
    /// The PRIVATE-TOKEN is always carried by the `PRIVATE-TOKEN`
    /// header (set inside [`Self::response`]); this helper never
    /// puts credentials in the URL so a recorded request line
    /// cannot leak the token through query parameters.
    pub(crate) fn post_with_query<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: Option<&B>,
        query: &[(&str, String)],
        operation: &str,
    ) -> Result<T, ForgejoError> {
        let mut builder = self.client.post(self.endpoint(path)?).query(query);
        if let Some(body) = body {
            builder = builder.header(CONTENT_TYPE, "application/json").json(body);
        }
        let (status, text) = self.response(builder, operation)?;
        if !status.is_success() {
            return Err(self.http_error(status, &text, operation));
        }
        serde_json::from_str(&text).map_err(|error| ForgejoError::Decode {
            operation: operation.to_owned(),
            message: self.redact(&error.to_string()),
        })
    }

    /// Issue a `PUT` against the GitLab API and decode the response.
    /// GitLab uses `PUT` for issue updates and accepts both an empty
    /// body (success) and a fresh `ApiIssue` JSON, so we decode via
    /// `Option<T>` and accept either.
    pub(crate) fn put<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
        operation: &str,
    ) -> Result<Option<T>, ForgejoError> {
        let (status, text) = self.response(
            self.client
                .put(self.endpoint(path)?)
                .header(CONTENT_TYPE, "application/json")
                .json(body),
            operation,
        )?;
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

    /// Walk a paginated `GET` endpoint that returns a top-level JSON
    /// array. The provider supplies the path and the per-call query
    /// parameters; this helper handles the iteration, page-failure
    /// decoding, repeated-page guard, and `MAX_PAGES` cap.
    ///
    /// GitLab signals the last page by returning a full page with an
    /// empty `x-next-page` header. We rely on that signal as the
    /// primary termination criterion so a partial page (which is
    /// the common case for short result sets) still triggers an
    /// extra round trip when the server has more data. Falling back
    /// to `count < PAGE_SIZE` would skip legitimate continuation
    /// pages with one or a handful of items.
    pub(crate) fn paginate<T, F>(
        &self,
        operation: &str,
        mut fetch: F,
    ) -> Result<Vec<T>, ForgejoError>
    where
        F: FnMut(&Self, usize) -> Result<(Vec<T>, HeaderMap, String), ForgejoError>,
    {
        let mut items = Vec::new();
        let mut previous_signature: Option<String> = None;
        for page in (1_usize..).take(MAX_PAGES) {
            let (page_items, headers, signature) = fetch(self, page)?;
            if previous_signature.as_deref() == Some(signature.as_str()) && !page_items.is_empty() {
                return Err(ForgejoError::pagination(
                    operation,
                    "GitLab returned the same non-empty page repeatedly",
                ));
            }
            let count = page_items.len();
            items.extend(page_items);
            if count == 0 {
                return Ok(items);
            }
            // GitLab sends `x-next-page` as either an empty string
            // (last page) or the next page number; `x-total-pages`
            // is the total count when the server knows it. Honour
            // either, and fall back to a partial-page heuristic only
            // when both are absent.
            let next_page_header = headers
                .get("x-next-page")
                .and_then(|value| value.to_str().ok())
                .map(|value| value.trim().to_owned());
            let total_pages_header = headers
                .get("x-total-pages")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<usize>().ok());
            let complete = match (next_page_header.as_deref(), total_pages_header) {
                (Some(""), _) => true,
                (Some(_), Some(total)) => page >= total,
                (None, Some(total)) => page >= total,
                (None, None) => count < PAGE_SIZE,
                (Some(_), None) => false,
            };
            if complete {
                return Ok(items);
            }
            previous_signature = Some(signature);
        }
        Err(ForgejoError::pagination(
            operation,
            "pagination exceeded the safety limit",
        ))
    }

    /// Build the absolute URL for a path under `/api/v4`. Strips
    /// stray slashes so a caller can pass `projects/1/issues` or
    /// `/projects/1/issues/` interchangeably. The api_base path
    /// (`/api/v4` or `/gitlab/api/v4`) is preserved so the URL lands
    /// on the right GitLab endpoint even with deployment prefixes.
    pub(crate) fn endpoint(&self, path: &str) -> Result<Url, ForgejoError> {
        let mut url = Url::parse(&self.api_base).map_err(|error| {
            ForgejoError::config(format!("invalid GitLab API base URL: {error}"))
        })?;
        let base_path = url.path().trim_end_matches('/');
        let trimmed = path.trim_start_matches('/').trim_end_matches('/');
        let combined = match (base_path.is_empty(), trimmed.is_empty()) {
            (true, true) => "/".to_owned(),
            (true, false) => format!("/{trimmed}"),
            (false, true) => format!("{base_path}/"),
            (false, false) => format!("{base_path}/{trimmed}"),
        };
        url.set_path(&combined);
        url.set_query(None);
        url.set_fragment(None);
        Ok(url)
    }

    fn response(
        &self,
        request: RequestBuilder,
        operation: &str,
    ) -> Result<(StatusCode, String), ForgejoError> {
        // Mutation path: no retry.
        let response = request
            .header(ACCEPT, "application/json")
            .header("PRIVATE-TOKEN", self.token.as_str())
            .send()
            .map_err(|error| ForgejoError::request(operation, self.redact(&error.to_string())))?;
        let status = response.status();
        let text = response
            .text()
            .map_err(|error| ForgejoError::request(operation, self.redact(&error.to_string())))?;
        Ok((status, text))
    }

    fn response_with_retry(
        &self,
        request: RequestBuilder,
        operation: &str,
    ) -> Result<(StatusCode, String), ForgejoError> {
        // Safe GET path: retry on transient failures.
        let (status, _headers, text) = crate::infra::http_client::fetch_with_retry(
            request
                .header(ACCEPT, "application/json")
                .header("PRIVATE-TOKEN", self.token.as_str()),
            operation,
            |message| self.redact(message),
        )?;
        Ok((status, text))
    }

    pub(crate) fn http_error(
        &self,
        status: StatusCode,
        text: &str,
        operation: &str,
    ) -> ForgejoError {
        let message = serde_json::from_str::<ApiError>(text)
            .ok()
            .and_then(|error| error.message.or(error.error).or(error.error_description))
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| {
                if text.trim().is_empty() {
                    format!("GitLab {operation} failed with HTTP {}", status.as_u16())
                } else {
                    "GitLab returned an error".to_owned()
                }
            });
        ForgejoError::Http {
            operation: operation.to_owned(),
            status: status.as_u16(),
            message: self.redact(&message),
        }
    }

    /// Build a `GET` request that returns a JSON array plus the
    /// response headers (needed for pagination termination). The
    /// caller supplies the path, the per-call query, and an
    /// operation name for error messages; this helper handles
    /// authentication, response decoding, and JSON-array extraction.
    pub(crate) fn get_page<T: DeserializeOwned>(
        &self,
        path: &str,
        extra_query: &[(&str, String)],
        operation: &str,
    ) -> Result<(Vec<T>, HeaderMap, String), ForgejoError> {
        // Safe GET with pagination: retry on transient failures.
        let mut params: Vec<(&str, String)> = extra_query.to_vec();
        if !params.iter().any(|(key, _)| *key == "per_page") {
            params.push(("per_page", PAGE_SIZE.to_string()));
        }
        let (status, headers, text) = crate::infra::http_client::fetch_with_retry(
            self.client
                .get(self.endpoint(path)?)
                .query(&params)
                .header(ACCEPT, "application/json")
                .header("PRIVATE-TOKEN", self.token.as_str()),
            operation,
            |message| self.redact(message),
        )?;
        if !status.is_success() {
            return Err(self.http_error(status, &text, operation));
        }
        let items: Vec<T> = serde_json::from_str(&text).map_err(|error| ForgejoError::Decode {
            operation: operation.to_owned(),
            message: self.redact(&error.to_string()),
        })?;
        Ok((items, headers, text))
    }

    /// Redact the GitLab PRIVATE-TOKEN from any string. Returns the
    /// input unchanged when the token is empty (which never happens
    /// for a constructed [`GitlabHttp`] but is exercised by the test
    /// suite for direct redaction helpers).
    pub(crate) fn redact(&self, value: &str) -> String {
        if self.token.is_empty() {
            value.to_owned()
        } else {
            value.replace(&self.token, "[redacted]")
        }
    }

    /// Issue a `DELETE` against the GitLab API. Phase 4 introduces
    /// this helper for issue link deletes (`/links/:id`) and any
    /// future endpoint that follows the same shape. The method is
    /// tolerant of an empty body so a successful `204 No Content`
    /// surfaces as `Ok(None)` instead of a decode error.
    pub(crate) fn delete<T: DeserializeOwned>(
        &self,
        path: &str,
        operation: &str,
    ) -> Result<Option<T>, ForgejoError> {
        let (status, text) = self.response(self.client.delete(self.endpoint(path)?), operation)?;
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
}

#[cfg(test)]
mod tests {
    use super::{GitlabHttp, MAX_PAGES, PAGE_SIZE};

    fn http(token: &str) -> GitlabHttp {
        GitlabHttp::new("https://gitlab.example/api/v4".to_owned(), token.to_owned()).unwrap()
    }

    #[test]
    fn redact_replaces_every_occurrence_of_the_token() {
        let http = http("secret-token");
        assert_eq!(
            http.redact("token=secret-token&other=secret-token"),
            "token=[redacted]&other=[redacted]",
        );
    }

    #[test]
    fn redact_is_a_no_op_when_the_input_does_not_contain_the_token() {
        let http = http("secret-token");
        assert_eq!(http.redact("hello world"), "hello world");
    }

    #[test]
    fn constructor_rejects_empty_token() {
        let error = GitlabHttp::new("https://gitlab.example/api/v4".to_owned(), "   ".to_owned())
            .unwrap_err();
        assert!(error.to_string().contains("empty"));
    }

    #[test]
    fn constructor_rejects_token_with_invalid_header_characters() {
        let error = GitlabHttp::new(
            "https://gitlab.example/api/v4".to_owned(),
            "token\u{0000}bad".to_owned(),
        )
        .unwrap_err();
        let rendered = error.to_string();
        assert!(rendered.contains("invalid header"));
        assert!(!rendered.contains("token\u{0000}bad"));
    }

    #[test]
    fn debug_redacts_token() {
        let http = http("super-secret-token");
        let rendered = format!("{http:?}");
        assert!(rendered.contains("[redacted]"));
        assert!(!rendered.contains("super-secret-token"));
    }

    #[test]
    fn page_size_and_max_pages_are_stable_for_contract_tests() {
        assert_eq!(PAGE_SIZE, 50);
        assert_eq!(MAX_PAGES, 10_000);
    }
}
