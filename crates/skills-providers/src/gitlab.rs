//! GitLab provider: `remote[]` entries with `from: "gitlab"`.
//!
//! - `package` accepts 2+ non-empty segments of arbitrary depth
//!   (`org/group/sub/project` — nested subgroups).
//! - Project id in every endpoint = the whole path percent-encoded as ONE
//!   segment (`org%2Fgroup%2Fsub%2Fproject`).
//! - API base: `<host>/api/v4` (default host `gitlab.com`).
//! - Archive: `/projects/{id}/repository/archive.zip?sha={ref}`.
//! - Auth: `GITLAB_TOKEN` → `PRIVATE-TOKEN` header. Private projects
//!   masquerade as **404**, so 401/403/404 errors carry an auth hint.

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

pub const GITLAB_DEFAULT_HOST: &str = "gitlab.com";
const GITLAB_DEFAULT_API: &str = "https://gitlab.com/api/v4";
const TAGS_PER_PAGE: u32 = 100;

/// API base URL for a declared host (`None` = public gitlab.com).
pub fn api_base(host: Option<&str>) -> String {
    match host {
        None => GITLAB_DEFAULT_API.to_string(),
        Some(host) => {
            let normalized = remote::normalize_host(host);
            if normalized == "https://gitlab.com" {
                GITLAB_DEFAULT_API.to_string()
            } else {
                format!("{normalized}/api/v4")
            }
        }
    }
}

/// `group/project` with optional nesting: 2+ segments, none empty.
pub fn validate_package(package: &str) -> Result<(), String> {
    let segments: Vec<&str> = package.split('/').collect();
    if segments.len() < 2 || segments.iter().any(|s| s.is_empty()) {
        return Err(format!(
            "gitlab package must be 'group[/subgroup...]/project' with non-empty segments, got '{package}'"
        ));
    }
    Ok(())
}

/// One-line pointer at the token setup, appended to 401/403/404 errors —
/// GitLab reports private projects as 404 to avoid leaking their existence.
fn auth_hint() -> &'static str {
    " - if the project is private, set the GITLAB_TOKEN environment variable \
     (a personal access token with the read_api scope); note that GitLab \
     reports private projects as 404"
}

pub struct GitlabProvider {
    http: Arc<dyn HttpClient>,
    token: Option<String>,
}

impl GitlabProvider {
    pub fn new(http: Arc<dyn HttpClient>, token: Option<String>) -> Self {
        GitlabProvider {
            http,
            token: remote::usable_token(token),
        }
    }

    /// Production wiring: token from `GITLAB_TOKEN`.
    pub fn from_env(http: Arc<dyn HttpClient>) -> Self {
        Self::new(http, std::env::var("GITLAB_TOKEN").ok())
    }
}

#[async_trait]
impl VendorProvider for GitlabProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Gitlab
    }

    async fn discover(&self, ctx: &Ctx) -> Result<Vec<VendorRef>, DiscoverError> {
        let mut refs = Vec::new();
        for entry in ctx.manifest.remote.iter().flatten() {
            if entry.from != "gitlab" {
                continue;
            }
            let package = entry.package.clone().unwrap_or_default();
            validate_package(&package).map_err(|message| DiscoverError::Provider {
                provider: self.id(),
                message,
            })?;
            let vendor = GitlabVendor::new(
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

pub struct GitlabVendor {
    name: VendorName,
    origin: Origin,
    spec: PackageSpec,
    api: GitlabApi,
}

impl GitlabVendor {
    /// `package` must be pre-validated (2+ non-empty segments).
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
                .unwrap_or_else(|| GITLAB_DEFAULT_HOST.to_string()),
            package: package.clone(),
            r#ref: r#ref.clone(),
        };
        let api = GitlabApi {
            http,
            api_base: api_base(host.as_deref()),
            package: package.clone(),
            token: remote::usable_token(token),
        };
        let spec = PackageSpec {
            name: name.clone(),
            origin: origin.clone(),
            from: "gitlab",
            host,
            package,
            ref_declared: r#ref,
        };
        GitlabVendor {
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
impl Vendor for GitlabVendor {
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

struct GitlabApi {
    http: Arc<dyn HttpClient>,
    api_base: String,
    package: String,
    token: Option<String>,
}

impl GitlabApi {
    /// Percent-encoded project id — the whole path as one URL segment.
    fn project_id(&self) -> String {
        remote::percent_encode(&self.package)
    }

    fn headers(&self, accept: &str) -> Vec<(String, String)> {
        let mut headers = vec![
            ("Accept".to_string(), accept.to_string()),
            ("User-Agent".to_string(), "ai-skills".to_string()),
        ];
        if let Some(token) = &self.token {
            headers.push(("PRIVATE-TOKEN".to_string(), token.clone()));
        }
        headers
    }

    async fn get_ok(&self, url: &str, accept: &str) -> Result<Vec<u8>, String> {
        let response = remote::get(self.http.as_ref(), url, self.headers(accept)).await?;
        if !response.is_success() {
            let mut message = format!("{url} returned HTTP {}", response.status);
            if matches!(response.status, 401 | 403 | 404) {
                message.push_str(auth_hint());
            }
            return Err(message);
        }
        Ok(response.body)
    }
}

#[async_trait]
impl HostApi for GitlabApi {
    async fn list_tags(&self) -> Result<Vec<String>, String> {
        let url = format!(
            "{}/projects/{}/repository/tags?per_page={TAGS_PER_PAGE}",
            self.api_base,
            self.project_id()
        );
        let body = self.get_ok(&url, "application/json").await?;
        remote::parse_tag_names(&url, &body)
    }

    async fn default_branch(&self) -> Result<String, String> {
        let url = format!("{}/projects/{}", self.api_base, self.project_id());
        let body = self.get_ok(&url, "application/json").await?;
        remote::parse_default_branch(&url, &body)
    }

    async fn download_archive(&self, r#ref: &str) -> Result<Vec<u8>, String> {
        let url = format!(
            "{}/projects/{}/repository/archive.zip?sha={}",
            self.api_base,
            self.project_id(),
            remote::percent_encode(r#ref)
        );
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

    fn api(http: Arc<MockHttp>, package: &str, token: Option<&str>) -> GitlabApi {
        GitlabApi {
            http,
            api_base: api_base(None),
            package: package.to_string(),
            token: token.map(str::to_string),
        }
    }

    #[test]
    fn api_base_mapping() {
        assert_eq!(api_base(None), "https://gitlab.com/api/v4");
        assert_eq!(api_base(Some("gitlab.com")), "https://gitlab.com/api/v4");
        assert_eq!(
            api_base(Some("https://gitlab.com")),
            "https://gitlab.com/api/v4"
        );
        assert_eq!(
            api_base(Some("gitlab.example.com")),
            "https://gitlab.example.com/api/v4"
        );
        assert_eq!(
            api_base(Some("http://127.0.0.1:9999/")),
            "http://127.0.0.1:9999/api/v4"
        );
    }

    #[test]
    fn package_shape_validation() {
        validate_package("group/project").unwrap();
        validate_package("org/group/sub/project").unwrap();
        for bad in [
            "project",
            "group/",
            "/project",
            "group//project",
            "group/sub//project",
            "",
        ] {
            assert!(validate_package(bad).is_err(), "expected error: {bad:?}");
        }
    }

    #[tokio::test]
    async fn discovers_gitlab_entries_with_subgroups() {
        let (_tmp, ctx) = ctx(r#"{ "remote": [
                { "from": "gitlab", "package": "org/group/sub/project", "host": "gitlab.example.com" },
                { "from": "github", "package": "a/b" }
            ] }"#);
        let provider = GitlabProvider::new(Arc::new(MockHttp::new()), None);
        let refs = provider.discover(&ctx).await.unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name.as_str(), "org/group/sub/project");
        assert_eq!(
            refs[0].origin,
            Origin::Remote {
                host: "gitlab.example.com".to_string(),
                package: "org/group/sub/project".to_string(),
                r#ref: None,
            }
        );
    }

    #[tokio::test]
    async fn empty_segment_package_is_a_discover_error() {
        let (_tmp, ctx) =
            ctx(r#"{ "remote": [ { "from": "gitlab", "package": "group//project" } ] }"#);
        let provider = GitlabProvider::new(Arc::new(MockHttp::new()), None);
        let err = provider.discover(&ctx).await.unwrap_err();
        assert!(err.to_string().contains("non-empty segments"), "{err}");
    }

    #[tokio::test]
    async fn deep_subgroup_project_id_is_one_percent_encoded_segment() {
        let http = Arc::new(MockHttp::new().route(
            "https://gitlab.com/api/v4/projects/org%2Fgroup%2Fsub%2Fproject/repository/tags?per_page=100",
            200,
            r#"[{"name":"v2.0.0"}]"#,
        ));
        let api = api(Arc::clone(&http), "org/group/sub/project", Some("glpat-x"));
        assert_eq!(api.list_tags().await.unwrap(), ["v2.0.0"]);

        let (url, headers) = &http.requests()[0];
        assert!(
            url.contains("/projects/org%2Fgroup%2Fsub%2Fproject/"),
            "{url}"
        );
        assert!(!url.contains("/projects/org/group"), "{url}");
        assert!(headers.contains(&("PRIVATE-TOKEN".to_string(), "glpat-x".to_string())));
        assert!(headers.contains(&("User-Agent".to_string(), "ai-skills".to_string())));
    }

    #[tokio::test]
    async fn archive_url_uses_sha_query_param() {
        let http = Arc::new(MockHttp::new().route(
            "https://gitlab.com/api/v4/projects/a%2Fb/repository/archive.zip?sha=v1.0.0",
            200,
            "zipbytes",
        ));
        let api = api(http, "a/b", None);
        assert_eq!(api.download_archive("v1.0.0").await.unwrap(), b"zipbytes");
    }

    #[tokio::test]
    async fn not_found_errors_carry_the_auth_hint() {
        // MockHttp answers 404 for unrouted URLs.
        let api = api(Arc::new(MockHttp::new()), "org/private/repo", None);
        let err = api.list_tags().await.unwrap_err();
        assert!(err.contains("HTTP 404"), "{err}");
        assert!(err.contains("GITLAB_TOKEN"), "{err}");
        assert!(err.contains("private projects as 404"), "{err}");
    }

    #[tokio::test]
    async fn unauthorized_and_forbidden_also_hint() {
        for status in [401u16, 403] {
            let http = Arc::new(MockHttp::new().route(
                "https://gitlab.com/api/v4/projects/a%2Fb/repository/tags?per_page=100",
                status,
                "denied",
            ));
            let api = api(http, "a/b", None);
            let err = api.list_tags().await.unwrap_err();
            assert!(err.contains(&format!("HTTP {status}")), "{err}");
            assert!(err.contains("GITLAB_TOKEN"), "{err}");
        }
    }

    #[tokio::test]
    async fn server_errors_do_not_hint() {
        let http = Arc::new(MockHttp::new().route(
            "https://gitlab.com/api/v4/projects/a%2Fb/repository/tags?per_page=100",
            500,
            "boom",
        ));
        let api = api(http, "a/b", None);
        let err = api.list_tags().await.unwrap_err();
        assert!(err.contains("HTTP 500"), "{err}");
        assert!(!err.contains("GITLAB_TOKEN"), "{err}");
    }
}
