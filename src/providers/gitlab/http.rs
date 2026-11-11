use crate::infra::http_client::{CONNECT_TIMEOUT, REQUEST_TIMEOUT, fetch_with_retry};
use crate::providers::api::PhasegentError;
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::de::DeserializeOwned;
use url::Url;

use super::config::GitlabConfig;

pub(super) const PRIVATE_TOKEN: &str = "PRIVATE-TOKEN";

pub struct GitlabHttp {
    client: Client,
    base: Url,
    token: HeaderValue,
}

impl GitlabHttp {
    pub fn new(config: &GitlabConfig, token: &str) -> Result<Self, PhasegentError> {
        if token.trim().is_empty() {
            return Err(PhasegentError::auth("GitLab credential is missing"));
        }
        let mut token = HeaderValue::from_str(token)
            .map_err(|_| PhasegentError::auth("invalid GitLab credential"))?;
        token.set_sensitive(true);
        let client = Client::builder()
            .user_agent(format!("phasegent/{}", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| PhasegentError::config("could not construct GitLab HTTP client"))?;
        let mut base = Url::parse(&config.api_base)
            .map_err(|_| PhasegentError::config("invalid GitLab API base"))?;
        base.path_segments_mut()
            .map_err(|_| PhasegentError::config("invalid GitLab API base"))?
            .pop_if_empty()
            .extend(["api", "v4"]);
        Ok(Self {
            client,
            base,
            token,
        })
    }

    pub fn request(
        &self,
        method: reqwest::Method,
        segments: &[&str],
    ) -> Result<RequestBuilder, PhasegentError> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|_| PhasegentError::config("invalid GitLab API base"))?
            .extend(segments.iter().copied());
        Ok(self
            .client
            .request(method, url)
            .header(PRIVATE_TOKEN, self.token.clone()))
    }

    pub fn get<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        operation: &str,
    ) -> Result<T, PhasegentError> {
        self.get_page(request, operation).map(|(value, _, _)| value)
    }

    pub fn get_page<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        operation: &str,
    ) -> Result<(T, HeaderMap, String), PhasegentError> {
        let (status, headers, text) =
            fetch_with_retry(request, operation, |_| "GitLab request failed".to_owned())?;
        Ok((Self::decode(status, &text, operation)?, headers, text))
    }

    pub fn write<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        operation: &str,
    ) -> Result<T, PhasegentError> {
        let response = request
            .send()
            .map_err(|_| PhasegentError::request(operation, "GitLab request failed".to_owned()))?;
        let status = response.status();
        let text = response.text().map_err(|_| {
            PhasegentError::request(operation, "GitLab response read failed".to_owned())
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
                message: "GitLab request returned an error".to_owned(),
            });
        }
        serde_json::from_str(text).map_err(|_| PhasegentError::Decode {
            operation: operation.to_owned(),
            message: "invalid GitLab response".to_owned(),
        })
    }
}
