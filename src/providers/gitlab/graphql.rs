//! GitLab GraphQL `/api/graphql` transport (issue 641 P3).
//!
//! Posts JSON `{query, variables}` to the GraphQL endpoint derived from the
//! normalized `/api/v4` base (deployment prefix preserved via
//! [`GitlabHttp::graphql_endpoint`]). The `PRIVATE-TOKEN` header carries the
//! credential so it never appears in the URL. HTTP failures surface via
//! `http_error`; GraphQL `errors[]` (including HTTP 200) surface as a
//! structured `request` error. Every message passes through `redact` so the
//! token never leaks.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::providers::api::ProviderError;
use crate::providers::gitlab::http::GitlabHttp;

#[derive(Debug, Serialize)]
struct GraphqlRequest<'a> {
    query: &'a str,
    variables: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct GraphqlEnvelope {
    data: Option<serde_json::Value>,
    errors: Option<Vec<GraphqlErrorItem>>,
}

#[derive(Debug, Deserialize)]
struct GraphqlErrorItem {
    #[serde(default)]
    message: String,
}

/// POST one GraphQL operation and return the decoded `data` payload.
pub(crate) fn execute<T: DeserializeOwned>(
    http: &GitlabHttp,
    query: &str,
    variables: serde_json::Value,
    operation: &str,
) -> Result<T, ProviderError> {
    use reqwest::header::{ACCEPT, CONTENT_TYPE};

    let url = http.graphql_endpoint()?;
    let body = GraphqlRequest { query, variables };
    let response = http
        .client
        .post(url)
        .header(ACCEPT, "application/json")
        .header(CONTENT_TYPE, "application/json")
        .header("PRIVATE-TOKEN", http.token.as_str())
        .json(&body)
        .send()
        .map_err(|error| ProviderError::request(operation, http.redact(&error.to_string())))?;
    let status = response.status();
    let text = response
        .text()
        .map_err(|error| ProviderError::request(operation, http.redact(&error.to_string())))?;
    if !status.is_success() {
        return Err(http.http_error(status, &text, operation));
    }
    let envelope: GraphqlEnvelope =
        serde_json::from_str(&text).map_err(|error| ProviderError::Decode {
            operation: operation.to_owned(),
            message: http.redact(&error.to_string()),
        })?;
    if let Some(errors) = envelope.errors {
        let messages: Vec<String> = errors
            .into_iter()
            .map(|item| item.message.trim().to_owned())
            .filter(|message| !message.is_empty())
            .collect();
        if !messages.is_empty() {
            return Err(ProviderError::Request {
                operation: operation.to_owned(),
                message: http.redact(&messages.join("; ")),
            });
        }
    }
    let data = envelope.data.ok_or_else(|| ProviderError::Decode {
        operation: operation.to_owned(),
        message: http.redact("GitLab GraphQL response contained no data"),
    })?;
    serde_json::from_value(data).map_err(|error| ProviderError::Decode {
        operation: operation.to_owned(),
        message: http.redact(&error.to_string()),
    })
}
