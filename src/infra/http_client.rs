//! Shared HTTP client policy for all provider transports.
//!
//! Centralizes connect/request timeouts, user-agent, gzip negotiation, and
//! safe-read retry policy so Forgejo, Redmine REST, Redmine git-mirror, and
//! GitLab share identical wire behaviour.

use std::time::Duration;

use reqwest::StatusCode;
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{HeaderMap, RETRY_AFTER};

use crate::providers::api::ForgejoError;

/// Default connect timeout: fail fast when the peer is unreachable.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Total request timeout: covers connect, send, and response body reads.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

const MAX_ATTEMPTS: usize = 3;
const BASE_BACKOFF_MS: u64 = 100;
const MAX_BACKOFF_MS: u64 = 2000;
const MAX_RETRY_AFTER_SECS: u64 = 2;

/// Build the shared blocking client with the production timeouts and
/// phasegent user agent. Gzip decompression is enabled via the `gzip`
/// Cargo feature and handled transparently by reqwest.
pub fn build_client() -> Result<Client, String> {
    build_client_with_timeouts(CONNECT_TIMEOUT, REQUEST_TIMEOUT)
}

/// Test-only helper that lets stall/timeout tests use shorter deadlines
/// without exposing a CLI flag. Production code must use `build_client`.
pub fn build_client_with_timeouts(connect: Duration, request: Duration) -> Result<Client, String> {
    Client::builder()
        .user_agent(format!("phasegent/{}", env!("CARGO_PKG_VERSION")))
        .connect_timeout(connect)
        .timeout(request)
        .build()
        .map_err(|error| error.to_string())
}

/// Whether the status code is a transient retry candidate.
pub fn is_retryable_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 502 | 503 | 504)
}

/// Whether the transport error is a transient timeout/connect failure.
/// Only these errors are retried; decode or protocol errors are not.
pub fn is_retryable_error(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect()
}

/// Bounded exponential backoff, optionally honoring a numeric `Retry-After`
/// capped to 2 seconds.
pub fn retry_delay(attempt: usize, retry_after: Option<Duration>) -> Duration {
    if let Some(delay) = retry_after {
        return delay.min(Duration::from_millis(MAX_BACKOFF_MS));
    }
    let exponential = BASE_BACKOFF_MS.saturating_mul(1_u64 << attempt);
    Duration::from_millis(exponential.min(MAX_BACKOFF_MS))
}

fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|secs| Duration::from_secs(secs.min(MAX_RETRY_AFTER_SECS)))
}

/// Execute a safe GET read with bounded retries. Handles transient transport
/// timeouts/connect failures and retryable HTTP 429/502/503/504, with capped
/// exponential backoff and capped numeric `Retry-After`. Uses `try_clone` to
/// ensure replayability; if the request cannot be cloned, returns the first
/// result without retry. Covers both `send` and `text` stages so a timeout
/// during body reading is handled consistently.
pub fn fetch_with_retry(
    request: RequestBuilder,
    operation: &str,
    redact: impl Fn(&str) -> String,
) -> Result<(StatusCode, HeaderMap, String), ForgejoError> {
    // If the builder cannot be cloned it is not replayable (e.g., streaming
    // body). Fail fast with a single attempt and no retry.
    if request.try_clone().is_none() {
        let response = request
            .send()
            .map_err(|error| ForgejoError::request(operation, redact(&error.to_string())))?;
        let status = response.status();
        let headers = response.headers().clone();
        let text = response
            .text()
            .map_err(|error| ForgejoError::request(operation, redact(&error.to_string())))?;
        return Ok((status, headers, text));
    }

    for attempt in 0..MAX_ATTEMPTS {
        let builder = request
            .try_clone()
            .expect("cloneable request must remain cloneable");
        match builder.send() {
            Err(error) if is_retryable_error(&error) && attempt + 1 < MAX_ATTEMPTS => {
                std::thread::sleep(retry_delay(attempt, None));
                continue;
            }
            Err(error) => {
                return Err(ForgejoError::request(operation, redact(&error.to_string())));
            }
            Ok(response) => {
                let status = response.status();
                let headers = response.headers().clone();
                if is_retryable_status(status) && attempt + 1 < MAX_ATTEMPTS {
                    let retry_after = parse_retry_after(&headers);
                    std::thread::sleep(retry_delay(attempt, retry_after));
                    continue;
                }
                // For non-retryable status or the final attempt, read the body.
                // A timeout during body reading for a safe GET is also retryable.
                match response.text() {
                    Err(error) if is_retryable_error(&error) && attempt + 1 < MAX_ATTEMPTS => {
                        std::thread::sleep(retry_delay(attempt, None));
                        continue;
                    }
                    Err(error) => {
                        return Err(ForgejoError::request(operation, redact(&error.to_string())));
                    }
                    Ok(text) => return Ok((status, headers, text)),
                }
            }
        }
    }
    unreachable!("retry loop must return within MAX_ATTEMPTS");
}
