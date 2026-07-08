//! `ComposerProvider` — donors discovered from an installed composer
//! project (SPEC §6.1, §8).
//!
//! Enumeration reads `vendor/composer/installed.json` (both the wrapped
//! `{"packages": [...]}` and the legacy bare-array forms); when that file is
//! absent it falls back to scanning `vendor/<vendor>/<package>/` dirs for
//! `composer.json`. No composer runtime is involved.
//!
//! A package becomes a donor by setting `extra.skills.source`. The mere
//! presence of `extra.skills` is not enough (root-level options such as
//! `aliases` are meaningful for the root project only) — those packages are
//! skipped silently, like any other undeclared package, unless they ship
//! discoverable skills, in which case they become discovery *candidates*
//! ([`DonorStatus::Undeclared`]). A declared-but-invalid source marks the
//! donor [`DonorStatus::Malformed`]; the TrustFilter stage drops it with a
//! warning, never blocking the run.
//!
//! Direct dependencies (root `require` / `require-dev`) are tagged
//! [`TrustBasis::DirectDependency`]; everything else is transitive and must
//! clear the trust list.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use skills_core::domain::{
    DonorStatus, MaterializedVendor, Origin, ProviderId, SkillsFilter, SourceHint, TrustBasis,
    VendorName, VendorRef,
};
use skills_core::error::{DiscoverError, MaterializeError};
use skills_core::paths::{is_absolute_like, normalize_rel};
use skills_core::pipeline::ctx::Ctx;
use skills_core::traits::{Cache, Vendor, VendorProvider};

use crate::treescan;

pub struct ComposerProvider;

#[async_trait]
impl VendorProvider for ComposerProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Composer
    }

    async fn discover(&self, ctx: &Ctx) -> Result<Vec<VendorRef>, DiscoverError> {
        if !ctx.manifest.composer_enabled() {
            return Ok(Vec::new());
        }
        let project_root = ctx.project_root.clone();
        tokio::task::spawn_blocking(move || discover_blocking(&project_root))
            .await
            .map_err(|e| DiscoverError::Provider {
                provider: ProviderId::Composer,
                message: format!("discover task panicked: {e}"),
            })?
    }
}

fn discover_blocking(project_root: &Path) -> Result<Vec<VendorRef>, DiscoverError> {
    let mut packages = enumerate_packages(project_root)?;
    packages.sort_by(|a, b| a.name.cmp(&b.name));
    let direct = direct_dependencies(project_root);

    let mut refs = Vec::new();
    for package in packages {
        if !package.root.is_dir() {
            // Metapackages / missing install paths carry no skills.
            continue;
        }

        let (status, source_hint) = match declared_skills_source(package.extra.as_ref()) {
            Some(source_value) => match validate_source(source_value) {
                Ok(normalized) => (DonorStatus::Declared, SourceHint::Declared(normalized)),
                Err(reason) => (
                    DonorStatus::Malformed { reason },
                    // Never used: malformed donors are dropped before
                    // Materialize.
                    SourceHint::Discovery,
                ),
            },
            None => {
                if !treescan::has_discoverable_skills(&package.root) {
                    // Not a donor, nothing discoverable: silently invisible.
                    continue;
                }
                (DonorStatus::Undeclared, SourceHint::Discovery)
            }
        };

        let trust = if direct.contains(&package.name) {
            TrustBasis::DirectDependency
        } else {
            TrustBasis::Transitive
        };
        let name = VendorName::new(&package.name);
        let origin = Origin::Local {
            path: origin_path(project_root, &package.root),
        };
        refs.push(VendorRef {
            provider: ProviderId::Composer,
            name: name.clone(),
            origin: origin.clone(),
            filter: SkillsFilter::All,
            trust,
            status,
            vendor: Arc::new(ComposerVendor {
                name,
                origin,
                root: package.root,
                source_hint,
            }),
        });
    }
    Ok(refs)
}

/// One installed package as far as this provider cares.
#[derive(Debug, Clone, PartialEq)]
struct InstalledPackage {
    name: String,
    extra: Option<serde_json::Value>,
    /// Absolute install directory.
    root: PathBuf,
}

/// Enumerate installed packages: `vendor/composer/installed.json` first,
/// directory scan as fallback. An absent `vendor/` yields no packages —
/// the provider stays silent outside composer projects.
fn enumerate_packages(project_root: &Path) -> Result<Vec<InstalledPackage>, DiscoverError> {
    let vendor_dir = project_root.join("vendor");
    let installed = vendor_dir.join("composer").join("installed.json");
    if installed.is_file() {
        return parse_installed_json(&installed, &vendor_dir);
    }
    if vendor_dir.is_dir() {
        return Ok(scan_vendor_dir(&vendor_dir));
    }
    Ok(Vec::new())
}

/// Parse `installed.json`. Both forms are accepted: composer 2 wraps the
/// package list (`{"packages": [...]}`); composer 1 wrote a bare array.
fn parse_installed_json(
    path: &Path,
    vendor_dir: &Path,
) -> Result<Vec<InstalledPackage>, DiscoverError> {
    let provider_err = |message: String| DiscoverError::Provider {
        provider: ProviderId::Composer,
        message,
    };
    let raw = std::fs::read_to_string(path).map_err(|source| DiscoverError::Io {
        provider: ProviderId::Composer,
        path: path.to_path_buf(),
        source,
    })?;
    let value: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| provider_err(format!("{}: invalid JSON: {e}", path.display())))?;
    let entries = match &value {
        serde_json::Value::Array(items) => items.as_slice(),
        serde_json::Value::Object(map) => match map.get("packages") {
            Some(serde_json::Value::Array(items)) => items.as_slice(),
            _ => {
                return Err(provider_err(format!(
                    "{}: expected a package array or a {{\"packages\": [...]}} wrapper",
                    path.display()
                )));
            }
        },
        _ => {
            return Err(provider_err(format!(
                "{}: expected a package array or a {{\"packages\": [...]}} wrapper",
                path.display()
            )));
        }
    };

    // `install-path` is relative to vendor/composer/ (composer 2). Absent
    // (composer 1) defaults to `vendor/<name>`.
    let base = vendor_dir.join("composer");
    let mut out = Vec::new();
    for entry in entries {
        let Some(name) = entry.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        if !name.contains('/') {
            continue;
        }
        let root = match entry.get("install-path").and_then(|p| p.as_str()) {
            Some(raw_path) => resolve_install_path(&base, raw_path),
            None => name
                .split('/')
                .fold(vendor_dir.to_path_buf(), |d, s| d.join(s)),
        };
        out.push(InstalledPackage {
            name: name.to_string(),
            extra: entry.get("extra").cloned(),
            root,
        });
    }
    Ok(out)
}

/// Lexically resolve an `install-path` against `vendor/composer/`.
fn resolve_install_path(base: &Path, raw: &str) -> PathBuf {
    if is_absolute_like(raw) {
        return PathBuf::from(raw);
    }
    let mut out = base.to_path_buf();
    for segment in raw.split(['/', '\\']) {
        match segment {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out
}

/// Fallback enumeration without `installed.json`: every
/// `vendor/<vendor>/<package>/composer.json` is a package.
fn scan_vendor_dir(vendor_dir: &Path) -> Vec<InstalledPackage> {
    let mut out = Vec::new();
    for namespace in sorted_subdirs(vendor_dir) {
        let ns_name = dir_name(&namespace);
        if ns_name.starts_with('.') || ns_name == "composer" || ns_name == "bin" {
            continue;
        }
        for package_dir in sorted_subdirs(&namespace) {
            let manifest = package_dir.join("composer.json");
            if !manifest.is_file() {
                continue;
            }
            let parsed: Option<serde_json::Value> = std::fs::read_to_string(&manifest)
                .ok()
                .and_then(|raw| serde_json::from_str(&raw).ok());
            let fallback_name = format!("{ns_name}/{}", dir_name(&package_dir));
            let name = parsed
                .as_ref()
                .and_then(|v| v.get("name"))
                .and_then(|n| n.as_str())
                .filter(|n| n.contains('/'))
                .map(str::to_string)
                .unwrap_or(fallback_name);
            out.push(InstalledPackage {
                name,
                extra: parsed.and_then(|v| v.get("extra").cloned()),
                root: package_dir,
            });
        }
    }
    out
}

fn sorted_subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    out.sort();
    out
}

fn dir_name(dir: &Path) -> String {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Package names declared in the root `composer.json` under `require` /
/// `require-dev`. Platform requirements (`php`, `ext-json`, …) carry no
/// slash and are skipped. No `composer.json` at all ⇒ empty set.
fn direct_dependencies(project_root: &Path) -> BTreeSet<String> {
    let mut deps = BTreeSet::new();
    let Ok(raw) = std::fs::read_to_string(project_root.join("composer.json")) else {
        return deps;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return deps;
    };
    for section in ["require", "require-dev"] {
        if let Some(serde_json::Value::Object(map)) = value.get(section) {
            deps.extend(map.keys().filter(|k| k.contains('/')).cloned());
        }
    }
    deps
}

/// `Some(value)` when the package opts in as a donor: `extra.skills` is an
/// object carrying a `source` key (whatever its value — validation is the
/// next step). `None` covers "no extra.skills at all" and the rootlike
/// shape (`extra.skills` without `source`), both non-donors.
fn declared_skills_source(extra: Option<&serde_json::Value>) -> Option<&serde_json::Value> {
    extra?.get("skills")?.as_object()?.get("source")
}

/// Validate `extra.skills.source` (SPEC §6.1); returns the normalized
/// `/`-separated relative path or the malformed-donor reason.
fn validate_source(source: &serde_json::Value) -> Result<String, String> {
    let value = source
        .as_str()
        .ok_or("extra.skills.source must be a non-empty string")?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("extra.skills.source must be a non-empty string".to_string());
    }
    if is_absolute_like(trimmed) {
        return Err("extra.skills.source must be a relative path".to_string());
    }
    normalize_rel(trimmed)
        .map_err(|_| "extra.skills.source must not escape the package root".to_string())
}

/// Lockfile origin path: relative to the project root (machine-independent)
/// when the package sits inside it, the raw path otherwise (path-repository
/// installs pointing outside the project).
fn origin_path(project_root: &Path, package_root: &Path) -> String {
    let path = match package_root.strip_prefix(project_root) {
        Ok(rel) => rel.to_path_buf(),
        Err(_) => package_root.to_path_buf(),
    };
    path.to_string_lossy().replace('\\', "/")
}

/// A single composer donor. Already on disk; materialization only verifies
/// the directory still exists.
pub struct ComposerVendor {
    name: VendorName,
    origin: Origin,
    root: PathBuf,
    source_hint: SourceHint,
}

#[async_trait]
impl Vendor for ComposerVendor {
    fn name(&self) -> &VendorName {
        &self.name
    }

    fn origin(&self) -> &Origin {
        &self.origin
    }

    async fn materialize(&self, _cache: &Cache) -> Result<MaterializedVendor, MaterializeError> {
        let root = self.root.clone();
        let exists = tokio::task::spawn_blocking(move || root.is_dir())
            .await
            .map_err(|e| MaterializeError::Task(e.to_string()))?;
        if !exists {
            return Err(MaterializeError::Vendor {
                vendor: self.name.clone(),
                message: format!("package directory disappeared: {}", self.root.display()),
            });
        }
        Ok(MaterializedVendor {
            name: self.name.clone(),
            origin: self.origin.clone(),
            root: self.root.clone(),
            ref_resolved: None,
            filter: SkillsFilter::All,
            source_hint: self.source_hint.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skills_core::manifest::MANIFEST_NAME;
    use skills_core::pipeline::ctx::{PrepareOptions, prepare};

    fn write(root: &Path, rel: &str, content: &str) {
        let path = rel.split('/').fold(root.to_path_buf(), |d, s| d.join(s));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn ctx(root: &Path) -> Ctx {
        if !root.join(MANIFEST_NAME).is_file() {
            std::fs::write(root.join(MANIFEST_NAME), "{}").unwrap();
        }
        prepare(root, PrepareOptions::default()).unwrap()
    }

    fn package_json(name: &str, extra_skills: Option<&str>) -> String {
        match extra_skills {
            None => format!(r#"{{ "name": "{name}" }}"#),
            Some(skills) => {
                format!(r#"{{ "name": "{name}", "extra": {{ "skills": {skills} }} }}"#)
            }
        }
    }

    /// Wrapped installed.json referencing packages by install-path.
    fn write_installed_wrapped(root: &Path, entries: &[serde_json::Value]) {
        let doc = serde_json::json!({ "packages": entries, "dev": true });
        write(
            root,
            "vendor/composer/installed.json",
            &serde_json::to_string_pretty(&doc).unwrap(),
        );
    }

    fn entry(name: &str, extra_skills: Option<serde_json::Value>) -> serde_json::Value {
        let mut e = serde_json::json!({
            "name": name,
            "install-path": format!("../{name}"),
        });
        if let Some(skills) = extra_skills {
            e["extra"] = serde_json::json!({ "skills": skills });
        }
        e
    }

    #[tokio::test]
    async fn wrapped_installed_json_yields_declared_donors() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "vendor/acme/basic/skills/greeting/SKILL.md",
            "x",
        );
        write_installed_wrapped(
            tmp.path(),
            &[entry(
                "acme/basic",
                Some(serde_json::json!({ "source": "skills" })),
            )],
        );
        let refs = ComposerProvider.discover(&ctx(tmp.path())).await.unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name.as_str(), "acme/basic");
        assert_eq!(refs[0].provider, ProviderId::Composer);
        assert_eq!(refs[0].status, DonorStatus::Declared);
        assert_eq!(refs[0].trust, TrustBasis::Transitive);
        assert_eq!(
            refs[0].origin,
            Origin::Local {
                path: "vendor/acme/basic".to_string()
            }
        );

        let cache = Cache::new(tmp.path().join(".skills-cache"));
        let mv = refs[0].vendor.materialize(&cache).await.unwrap();
        assert_eq!(
            mv.root,
            tmp.path().join("vendor").join("acme").join("basic")
        );
        assert_eq!(mv.source_hint, SourceHint::Declared("skills".into()));
    }

    #[tokio::test]
    async fn bare_array_installed_json_is_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "vendor/acme/basic/skills/one/SKILL.md", "x");
        let doc = serde_json::Value::Array(vec![entry(
            "acme/basic",
            Some(serde_json::json!({ "source": "skills" })),
        )]);
        write(
            tmp.path(),
            "vendor/composer/installed.json",
            &doc.to_string(),
        );
        let refs = ComposerProvider.discover(&ctx(tmp.path())).await.unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].status, DonorStatus::Declared);
    }

    #[tokio::test]
    async fn missing_install_path_defaults_to_vendor_name_dir() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "vendor/acme/basic/skills/one/SKILL.md", "x");
        let doc = serde_json::json!({ "packages": [ {
            "name": "acme/basic",
            "extra": { "skills": { "source": "skills" } }
        } ] });
        write(
            tmp.path(),
            "vendor/composer/installed.json",
            &doc.to_string(),
        );
        let refs = ComposerProvider.discover(&ctx(tmp.path())).await.unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(
            refs[0].origin,
            Origin::Local {
                path: "vendor/acme/basic".to_string()
            }
        );
    }

    #[tokio::test]
    async fn dir_scan_fallback_without_installed_json() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "vendor/acme/basic/composer.json",
            &package_json("acme/basic", Some(r#"{ "source": "skills" }"#)),
        );
        write(tmp.path(), "vendor/acme/basic/skills/one/SKILL.md", "x");
        // vendor/composer and vendor/bin are infrastructure, not packages.
        write(tmp.path(), "vendor/composer/ClassLoader.php", "<?php");
        write(tmp.path(), "vendor/bin/tool", "#!/bin/sh");
        // A dir without composer.json is not a package.
        std::fs::create_dir_all(tmp.path().join("vendor").join("acme").join("empty")).unwrap();

        let refs = ComposerProvider.discover(&ctx(tmp.path())).await.unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name.as_str(), "acme/basic");
        assert_eq!(refs[0].status, DonorStatus::Declared);
    }

    #[tokio::test]
    async fn no_vendor_dir_yields_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(
            ComposerProvider
                .discover(&ctx(tmp.path()))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn composer_disabled_disables_the_provider() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "vendor/acme/basic/composer.json",
            &package_json("acme/basic", Some(r#"{ "source": "skills" }"#)),
        );
        write(tmp.path(), "vendor/acme/basic/skills/one/SKILL.md", "x");
        std::fs::write(
            tmp.path().join(MANIFEST_NAME),
            r#"{ "dependencies": { "composer": false } }"#,
        )
        .unwrap();
        assert!(
            ComposerProvider
                .discover(&ctx(tmp.path()))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn direct_dependencies_are_tagged() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("composer.json"),
            r#"{ "require": { "acme/direct": "^1", "php": ">=8.1" },
                 "require-dev": { "acme/dev-dep": "@dev" } }"#,
        )
        .unwrap();
        write_installed_wrapped(
            tmp.path(),
            &[
                entry(
                    "acme/direct",
                    Some(serde_json::json!({ "source": "skills" })),
                ),
                entry(
                    "acme/dev-dep",
                    Some(serde_json::json!({ "source": "skills" })),
                ),
                entry(
                    "acme/transitive",
                    Some(serde_json::json!({ "source": "skills" })),
                ),
            ],
        );
        for pkg in ["direct", "dev-dep", "transitive"] {
            write(
                tmp.path(),
                &format!("vendor/acme/{pkg}/skills/s-{pkg}/SKILL.md"),
                "x",
            );
        }
        let refs = ComposerProvider.discover(&ctx(tmp.path())).await.unwrap();
        let by_name: Vec<(&str, TrustBasis)> =
            refs.iter().map(|r| (r.name.as_str(), r.trust)).collect();
        assert_eq!(
            by_name,
            [
                ("acme/dev-dep", TrustBasis::DirectDependency),
                ("acme/direct", TrustBasis::DirectDependency),
                ("acme/transitive", TrustBasis::Transitive),
            ]
        );
    }

    #[tokio::test]
    async fn malformed_sources_carry_their_reason() {
        let tmp = tempfile::tempdir().unwrap();
        write_installed_wrapped(
            tmp.path(),
            &[
                entry("acme/empty-src", Some(serde_json::json!({ "source": "" }))),
                entry(
                    "acme/abs-src",
                    Some(serde_json::json!({ "source": "/abs" })),
                ),
                entry(
                    "acme/escaping",
                    Some(serde_json::json!({ "source": "../out" })),
                ),
                entry("acme/non-string", Some(serde_json::json!({ "source": 5 }))),
            ],
        );
        for pkg in ["empty-src", "abs-src", "escaping", "non-string"] {
            std::fs::create_dir_all(tmp.path().join("vendor").join("acme").join(pkg)).unwrap();
        }
        let refs = ComposerProvider.discover(&ctx(tmp.path())).await.unwrap();
        let reasons: Vec<(&str, String)> = refs
            .iter()
            .map(|r| {
                let DonorStatus::Malformed { reason } = &r.status else {
                    panic!("{} must be malformed", r.name);
                };
                (r.name.as_str(), reason.clone())
            })
            .collect();
        assert_eq!(
            reasons,
            [
                (
                    "acme/abs-src",
                    "extra.skills.source must be a relative path".to_string()
                ),
                (
                    "acme/empty-src",
                    "extra.skills.source must be a non-empty string".to_string()
                ),
                (
                    "acme/escaping",
                    "extra.skills.source must not escape the package root".to_string()
                ),
                (
                    "acme/non-string",
                    "extra.skills.source must be a non-empty string".to_string()
                ),
            ]
        );
    }

    #[tokio::test]
    async fn rootlike_extra_without_source_is_invisible() {
        let tmp = tempfile::tempdir().unwrap();
        write_installed_wrapped(
            tmp.path(),
            &[entry(
                "acme/rootlike",
                Some(serde_json::json!({ "aliases": [".claude/skills"], "auto-sync": true })),
            )],
        );
        std::fs::create_dir_all(tmp.path().join("vendor").join("acme").join("rootlike")).unwrap();
        assert!(
            ComposerProvider
                .discover(&ctx(tmp.path()))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn undeclared_package_with_skills_is_a_discovery_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        write_installed_wrapped(
            tmp.path(),
            &[
                entry("acme/undeclared", None),
                entry("acme/plain-lib", None),
            ],
        );
        write(
            tmp.path(),
            "vendor/acme/undeclared/skills/auto-skill/SKILL.md",
            "x",
        );
        write(tmp.path(), "vendor/acme/plain-lib/src/Lib.php", "<?php");
        let refs = ComposerProvider.discover(&ctx(tmp.path())).await.unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name.as_str(), "acme/undeclared");
        assert_eq!(refs[0].status, DonorStatus::Undeclared);
        let cache = Cache::new(tmp.path().join(".skills-cache"));
        let mv = refs[0].vendor.materialize(&cache).await.unwrap();
        assert_eq!(mv.source_hint, SourceHint::Discovery);
    }

    #[tokio::test]
    async fn malformed_installed_json_is_a_provider_error() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "vendor/composer/installed.json", "{ nope");
        let err = ComposerProvider
            .discover(&ctx(tmp.path()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("invalid JSON"), "{err}");

        write(tmp.path(), "vendor/composer/installed.json", r#"{"a":1}"#);
        let err = ComposerProvider
            .discover(&ctx(tmp.path()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("packages"), "{err}");
    }

    #[test]
    fn install_path_resolution_is_lexical() {
        let base = Path::new("proj").join("vendor").join("composer");
        assert_eq!(
            resolve_install_path(&base, "../acme/basic"),
            Path::new("proj").join("vendor").join("acme").join("basic")
        );
        assert_eq!(
            resolve_install_path(&base, "./dir"),
            Path::new("proj")
                .join("vendor")
                .join("composer")
                .join("dir")
        );
        assert_eq!(
            resolve_install_path(&base, "C:\\elsewhere\\pkg"),
            PathBuf::from("C:\\elsewhere\\pkg")
        );
    }
}
