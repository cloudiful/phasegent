use crate::infra::http_client::{CONNECT_TIMEOUT, REQUEST_TIMEOUT, fetch_with_retry};
use crate::providers::api::PhasegentError;
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde::de::DeserializeOwned;
use url::Url;

use super::config::ForgejoConfig;

pub struct ForgejoHttp {
    client: Client,
    base: Url,
    authorization: HeaderValue,
}

impl ForgejoHttp {
    pub fn new(config: &ForgejoConfig, token: &str) -> Result<Self, PhasegentError> {
        if token.trim().is_empty() {
            return Err(PhasegentError::auth("Forgejo credential is missing"));
        }
        let mut authorization = HeaderValue::from_str(&format!("token {token}"))
            .map_err(|_| PhasegentError::auth("invalid Forgejo credential"))?;
        authorization.set_sensitive(true);
        let client = Client::builder()
            .user_agent(format!("phasegent/{}", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| PhasegentError::config("could not construct Forgejo HTTP client"))?;
        let mut base = Url::parse(&config.api_base)
            .map_err(|_| PhasegentError::config("invalid Forgejo API base"))?;
        base.path_segments_mut()
            .map_err(|_| PhasegentError::config("invalid Forgejo API base"))?
            .pop_if_empty()
            .extend(["api", "v1", "repos", config.owner(), config.repo()]);
        Ok(Self {
            client,
            base,
            authorization,
        })
    }

    pub fn request(
        &self,
        method: reqwest::Method,
        segments: &[&str],
    ) -> Result<RequestBuilder, PhasegentError> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|_| PhasegentError::config("invalid Forgejo API base"))?
            .extend(segments.iter().copied());
        Ok(self
            .client
            .request(method, url)
            .header(AUTHORIZATION, self.authorization.clone()))
    }

    pub fn get<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        operation: &str,
    ) -> Result<T, PhasegentError> {
        self.get_page(request, operation).map(|(value, _, _)| value)
    }

    /// Read one GET response together with its headers and raw body, reusing
    /// the existing retry, timeout and redaction path. Pagination needs the
    /// response metadata without a second request.
    pub fn get_page<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        operation: &str,
    ) -> Result<(T, HeaderMap, String), PhasegentError> {
        let (status, headers, text) =
            fetch_with_retry(request, operation, |_| "Forgejo request failed".to_owned())?;
        Ok((Self::decode(status, &text, operation)?, headers, text))
    }

    pub fn write<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        operation: &str,
    ) -> Result<T, PhasegentError> {
        let response = request
            .send()
            .map_err(|_| PhasegentError::request(operation, "Forgejo request failed".to_owned()))?;
        let status = response.status();
        let text = response.text().map_err(|_| {
            PhasegentError::request(operation, "Forgejo response read failed".to_owned())
        })?;
        Self::decode(status, &text, operation)
    }

    fn decode<T: DeserializeOwned>(
        status: reqwest::StatusCode,
        text: &str,
        operation: &str,
    ) -> Result<T, PhasegentError> {
        if !status.is_success() {
            return Err(PhasegentError::Http {
                operation: operation.to_owned(),
                status: status.as_u16(),
                message: "Forgejo request returned an error".to_owned(),
            });
        }
        serde_json::from_str(text).map_err(|_| PhasegentError::Decode {
            operation: operation.to_owned(),
            message: "invalid Forgejo response".to_owned(),
        })
    }
}
