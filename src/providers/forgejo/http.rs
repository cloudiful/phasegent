use crate::providers::api::ForgejoError;
use crate::providers::forgejo::model::ApiError;
use reqwest::StatusCode;
use reqwest::blocking::Response;
use reqwest::header::{HeaderMap, LINK};
use serde::de::DeserializeOwned;

pub(crate) struct Page<T> {
    pub(crate) items: Vec<T>,
    pub(crate) total: Option<usize>,
    pub(crate) next: Option<bool>,
    pub(crate) signature: String,
}

impl<T> Page<T> {
    pub(crate) fn is_complete(&self, collected: usize, total: Option<usize>) -> bool {
        total.is_some_and(|total| collected >= total) || self.next == Some(false)
    }
}

pub(crate) fn decode<T: DeserializeOwned>(
    response: Response,
    operation: &str,
) -> Result<T, ForgejoError> {
    let (status, _, text) = response_parts(response, operation)?;
    if !status.is_success() {
        return Err(http_error(status, &text, operation));
    }
    serde_json::from_str(&text).map_err(|error| ForgejoError::Decode {
        operation: operation.to_owned(),
        message: error.to_string(),
    })
}

#[allow(dead_code)]
pub(crate) fn decode_page<T: DeserializeOwned>(
    response: Response,
    operation: &str,
) -> Result<Page<T>, ForgejoError> {
    let (status, headers, text) = response_parts(response, operation)?;
    if !status.is_success() {
        return Err(http_error(status, &text, operation));
    }
    let items = serde_json::from_str(&text).map_err(|error| ForgejoError::Decode {
        operation: operation.to_owned(),
        message: error.to_string(),
    })?;
    let total = headers
        .get("x-total-count")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok());
    let next = headers
        .get(LINK)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(',')
                .any(|link| link.contains("rel=\"next\"") || link.contains("rel=next"))
        });
    Ok(Page {
        items,
        total,
        next,
        signature: text,
    })
}

#[allow(dead_code)]
pub(crate) fn decode_text(response: Response, operation: &str) -> Result<String, ForgejoError> {
    let (status, _, text) = response_parts(response, operation)?;
    if !status.is_success() {
        return Err(http_error(status, &text, operation));
    }
    Ok(text)
}

pub(crate) fn decode_from_parts<T: DeserializeOwned>(
    status: StatusCode,
    text: &str,
    operation: &str,
) -> Result<T, ForgejoError> {
    if !status.is_success() {
        return Err(http_error(status, text, operation));
    }
    serde_json::from_str(text).map_err(|error| ForgejoError::Decode {
        operation: operation.to_owned(),
        message: error.to_string(),
    })
}

pub(crate) fn decode_page_from_parts<T: DeserializeOwned>(
    status: StatusCode,
    headers: &HeaderMap,
    text: String,
    operation: &str,
) -> Result<Page<T>, ForgejoError> {
    if !status.is_success() {
        return Err(http_error(status, &text, operation));
    }
    let items = serde_json::from_str(&text).map_err(|error| ForgejoError::Decode {
        operation: operation.to_owned(),
        message: error.to_string(),
    })?;
    let total = headers
        .get("x-total-count")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok());
    let next = headers
        .get(LINK)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(',')
                .any(|link| link.contains("rel=\"next\"") || link.contains("rel=next"))
        });
    Ok(Page {
        items,
        total,
        next,
        signature: text,
    })
}

pub(crate) fn decode_text_from_parts(
    status: StatusCode,
    text: String,
    operation: &str,
) -> Result<String, ForgejoError> {
    if !status.is_success() {
        return Err(http_error(status, &text, operation));
    }
    Ok(text)
}

fn response_parts(
    response: Response,
    operation: &str,
) -> Result<(StatusCode, HeaderMap, String), ForgejoError> {
    let status = response.status();
    let headers = response.headers().clone();
    let text = response
        .text()
        .map_err(|error| ForgejoError::request(operation, error.to_string()))?;
    Ok((status, headers, text))
}

pub(crate) fn http_error(status: StatusCode, text: &str, operation: &str) -> ForgejoError {
    let message = serde_json::from_str::<ApiError>(text)
        .ok()
        .and_then(|error| error.message)
        .unwrap_or_else(|| "Forgejo returned an error".to_owned());
    ForgejoError::Http {
        operation: operation.to_owned(),
        status: status.as_u16(),
        message: message.chars().take(512).collect(),
    }
}
