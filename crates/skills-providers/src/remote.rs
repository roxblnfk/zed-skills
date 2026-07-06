//! Shared machinery for by-package remote vendors (GitHub / GitLab).
//!
//! The two hosts differ only in their API surface; ref resolution, caching
//! and extraction are identical. Each host implements [`HostApi`]; the
//! functions here drive the cascade and the cache.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use skills_core::domain::VendorName;
use skills_core::error::MaterializeError;
use skills_core::traits::Cache;

use crate::cachepath;
use crate::http::{HttpClient, HttpResponse};
use crate::refresolver;

/// Host-specific API surface consumed by the shared resolve/materialize
/// logic. Errors are plain messages; callers wrap them with vendor context.
#[async_trait]
pub(crate) trait HostApi: Send + Sync {
    /// Tag names, newest-first or not — order is irrelevant.
    async fn list_tags(&self) -> Result<Vec<String>, String>;
    async fn default_branch(&self) -> Result<String, String>;
    async fn download_archive(&self, r#ref: &str) -> Result<Vec<u8>, String>;
}

/// Resolve the manifest-declared ref (or its absence) to a concrete
/// tag / branch / SHA (SPEC §7 Ref resolution).
pub(crate) async fn resolve_ref(
    api: &dyn HostApi,
    package: &str,
    declared: Option<&str>,
) -> Result<String, String> {
    match declared {
        // Verbatim ref: tag, branch or SHA — the archive endpoints accept
        // any of them, no API roundtrip needed.
        Some(r#ref) if !refresolver::is_caret(r#ref) => Ok(r#ref.to_string()),
        // Caret constraint: highest stable tag in range; `^0.x` and
        // no-match are errors.
        Some(constraint) => {
            let tags = api.list_tags().await?;
            refresolver::resolve_caret(constraint, &tags)
                .map(str::to_string)
                .ok_or_else(|| format!("no tag in {package} matches constraint {constraint}"))
        }
        // Absent ref: highest stable -> highest any-semver -> default branch.
        None => {
            let tags = api.list_tags().await?;
            if let Some(tag) = refresolver::pick_highest_stable(&tags) {
                return Ok(tag.to_string());
            }
            if let Some(tag) = refresolver::pick_highest_any(&tags) {
                return Ok(tag.to_string());
            }
            api.default_branch().await
        }
    }
}

/// Everything the shared materializer needs to know about one by-package
/// vendor, independent of the host API.
pub(crate) struct PackageSpec {
    pub name: VendorName,
    pub origin: skills_core::domain::Origin,
    /// Manifest `from` value: `github` | `gitlab`.
    pub from: &'static str,
    /// Declared host (manifest `host`), `None` for the public default.
    pub host: Option<String>,
    pub package: String,
    pub ref_declared: Option<String>,
}

/// Materialize a by-package remote vendor: resolve the ref, serve from the
/// cache when possible, otherwise download + extract into the cache.
pub(crate) async fn materialize_package(
    spec: &PackageSpec,
    api: &dyn HostApi,
    cache: &Cache,
) -> Result<skills_core::domain::MaterializedVendor, MaterializeError> {
    let vendor_err = |message: String| MaterializeError::Vendor {
        vendor: spec.name.clone(),
        message,
    };

    // `--refresh`: drop every cached ref of this entry before anything else.
    if cache.refresh {
        let entry_root = cache
            .root
            .join(cachepath::encode_segment(spec.from))
            .join(cachepath::host_segment(spec.host.as_deref()))
            .join(cachepath::encode_segment(&spec.package));
        remove_dir_if_exists(&entry_root).map_err(|e| {
            vendor_err(format!(
                "failed to refresh cache at {}: {e}",
                entry_root.display()
            ))
        })?;
    }

    let resolved = resolve_ref(api, &spec.package, spec.ref_declared.as_deref())
        .await
        .map_err(vendor_err)?;

    let dir = cachepath::entry_dir(
        &cache.root,
        spec.from,
        spec.host.as_deref(),
        &spec.package,
        &resolved,
    );

    if !cachepath::is_hit(&dir) {
        let bytes = api.download_archive(&resolved).await.map_err(vendor_err)?;
        populate_cache_entry(cache, &dir, bytes, &resolved)
            .await
            .map_err(&vendor_err)?;
    }

    Ok(skills_core::domain::MaterializedVendor {
        name: spec.name.clone(),
        origin: spec.origin.clone(),
        root: dir,
        ref_resolved: Some(resolved),
        filter: skills_core::domain::SkillsFilter::All,
    })
}

/// Extract downloaded archive bytes into `dir` and mark the entry complete.
/// Blocking work happens on the blocking pool.
pub(crate) async fn populate_cache_entry(
    cache: &Cache,
    dir: &Path,
    bytes: Vec<u8>,
    marker_note: &str,
) -> Result<(), String> {
    ensure_cache_root(&cache.root).map_err(|e| format!("failed to set up cache root: {e}"))?;
    let dir_owned = dir.to_path_buf();
    let note = marker_note.to_string();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        crate::archive::extract_zip_unwrapped(&bytes, &dir_owned).map_err(|e| e.to_string())?;
        cachepath::write_marker(&dir_owned, &note)
            .map_err(|e| format!("failed to write cache marker: {e}"))
    })
    .await
    .map_err(|e| format!("extract task panicked: {e}"))?
}

/// Create the cache root and drop a `.gitignore` ignoring everything in it —
/// belt and braces on top of the project-level ignore entry.
pub(crate) fn ensure_cache_root(root: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(root)?;
    let gitignore = root.join(".gitignore");
    if !gitignore.exists() {
        std::fs::write(gitignore, "*\n")?;
    }
    Ok(())
}

pub(crate) fn remove_dir_if_exists(dir: &PathBuf) -> std::io::Result<()> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Percent-encode a string as a single URL path segment (RFC 3986
/// unreserved characters pass through). `a/b/c` becomes `a%2Fb%2Fc` — the
/// GitLab "project id" trick.
pub(crate) fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                use std::fmt::Write;
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// Normalize a declared host to `scheme://host` form: a bare host gets
/// `https://`, trailing slashes are dropped.
pub(crate) fn normalize_host(host: &str) -> String {
    let host = host.trim().trim_end_matches('/');
    if host.contains("://") {
        host.to_string()
    } else {
        format!("https://{host}")
    }
}

/// GET returning a plain error message for transport failures. HTTP error
/// statuses are returned as responses — status handling is caller policy.
pub(crate) async fn get(
    http: &dyn HttpClient,
    url: &str,
    headers: Vec<(String, String)>,
) -> Result<HttpResponse, String> {
    http.get(url, &headers).await.map_err(|e| e.to_string())
}

/// Parse a tag-listing response (`[{ "name": "v1.0.0", ... }, ...]`) into
/// tag names. Non-string / missing names are skipped.
pub(crate) fn parse_tag_names(url: &str, body: &[u8]) -> Result<Vec<String>, String> {
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("{url} returned invalid JSON: {e}"))?;
    let serde_json::Value::Array(items) = value else {
        return Err(format!("{url} returned a non-array body"));
    };
    Ok(items
        .iter()
        .filter_map(|item| item.get("name"))
        .filter_map(|name| name.as_str())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect())
}

/// Parse a project/repo response and extract `default_branch`.
pub(crate) fn parse_default_branch(url: &str, body: &[u8]) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("{url} returned invalid JSON: {e}"))?;
    match value.get("default_branch").and_then(|b| b.as_str()) {
        Some(branch) if !branch.is_empty() => Ok(branch.to_string()),
        _ => Err(format!("{url} did not return a non-empty default_branch")),
    }
}

/// A token usable as an HTTP header value (visible ASCII). Malformed tokens
/// degrade to anonymous access instead of poisoning every request.
pub(crate) fn usable_token(token: Option<String>) -> Option<String> {
    token.filter(|t| !t.is_empty() && t.bytes().all(|b| (0x21..=0x7e).contains(&b)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encode_gitlab_project_id() {
        assert_eq!(percent_encode("a/b"), "a%2Fb");
        assert_eq!(
            percent_encode("org/group/sub/project"),
            "org%2Fgroup%2Fsub%2Fproject"
        );
        assert_eq!(percent_encode("v1.2.3"), "v1.2.3");
        assert_eq!(percent_encode("feature branch"), "feature%20branch");
        assert_eq!(percent_encode("a-b_c.d~e"), "a-b_c.d~e");
    }

    #[test]
    fn normalize_host_variants() {
        assert_eq!(
            normalize_host("gitlab.example.com"),
            "https://gitlab.example.com"
        );
        assert_eq!(
            normalize_host("https://gitlab.example.com/"),
            "https://gitlab.example.com"
        );
        assert_eq!(
            normalize_host("http://127.0.0.1:8080"),
            "http://127.0.0.1:8080"
        );
    }

    #[test]
    fn tag_names_parse_and_skip_junk() {
        let body = br#"[{"name":"v1.0.0"},{"name":""},{"nope":1},{"name":2},{"name":"main"}]"#;
        assert_eq!(
            parse_tag_names("u", body).unwrap(),
            vec!["v1.0.0".to_string(), "main".to_string()]
        );
        assert!(
            parse_tag_names("u", b"{}")
                .unwrap_err()
                .contains("non-array")
        );
        assert!(
            parse_tag_names("u", b"nope")
                .unwrap_err()
                .contains("invalid JSON")
        );
    }

    #[test]
    fn default_branch_parse() {
        assert_eq!(
            parse_default_branch("u", br#"{"default_branch":"main"}"#).unwrap(),
            "main"
        );
        assert!(parse_default_branch("u", br#"{"default_branch":""}"#).is_err());
        assert!(parse_default_branch("u", br#"{}"#).is_err());
    }

    #[test]
    fn malformed_tokens_degrade_to_anonymous() {
        assert_eq!(usable_token(None), None);
        assert_eq!(usable_token(Some(String::new())), None);
        assert_eq!(usable_token(Some("with space".into())), None);
        assert_eq!(usable_token(Some("with\nnewline".into())), None);
        assert_eq!(
            usable_token(Some("ghp_abc123".into())),
            Some("ghp_abc123".to_string())
        );
    }

    struct ScriptedApi {
        tags: Vec<String>,
        branch: Option<String>,
    }

    #[async_trait]
    impl HostApi for ScriptedApi {
        async fn list_tags(&self) -> Result<Vec<String>, String> {
            Ok(self.tags.clone())
        }
        async fn default_branch(&self) -> Result<String, String> {
            self.branch.clone().ok_or_else(|| "no branch".to_string())
        }
        async fn download_archive(&self, _r: &str) -> Result<Vec<u8>, String> {
            Err("not used".to_string())
        }
    }

    fn api(tags: &[&str], branch: Option<&str>) -> ScriptedApi {
        ScriptedApi {
            tags: tags.iter().map(|s| s.to_string()).collect(),
            branch: branch.map(str::to_string),
        }
    }

    #[tokio::test]
    async fn verbatim_ref_needs_no_api_calls() {
        let a = api(&[], None);
        assert_eq!(
            resolve_ref(&a, "a/b", Some("my-branch")).await.unwrap(),
            "my-branch"
        );
        assert_eq!(
            resolve_ref(&a, "a/b", Some("v9.9.9")).await.unwrap(),
            "v9.9.9"
        );
    }

    #[tokio::test]
    async fn absent_ref_cascades_stable_then_any_then_branch() {
        let a = api(&["v1.0.0", "v1.1.0", "2.0.0-rc.1"], Some("main"));
        assert_eq!(resolve_ref(&a, "a/b", None).await.unwrap(), "v1.1.0");

        let a = api(&["2.0.0-rc.1", "weird"], Some("main"));
        assert_eq!(resolve_ref(&a, "a/b", None).await.unwrap(), "2.0.0-rc.1");

        let a = api(&["weird"], Some("develop"));
        assert_eq!(resolve_ref(&a, "a/b", None).await.unwrap(), "develop");
    }

    #[tokio::test]
    async fn caret_resolves_or_errors() {
        let a = api(&["v1.2.0", "v1.9.0", "v2.0.0"], None);
        assert_eq!(
            resolve_ref(&a, "a/b", Some("^1.2")).await.unwrap(),
            "v1.9.0"
        );

        let err = resolve_ref(&a, "a/b", Some("^3")).await.unwrap_err();
        assert!(err.contains("no tag in a/b matches constraint ^3"), "{err}");

        // ^0.x is unsupported by design.
        let a = api(&["0.2.0", "0.3.0"], None);
        let err = resolve_ref(&a, "a/b", Some("^0.2")).await.unwrap_err();
        assert!(err.contains("no tag"), "{err}");
    }
}
