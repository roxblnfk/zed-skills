//! Offline provider wiring for the analysis pipeline.
//!
//! The dry analysis must never touch the network: materialization already
//! runs cache-only (`Cache::offline`), and on top of that the providers get
//! an HTTP client that fails every request — a belt-and-braces guarantee
//! that no code path can slip a download in.

use std::sync::Arc;

use async_trait::async_trait;

use skills_core::traits::VendorProvider;
use skills_providers::http::{HttpClient, HttpError, HttpResponse};
use skills_providers::{
    ComposerProvider, DirProvider, GithubProvider, GitlabProvider, NpmProvider, UrlProvider,
};

/// HTTP client that refuses every request.
pub struct OfflineHttp;

#[async_trait]
impl HttpClient for OfflineHttp {
    async fn get(
        &self,
        url: &str,
        _headers: &[(String, String)],
    ) -> Result<HttpResponse, HttpError> {
        Err(HttpError(format!(
            "GET {url}: network access is disabled during analysis"
        )))
    }
}

/// The full provider set backed by the offline HTTP client (no env tokens —
/// nothing will be requested anyway).
pub fn offline_providers() -> Vec<Arc<dyn VendorProvider>> {
    let http: Arc<dyn HttpClient> = Arc::new(OfflineHttp);
    vec![
        Arc::new(DirProvider),
        Arc::new(ComposerProvider),
        Arc::new(NpmProvider),
        Arc::new(GithubProvider::new(Arc::clone(&http), None)),
        Arc::new(GitlabProvider::new(Arc::clone(&http), None)),
        Arc::new(UrlProvider::new(http)),
    ]
}
