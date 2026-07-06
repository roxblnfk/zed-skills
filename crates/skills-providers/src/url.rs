//! By-url provider: `remote[]` entries with `from: "http" | "zip"`.
//!
//! Downloads the archive from the literal `url`, optionally verifies its
//! SHA-256, then runs the same zip handling as the by-package providers.
//! Cache layout: `<root>/url/<sha256(url)[..16]>/<ref-seg>/` where the ref
//! label is the sha256 prefix when declared, else `latest`.

use std::sync::Arc;

use async_trait::async_trait;

use sha2::{Digest, Sha256};
use skills_core::domain::{
    MaterializedVendor, Origin, ProviderId, SkillsFilter, VendorName, VendorRef,
};
use skills_core::error::{DiscoverError, MaterializeError};
use skills_core::pipeline::ctx::Ctx;
use skills_core::traits::{Cache, Vendor, VendorProvider};

use crate::cachepath;
use crate::http::HttpClient;
use crate::remote;

pub struct UrlProvider {
    http: Arc<dyn HttpClient>,
}

impl UrlProvider {
    pub fn new(http: Arc<dyn HttpClient>) -> Self {
        UrlProvider { http }
    }
}

#[async_trait]
impl VendorProvider for UrlProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Url
    }

    async fn discover(&self, ctx: &Ctx) -> Result<Vec<VendorRef>, DiscoverError> {
        let mut refs = Vec::new();
        for entry in ctx.manifest.remote.iter().flatten() {
            if entry.from != "http" && entry.from != "zip" {
                continue;
            }
            let url = entry.url.clone().unwrap_or_default();
            if let Some(sha256) = &entry.sha256
                && !is_hex_sha256(sha256)
            {
                return Err(DiscoverError::Provider {
                    provider: self.id(),
                    message: format!("sha256 for '{url}' must be 64 hex chars, got '{sha256}'"),
                });
            }
            let vendor = UrlVendor::new(Arc::clone(&self.http), url, entry.sha256.clone());
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

fn is_hex_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

pub struct UrlVendor {
    name: VendorName,
    origin: Origin,
    url: String,
    sha256: Option<String>,
    http: Arc<dyn HttpClient>,
}

impl UrlVendor {
    pub fn new(http: Arc<dyn HttpClient>, url: String, sha256: Option<String>) -> Self {
        UrlVendor {
            name: VendorName::new(&url),
            origin: Origin::Url { url: url.clone() },
            url,
            sha256,
            http,
        }
    }

    /// Ref label used in the cache path: content-addressed by sha256 when
    /// declared, otherwise the moving `latest`.
    fn ref_label(&self) -> String {
        match &self.sha256 {
            Some(sha) => sha.get(..12).unwrap_or(sha).to_ascii_lowercase(),
            None => "latest".to_string(),
        }
    }
}

#[async_trait]
impl Vendor for UrlVendor {
    fn name(&self) -> &VendorName {
        &self.name
    }

    fn origin(&self) -> &Origin {
        &self.origin
    }

    async fn materialize(&self, cache: &Cache) -> Result<MaterializedVendor, MaterializeError> {
        let vendor_err = |message: String| MaterializeError::Vendor {
            vendor: self.name.clone(),
            message,
        };

        // `--refresh`: drop every cached label of this URL.
        if cache.refresh {
            let url_root = cache.root.join("url").join(cachepath::url_hash(&self.url));
            remote::remove_dir_if_exists(&url_root).map_err(|e| {
                vendor_err(format!(
                    "failed to refresh cache at {}: {e}",
                    url_root.display()
                ))
            })?;
        }

        let dir = cachepath::url_dir(&cache.root, &self.url, &self.ref_label());
        if !cachepath::is_hit(&dir) {
            let headers = vec![
                ("Accept".to_string(), "*/*".to_string()),
                ("User-Agent".to_string(), "ai-skills".to_string()),
            ];
            let response = remote::get(self.http.as_ref(), &self.url, headers)
                .await
                .map_err(&vendor_err)?;
            if !response.is_success() {
                return Err(vendor_err(format!(
                    "{} returned HTTP {}",
                    self.url, response.status
                )));
            }
            if let Some(expected) = &self.sha256 {
                let actual = hex_digest(&response.body);
                if !actual.eq_ignore_ascii_case(expected) {
                    return Err(vendor_err(format!(
                        "sha256 mismatch for {}: expected {expected}, got {actual}",
                        self.url
                    )));
                }
            }
            remote::populate_cache_entry(cache, &dir, response.body, &self.url)
                .await
                .map_err(&vendor_err)?;
        }

        Ok(MaterializedVendor {
            name: self.name.clone(),
            origin: self.origin.clone(),
            root: dir,
            ref_resolved: None,
            filter: SkillsFilter::All,
        })
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest.iter() {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::MockHttp;
    use crate::testkit::build_zip;
    use skills_core::manifest::MANIFEST_NAME;
    use skills_core::pipeline::ctx::{PrepareOptions, prepare};

    fn ctx(manifest: &str) -> (tempfile::TempDir, Ctx) {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(MANIFEST_NAME), manifest).unwrap();
        let ctx = prepare(tmp.path(), PrepareOptions::default()).unwrap();
        (tmp, ctx)
    }

    fn fixture_zip() -> Vec<u8> {
        build_zip(&[(
            "repo/skills/alpha/SKILL.md",
            Some("---\nname: alpha\n---\n"),
        )])
    }

    #[tokio::test]
    async fn discovers_http_and_zip_entries() {
        let (_tmp, ctx) = ctx(r#"{ "remote": [
                { "from": "zip", "url": "https://example.com/a.zip" },
                { "from": "http", "url": "https://example.com/b" },
                { "from": "github", "package": "a/b" }
            ] }"#);
        let provider = UrlProvider::new(Arc::new(MockHttp::new()));
        let refs = provider.discover(&ctx).await.unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].name.as_str(), "https://example.com/a.zip");
        assert_eq!(
            refs[0].origin,
            Origin::Url {
                url: "https://example.com/a.zip".to_string()
            }
        );
    }

    #[tokio::test]
    async fn malformed_sha256_is_a_discover_error() {
        let (_tmp, ctx) =
            ctx(r#"{ "remote": [ { "from": "zip", "url": "https://x/a.zip", "sha256": "zz" } ] }"#);
        let provider = UrlProvider::new(Arc::new(MockHttp::new()));
        let err = provider.discover(&ctx).await.unwrap_err();
        assert!(err.to_string().contains("64 hex chars"), "{err}");
    }

    #[tokio::test]
    async fn downloads_verifies_and_extracts() {
        let bytes = fixture_zip();
        let expected = hex_digest(&bytes);
        let http = Arc::new(MockHttp::new().route("https://example.com/a.zip", 200, bytes));
        let vendor = UrlVendor::new(
            Arc::clone(&http) as Arc<dyn HttpClient>,
            "https://example.com/a.zip".to_string(),
            Some(expected),
        );
        let tmp = tempfile::tempdir().unwrap();
        let cache = Cache::new(tmp.path().join("cache"));
        let mv = vendor.materialize(&cache).await.unwrap();
        assert!(
            mv.root
                .join("skills")
                .join("alpha")
                .join("SKILL.md")
                .is_file()
        );
        assert_eq!(mv.ref_resolved, None);

        // Cache hit: no second download.
        vendor.materialize(&cache).await.unwrap();
        assert_eq!(http.request_count(), 1);
    }

    #[tokio::test]
    async fn sha256_mismatch_is_an_error_and_nothing_is_cached() {
        let http = Arc::new(MockHttp::new().route("https://example.com/a.zip", 200, fixture_zip()));
        let vendor = UrlVendor::new(
            http,
            "https://example.com/a.zip".to_string(),
            Some("0".repeat(64)),
        );
        let tmp = tempfile::tempdir().unwrap();
        let cache = Cache::new(tmp.path().join("cache"));
        let err = vendor.materialize(&cache).await.unwrap_err();
        assert!(err.to_string().contains("sha256 mismatch"), "{err}");
        assert!(!cache.root.exists());
    }

    #[tokio::test]
    async fn http_error_is_a_vendor_error() {
        let vendor = UrlVendor::new(
            Arc::new(MockHttp::new()),
            "https://example.com/missing.zip".to_string(),
            None,
        );
        let tmp = tempfile::tempdir().unwrap();
        let cache = Cache::new(tmp.path().join("cache"));
        let err = vendor.materialize(&cache).await.unwrap_err();
        assert!(err.to_string().contains("HTTP 404"), "{err}");
    }

    #[tokio::test]
    async fn refresh_forces_redownload() {
        let http = Arc::new(MockHttp::new().route("https://example.com/a.zip", 200, fixture_zip()));
        let vendor = UrlVendor::new(
            Arc::clone(&http) as Arc<dyn HttpClient>,
            "https://example.com/a.zip".to_string(),
            None,
        );
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = Cache::new(tmp.path().join("cache"));
        vendor.materialize(&cache).await.unwrap();
        assert_eq!(http.request_count(), 1);

        cache.refresh = true;
        vendor.materialize(&cache).await.unwrap();
        assert_eq!(http.request_count(), 2);
    }
}
