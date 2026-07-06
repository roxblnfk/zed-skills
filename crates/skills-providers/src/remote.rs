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
/// cache when possible, otherwise download + extract into the cache. In
/// offline (cache-only) mode nothing is downloaded — a miss is
/// [`MaterializeError::NotFetched`].
pub(crate) async fn materialize_package(
    spec: &PackageSpec,
    api: &dyn HostApi,
    cache: &Cache,
) -> Result<skills_core::domain::MaterializedVendor, MaterializeError> {
    if cache.offline {
        return materialize_package_offline(spec, cache);
    }

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
        source_hint: skills_core::domain::SourceHint::Probe,
    })
}

/// Offline (cache-only) materialization: no API calls, no downloads. The
/// declared ref is resolved against the *cached* refs of this entry — every
/// completed cache dir's marker file records the resolved ref it holds:
///
/// - verbatim ref → itself;
/// - caret constraint → highest cached tag in range;
/// - absent ref → highest cached stable tag, else highest cached semver tag,
///   else the single cached ref (a previously resolved default branch).
///
/// Anything unresolvable (or a resolved ref without a completed cache dir)
/// is [`MaterializeError::NotFetched`] — the caller surfaces "run `skills
/// update`" instead of failing the analysis.
fn materialize_package_offline(
    spec: &PackageSpec,
    cache: &Cache,
) -> Result<skills_core::domain::MaterializedVendor, MaterializeError> {
    let not_fetched = || MaterializeError::NotFetched {
        vendor: spec.name.clone(),
    };
    let entry_root = cache
        .root
        .join(cachepath::encode_segment(spec.from))
        .join(cachepath::host_segment(spec.host.as_deref()))
        .join(cachepath::encode_segment(&spec.package));

    let resolved = match spec.ref_declared.as_deref() {
        Some(r#ref) if !refresolver::is_caret(r#ref) => r#ref.to_string(),
        declared => {
            let cached = cached_refs(&entry_root);
            let picked = match declared {
                Some(constraint) => refresolver::resolve_caret(constraint, &cached),
                None => refresolver::pick_highest_stable(&cached)
                    .or_else(|| refresolver::pick_highest_any(&cached))
                    .or(match cached.as_slice() {
                        [single] => Some(single.as_str()),
                        _ => None,
                    }),
            };
            picked.ok_or_else(not_fetched)?.to_string()
        }
    };

    let dir = cachepath::entry_dir(
        &cache.root,
        spec.from,
        spec.host.as_deref(),
        &spec.package,
        &resolved,
    );
    if !cachepath::is_hit(&dir) {
        return Err(not_fetched());
    }

    Ok(skills_core::domain::MaterializedVendor {
        name: spec.name.clone(),
        origin: spec.origin.clone(),
        root: dir,
        ref_resolved: Some(resolved),
        filter: skills_core::domain::SkillsFilter::All,
        source_hint: skills_core::domain::SourceHint::Probe,
    })
}

/// Resolved refs recorded by the markers of completed cache dirs under one
/// entry root (each marker file stores the resolved ref it was written for).
fn cached_refs(entry_root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(entry_root) else {
        return Vec::new();
    };
    let mut refs: Vec<String> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|dir| cachepath::is_hit(dir))
        .filter_map(|dir| std::fs::read_to_string(dir.join(cachepath::CACHE_MARKER)).ok())
        .map(|note| note.trim().to_string())
        .filter(|note| !note.is_empty())
        .collect();
    refs.sort();
    refs.dedup();
    refs
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

    /// HostApi that must never be reached (offline contract).
    struct NoApi;

    #[async_trait]
    impl HostApi for NoApi {
        async fn list_tags(&self) -> Result<Vec<String>, String> {
            panic!("offline materialize must not call the API");
        }
        async fn default_branch(&self) -> Result<String, String> {
            panic!("offline materialize must not call the API");
        }
        async fn download_archive(&self, _r: &str) -> Result<Vec<u8>, String> {
            panic!("offline materialize must not call the API");
        }
    }

    fn spec(package: &str, r#ref: Option<&str>) -> PackageSpec {
        PackageSpec {
            name: VendorName::new(package),
            origin: skills_core::domain::Origin::Remote {
                host: "github.com".to_string(),
                package: package.to_string(),
                r#ref: r#ref.map(str::to_string),
            },
            from: "github",
            host: None,
            package: package.to_string(),
            ref_declared: r#ref.map(str::to_string),
        }
    }

    /// Simulate a previous online run: completed cache dir + ref marker.
    fn seed_cache(cache: &Cache, package: &str, resolved: &str) -> PathBuf {
        let dir = cachepath::entry_dir(&cache.root, "github", None, package, resolved);
        std::fs::create_dir_all(&dir).unwrap();
        cachepath::write_marker(&dir, resolved).unwrap();
        dir
    }

    fn offline_cache(tmp: &tempfile::TempDir) -> Cache {
        let mut cache = Cache::new(tmp.path().join("cache"));
        cache.offline = true;
        cache
    }

    #[tokio::test]
    async fn offline_verbatim_ref_hits_cache_or_reports_not_fetched() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = offline_cache(&tmp);

        let err = materialize_package(&spec("a/b", Some("v1.0.0")), &NoApi, &cache)
            .await
            .unwrap_err();
        assert!(matches!(err, MaterializeError::NotFetched { .. }), "{err}");

        let dir = seed_cache(&cache, "a/b", "v1.0.0");
        let mv = materialize_package(&spec("a/b", Some("v1.0.0")), &NoApi, &cache)
            .await
            .unwrap();
        assert_eq!(mv.root, dir);
        assert_eq!(mv.ref_resolved.as_deref(), Some("v1.0.0"));
    }

    #[tokio::test]
    async fn offline_absent_ref_resolves_from_cached_markers() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = offline_cache(&tmp);
        seed_cache(&cache, "a/b", "v1.0.0");
        seed_cache(&cache, "a/b", "v1.2.0");

        let mv = materialize_package(&spec("a/b", None), &NoApi, &cache)
            .await
            .unwrap();
        assert_eq!(mv.ref_resolved.as_deref(), Some("v1.2.0"));
    }

    #[tokio::test]
    async fn offline_absent_ref_uses_single_cached_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = offline_cache(&tmp);
        // A previously resolved default branch is not a semver tag.
        seed_cache(&cache, "a/b", "main");

        let mv = materialize_package(&spec("a/b", None), &NoApi, &cache)
            .await
            .unwrap();
        assert_eq!(mv.ref_resolved.as_deref(), Some("main"));
    }

    #[tokio::test]
    async fn offline_caret_resolves_within_cached_tags() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = offline_cache(&tmp);
        seed_cache(&cache, "a/b", "v1.4.0");
        seed_cache(&cache, "a/b", "v2.0.0");

        let mv = materialize_package(&spec("a/b", Some("^1.2")), &NoApi, &cache)
            .await
            .unwrap();
        assert_eq!(mv.ref_resolved.as_deref(), Some("v1.4.0"));

        let err = materialize_package(&spec("a/b", Some("^3")), &NoApi, &cache)
            .await
            .unwrap_err();
        assert!(matches!(err, MaterializeError::NotFetched { .. }), "{err}");
    }

    #[tokio::test]
    async fn offline_empty_cache_is_not_fetched() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = offline_cache(&tmp);
        let err = materialize_package(&spec("a/b", None), &NoApi, &cache)
            .await
            .unwrap_err();
        assert!(matches!(err, MaterializeError::NotFetched { .. }), "{err}");
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
