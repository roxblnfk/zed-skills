//! HTTP client abstraction.
//!
//! Remote providers depend on this minimal trait so no test ever touches the
//! real network: unit tests inject [`MockHttp`], integration tests point the
//! real [`ReqwestClient`] at a `wiremock` server.

use async_trait::async_trait;
use thiserror::Error;

/// Transport-level failure (connect error, invalid URL, ...). HTTP error
/// statuses are NOT errors — they come back as a [`HttpResponse`].
#[derive(Debug, Error)]
#[error("{0}")]
pub struct HttpError(pub String);

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn body_str(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }
}

/// Minimal async HTTP GET. Implementations follow redirects.
#[async_trait]
pub trait HttpClient: Send + Sync {
    async fn get(&self, url: &str, headers: &[(String, String)])
    -> Result<HttpResponse, HttpError>;
}

/// Production client over `reqwest` (rustls, redirects followed — GitHub's
/// zipball endpoint 302-redirects to codeload).
pub struct ReqwestClient {
    inner: reqwest::Client,
}

impl ReqwestClient {
    pub fn new() -> Result<Self, HttpError> {
        let inner = reqwest::Client::builder()
            .build()
            .map_err(|e| HttpError(format!("failed to build HTTP client: {e}")))?;
        Ok(ReqwestClient { inner })
    }
}

#[async_trait]
impl HttpClient for ReqwestClient {
    async fn get(
        &self,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<HttpResponse, HttpError> {
        let mut request = self.inner.get(url);
        for (name, value) in headers {
            request = request.header(name, value);
        }
        let response = request
            .send()
            .await
            .map_err(|e| HttpError(format!("GET {url}: {e}")))?;
        let status = response.status().as_u16();
        let body = response
            .bytes()
            .await
            .map_err(|e| HttpError(format!("GET {url}: failed to read body: {e}")))?
            .to_vec();
        Ok(HttpResponse { status, body })
    }
}

/// One recorded request: `(url, headers)`.
pub type RecordedRequest = (String, Vec<(String, String)>);

/// Scripted client for unit tests: exact-URL routing plus a log of every
/// request (URL + headers) for assertions.
pub struct MockHttp {
    routes: std::sync::Mutex<Vec<(String, HttpResponse)>>,
    requests: std::sync::Mutex<Vec<RecordedRequest>>,
}

impl MockHttp {
    pub fn new() -> Self {
        MockHttp {
            routes: std::sync::Mutex::new(Vec::new()),
            requests: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn route(self, url: impl Into<String>, status: u16, body: impl Into<Vec<u8>>) -> Self {
        self.routes.lock().expect("mock lock").push((
            url.into(),
            HttpResponse {
                status,
                body: body.into(),
            },
        ));
        self
    }

    /// Every request made so far: `(url, headers)`.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().expect("mock lock").clone()
    }

    pub fn request_count(&self) -> usize {
        self.requests.lock().expect("mock lock").len()
    }
}

impl Default for MockHttp {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HttpClient for MockHttp {
    async fn get(
        &self,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<HttpResponse, HttpError> {
        self.requests
            .lock()
            .expect("mock lock")
            .push((url.to_string(), headers.to_vec()));
        let routes = self.routes.lock().expect("mock lock");
        match routes.iter().find(|(u, _)| u == url) {
            Some((_, response)) => Ok(response.clone()),
            None => Ok(HttpResponse {
                status: 404,
                body: format!("mock: no route for {url}").into_bytes(),
            }),
        }
    }
}
