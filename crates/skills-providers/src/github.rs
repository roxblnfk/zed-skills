//! GitHub provider: `remote[]` entries with `from: "github"`.
//!
//! - `package` must be exactly two non-empty segments (`owner/repo`).
//! - API base: `https://api.github.com` for the public host, `<host>/api/v3`
//!   for GitHub Enterprise.
//! - Endpoints: `/repos/{owner}/{repo}/tags?per_page=100`,
//!   `/repos/{owner}/{repo}` (default branch),
//!   `/repos/{owner}/{repo}/zipball/{ref}` (302 → codeload, followed by the
//!   HTTP client).
//! - Auth: `GITHUB_TOKEN` → `Authorization: Bearer <token>`.

use std::sync::Arc;

use async_trait::async_trait;

use skills_core::domain::{
    MaterializedVendor, Origin, ProviderId, SkillsFilter, VendorName, VendorRef,
};
use skills_core::error::{DiscoverError, MaterializeError};
use skills_core::pipeline::ctx::Ctx;
use skills_core::traits::{Cache, Vendor, VendorProvider};

use crate::http::HttpClient;
use crate::remote::{self, HostApi, PackageSpec};

pub const GITHUB_DEFAULT_HOST: &str = "github.com";
const GITHUB_DEFAULT_API: &str = "https://api.github.com";
const TAGS_PER_PAGE: u32 = 100;

/// API base URL for a declared host (`None` = public github.com).
pub fn api_base(host: Option<&str>) -> String {
    match host {
        None => GITHUB_DEFAULT_API.to_string(),
        Some(host) => {
            let normalized = remote::normalize_host(host);
            if normalized == "https://github.com" {
                GITHUB_DEFAULT_API.to_string()
            } else {
                format!("{normalized}/api/v3")
            }
        }
    }
}

/// `owner/repo`: exactly two non-empty segments.
pub fn validate_package(package: &str) -> Result<(), String> {
    let segments: Vec<&str> = package.split('/').collect();
    if segments.len() != 2 || segments.iter().any(|s| s.is_empty()) {
        return Err(format!(
            "github package must be exactly 'owner/repo', got '{package}'"
        ));
    }
    Ok(())
}

pub struct GithubProvider {
    http: Arc<dyn HttpClient>,
    token: Option<String>,
}

impl GithubProvider {
    pub fn new(http: Arc<dyn HttpClient>, token: Option<String>) -> Self {
        GithubProvider {
            http,
            token: remote::usable_token(token),
        }
    }

    /// Production wiring: token from `GITHUB_TOKEN`.
    pub fn from_env(http: Arc<dyn HttpClient>) -> Self {
        Self::new(http, std::env::var("GITHUB_TOKEN").ok())
    }
}

#[async_trait]
impl VendorProvider for GithubProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Github
    }

    async fn discover(&self, ctx: &Ctx) -> Result<Vec<VendorRef>, DiscoverError> {
        let mut refs = Vec::new();
        for entry in ctx.manifest.remote.iter().flatten() {
            if entry.from != "github" {
                continue;
            }
            let package = entry.package.clone().unwrap_or_default();
            validate_package(&package).map_err(|message| DiscoverError::Provider {
                provider: self.id(),
                message,
            })?;
            let vendor = GithubVendor::new(
                Arc::clone(&self.http),
                package,
                entry.host.clone(),
                entry.r#ref.clone(),
                self.token.clone(),
            );
            refs.push(VendorRef {
                provider: self.id(),
                name: vendor.name().clone(),
                origin: vendor.origin().clone(),
                filter: SkillsFilter::from_manifest(entry.skills.clone()),
                vendor: Arc::new(vendor),
            });
        }
        Ok(refs)
    }
}

pub struct GithubVendor {
    name: VendorName,
    origin: Origin,
    spec: PackageSpec,
    api: GithubApi,
}

impl GithubVendor {
    /// `package` must be pre-validated (`owner/repo`).
    pub fn new(
        http: Arc<dyn HttpClient>,
        package: String,
        host: Option<String>,
        r#ref: Option<String>,
        token: Option<String>,
    ) -> Self {
        let name = VendorName::new(&package);
        let origin = Origin::Remote {
            host: host
                .clone()
                .unwrap_or_else(|| GITHUB_DEFAULT_HOST.to_string()),
            package: package.clone(),
            r#ref: r#ref.clone(),
        };
        let api = GithubApi {
            http,
            api_base: api_base(host.as_deref()),
            package: package.clone(),
            token: remote::usable_token(token),
        };
        let spec = PackageSpec {
            name: name.clone(),
            origin: origin.clone(),
            from: "github",
            host,
            package,
            ref_declared: r#ref,
        };
        GithubVendor {
            name,
            origin,
            spec,
            api,
        }
    }

    /// Resolve the declared ref to a concrete tag / branch / SHA without
    /// materializing (used by `skills add`).
    pub async fn resolve_ref(&self) -> Result<String, String> {
        remote::resolve_ref(
            &self.api,
            &self.spec.package,
            self.spec.ref_declared.as_deref(),
        )
        .await
    }
}

#[async_trait]
impl Vendor for GithubVendor {
    fn name(&self) -> &VendorName {
        &self.name
    }

    fn origin(&self) -> &Origin {
        &self.origin
    }

    async fn materialize(&self, cache: &Cache) -> Result<MaterializedVendor, MaterializeError> {
        remote::materialize_package(&self.spec, &self.api, cache).await
    }
}

struct GithubApi {
    http: Arc<dyn HttpClient>,
    api_base: String,
    package: String,
    token: Option<String>,
}

impl GithubApi {
    fn headers(&self, accept: &str) -> Vec<(String, String)> {
        let mut headers = vec![
            ("Accept".to_string(), accept.to_string()),
            ("User-Agent".to_string(), "ai-skills".to_string()),
        ];
        if let Some(token) = &self.token {
            headers.push(("Authorization".to_string(), format!("Bearer {token}")));
        }
        headers
    }

    async fn get_ok(&self, url: &str, accept: &str) -> Result<Vec<u8>, String> {
        let response = remote::get(self.http.as_ref(), url, self.headers(accept)).await?;
        if !response.is_success() {
            return Err(format!("{url} returned HTTP {}", response.status));
        }
        Ok(response.body)
    }
}

#[async_trait]
impl HostApi for GithubApi {
    async fn list_tags(&self) -> Result<Vec<String>, String> {
        let url = format!(
            "{}/repos/{}/tags?per_page={TAGS_PER_PAGE}",
            self.api_base, self.package
        );
        let body = self.get_ok(&url, "application/vnd.github+json").await?;
        remote::parse_tag_names(&url, &body)
    }

    async fn default_branch(&self) -> Result<String, String> {
        let url = format!("{}/repos/{}", self.api_base, self.package);
        let body = self.get_ok(&url, "application/vnd.github+json").await?;
        remote::parse_default_branch(&url, &body)
    }

    async fn download_archive(&self, r#ref: &str) -> Result<Vec<u8>, String> {
        let url = format!(
            "{}/repos/{}/zipball/{}",
            self.api_base,
            self.package,
            remote::percent_encode(r#ref)
        );
        // `*/*` rather than the JSON media type: the zipball endpoint
        // rejects mismatched Accept headers with 415 on some deployments.
        self.get_ok(&url, "*/*").await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::MockHttp;
    use skills_core::manifest::MANIFEST_NAME;
    use skills_core::pipeline::ctx::{PrepareOptions, prepare};

    fn ctx(manifest: &str) -> (tempfile::TempDir, Ctx) {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(MANIFEST_NAME), manifest).unwrap();
        let ctx = prepare(tmp.path(), PrepareOptions::default()).unwrap();
        (tmp, ctx)
    }

    #[test]
    fn api_base_mapping() {
        assert_eq!(api_base(None), "https://api.github.com");
        assert_eq!(api_base(Some("github.com")), "https://api.github.com");
        assert_eq!(
            api_base(Some("https://github.com")),
            "https://api.github.com"
        );
        assert_eq!(
            api_base(Some("ghe.example.com")),
            "https://ghe.example.com/api/v3"
        );
        assert_eq!(
            api_base(Some("https://ghe.example.com/")),
            "https://ghe.example.com/api/v3"
        );
        assert_eq!(
            api_base(Some("http://127.0.0.1:8080")),
            "http://127.0.0.1:8080/api/v3"
        );
    }

    #[test]
    fn package_shape_validation() {
        validate_package("owner/repo").unwrap();
        for bad in ["owner", "owner/", "/repo", "a/b/c", "", "/"] {
            assert!(validate_package(bad).is_err(), "expected error: {bad:?}");
        }
    }

    #[tokio::test]
    async fn discovers_github_entries_only() {
        let (_tmp, ctx) = ctx(r#"{ "remote": [
                { "from": "github", "package": "acme/skills", "ref": "v1.0.0" },
                { "from": "gitlab", "package": "a/b/c" },
                { "from": "zip", "url": "https://example.com/x.zip" }
            ] }"#);
        let provider = GithubProvider::new(Arc::new(MockHttp::new()), None);
        let refs = provider.discover(&ctx).await.unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name.as_str(), "acme/skills");
        assert_eq!(
            refs[0].origin,
            Origin::Remote {
                host: "github.com".to_string(),
                package: "acme/skills".to_string(),
                r#ref: Some("v1.0.0".to_string()),
            }
        );
        assert_eq!(refs[0].filter, SkillsFilter::All);
    }

    #[tokio::test]
    async fn bad_package_shape_is_a_discover_error() {
        let (_tmp, ctx) = ctx(r#"{ "remote": [ { "from": "github", "package": "just-one" } ] }"#);
        let provider = GithubProvider::new(Arc::new(MockHttp::new()), None);
        let err = provider.discover(&ctx).await.unwrap_err();
        assert!(err.to_string().contains("owner/repo"), "{err}");
    }

    #[tokio::test]
    async fn skills_allowlist_flows_into_the_ref() {
        let (_tmp, ctx) =
            ctx(r#"{ "remote": [ { "from": "github", "package": "a/b", "skills": ["one"] } ] }"#);
        let provider = GithubProvider::new(Arc::new(MockHttp::new()), None);
        let refs = provider.discover(&ctx).await.unwrap();
        assert_eq!(refs[0].filter, SkillsFilter::Only(vec!["one".to_string()]));
    }

    #[tokio::test]
    async fn tags_and_zipball_urls_and_headers() {
        let http = Arc::new(
            MockHttp::new()
                .route(
                    "https://api.github.com/repos/acme/skills/tags?per_page=100",
                    200,
                    r#"[{"name":"v1.0.0"}]"#,
                )
                .route(
                    "https://api.github.com/repos/acme/skills/zipball/feature%2Fx",
                    200,
                    "zipbytes",
                ),
        );
        let api = GithubApi {
            http: Arc::clone(&http) as Arc<dyn HttpClient>,
            api_base: api_base(None),
            package: "acme/skills".to_string(),
            token: Some("tok-123".to_string()),
        };
        assert_eq!(api.list_tags().await.unwrap(), ["v1.0.0"]);
        assert_eq!(
            api.download_archive("feature/x").await.unwrap(),
            b"zipbytes"
        );

        let requests = http.requests();
        assert_eq!(requests.len(), 2);
        let (url, headers) = &requests[0];
        assert!(url.ends_with("/tags?per_page=100"), "{url}");
        assert!(headers.contains(&(
            "Accept".to_string(),
            "application/vnd.github+json".to_string()
        )));
        assert!(headers.contains(&("User-Agent".to_string(), "ai-skills".to_string())));
        assert!(headers.contains(&("Authorization".to_string(), "Bearer tok-123".to_string())));
        // Zipball ref is percent-encoded and downloaded with a permissive Accept.
        let (url, headers) = &requests[1];
        assert!(url.ends_with("/zipball/feature%2Fx"), "{url}");
        assert!(headers.contains(&("Accept".to_string(), "*/*".to_string())));
    }

    #[tokio::test]
    async fn malformed_token_degrades_to_anonymous() {
        let vendor = GithubVendor::new(
            Arc::new(MockHttp::new()),
            "a/b".to_string(),
            None,
            Some("main".to_string()),
            Some("bad token\n".to_string()),
        );
        assert!(vendor.api.token.is_none());
    }

    #[tokio::test]
    async fn http_error_status_is_reported_with_url() {
        let http: Arc<dyn HttpClient> = Arc::new(MockHttp::new());
        let api = GithubApi {
            http,
            api_base: api_base(None),
            package: "acme/skills".to_string(),
            token: None,
        };
        let err = api.list_tags().await.unwrap_err();
        assert!(err.contains("HTTP 404"), "{err}");
        assert!(err.contains("repos/acme/skills/tags"), "{err}");
    }
}
