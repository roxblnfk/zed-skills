//! `NpmProvider` — donors discovered from an installed npm tree
//! (`node_modules/`).
//!
//! Enumeration is a flat scan of the top-level `node_modules/` directory: a
//! `@scope` directory expands one level (`node_modules/@scope/<pkg>/`), every
//! other `node_modules/<pkg>/` carrying a `package.json` is a package. The
//! `.bin` directory, dot-directories, and any entry without a `package.json`
//! are skipped. No npm runtime and no lockfile parsing is involved.
//!
//! npm has no declared-donor contract (unlike composer's
//! `extra.skills.source`): every package is a potential *discovery* donor.
//! A package is admitted as [`DonorStatus::Undeclared`] only when it ships
//! discoverable skills; packages without them are silently invisible, like
//! composer's undeclared branch.
//!
//! Direct dependencies (root `package.json` `dependencies` /
//! `devDependencies` / `optionalDependencies`) are tagged
//! [`TrustBasis::DirectDependency`]; everything else is transitive and must
//! clear the trust list.
//!
//! Unlike composer, npm discovery is **disabled by default**: the provider
//! stays silent unless `dependencies.npm` opts in.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use skills_core::domain::{
    DonorStatus, MaterializedVendor, Origin, ProviderId, SkillsFilter, SourceHint, TrustBasis,
    VendorName, VendorRef,
};
use skills_core::error::{DiscoverError, MaterializeError};
use skills_core::pipeline::ctx::Ctx;
use skills_core::traits::{Cache, Vendor, VendorProvider};

use crate::treescan;

pub struct NpmProvider;

#[async_trait]
impl VendorProvider for NpmProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Npm
    }

    async fn discover(&self, ctx: &Ctx) -> Result<Vec<VendorRef>, DiscoverError> {
        if !ctx.manifest.npm_enabled() {
            return Ok(Vec::new());
        }
        let project_root = ctx.project_root.clone();
        let refs = tokio::task::spawn_blocking(move || discover_blocking(&project_root))
            .await
            .map_err(|e| DiscoverError::Provider {
                provider: ProviderId::Npm,
                message: format!("discover task panicked: {e}"),
            })?;
        Ok(refs)
    }
}

fn discover_blocking(project_root: &Path) -> Vec<VendorRef> {
    let mut packages = enumerate_packages(project_root);
    packages.sort_by(|a, b| a.name.cmp(&b.name));
    let direct = direct_dependencies(project_root);

    let mut refs = Vec::new();
    for package in packages {
        if !treescan::has_discoverable_skills(&package.root) {
            // No declared-donor contract in npm: a package with nothing
            // discoverable is silently invisible.
            continue;
        }
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
            provider: ProviderId::Npm,
            name: name.clone(),
            origin: origin.clone(),
            filter: SkillsFilter::All,
            trust,
            status: DonorStatus::Undeclared,
            vendor: Arc::new(NpmVendor {
                name,
                origin,
                root: package.root,
            }),
        });
    }
    refs
}

/// One installed npm package as far as this provider cares.
#[derive(Debug, Clone, PartialEq)]
struct InstalledPackage {
    name: String,
    /// Absolute install directory.
    root: PathBuf,
}

/// Enumerate installed packages by a flat scan of `node_modules/`. A `@scope`
/// directory expands one level; every other directory carrying a
/// `package.json` is a package. An absent `node_modules/` yields no packages —
/// the provider stays silent outside npm projects.
fn enumerate_packages(project_root: &Path) -> Vec<InstalledPackage> {
    let node_modules = project_root.join("node_modules");
    if !node_modules.is_dir() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for entry in sorted_subdirs(&node_modules) {
        let dir = dir_name(&entry);
        // `.bin` and other dot-directories are npm infrastructure, never
        // packages.
        if dir.starts_with('.') {
            continue;
        }
        if dir.starts_with('@') {
            // A scope directory expands exactly one level.
            for package_dir in sorted_subdirs(&entry) {
                let pkg = dir_name(&package_dir);
                if pkg.starts_with('.') {
                    continue;
                }
                push_if_package(&mut out, package_dir, format!("{dir}/{pkg}"));
            }
            continue;
        }
        push_if_package(&mut out, entry, dir);
    }
    out
}

/// Push a package if its directory carries a `package.json`. The name is the
/// `name` field of that manifest (authoritative); the directory-derived
/// `fallback_name` is used when the manifest is missing/unparseable/nameless.
fn push_if_package(out: &mut Vec<InstalledPackage>, root: PathBuf, fallback_name: String) {
    let manifest = root.join("package.json");
    if !manifest.is_file() {
        return;
    }
    let name = std::fs::read_to_string(&manifest)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(str::to_string))
        .filter(|n| !n.trim().is_empty())
        .unwrap_or(fallback_name);
    out.push(InstalledPackage { name, root });
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

/// Package names declared in the root `package.json` under `dependencies` /
/// `devDependencies` / `optionalDependencies` (`peerDependencies` excluded).
/// No `package.json` at all ⇒ empty set, so everything is transitive.
fn direct_dependencies(project_root: &Path) -> BTreeSet<String> {
    let mut deps = BTreeSet::new();
    let Ok(raw) = std::fs::read_to_string(project_root.join("package.json")) else {
        return deps;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return deps;
    };
    for section in ["dependencies", "devDependencies", "optionalDependencies"] {
        if let Some(serde_json::Value::Object(map)) = value.get(section) {
            deps.extend(map.keys().cloned());
        }
    }
    deps
}

/// Lockfile origin path: relative to the project root (machine-independent),
/// `/`-separated. Packages always live under `node_modules/` inside the
/// project, so the strip succeeds; the raw path is a defensive fallback.
fn origin_path(project_root: &Path, package_root: &Path) -> String {
    let path = match package_root.strip_prefix(project_root) {
        Ok(rel) => rel.to_path_buf(),
        Err(_) => package_root.to_path_buf(),
    };
    path.to_string_lossy().replace('\\', "/")
}

/// A single npm donor. Already on disk; materialization only verifies the
/// directory still exists. npm donors are always discovery-routed.
pub struct NpmVendor {
    name: VendorName,
    origin: Origin,
    root: PathBuf,
}

#[async_trait]
impl Vendor for NpmVendor {
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
            source_hint: SourceHint::Discovery,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::locate::{RecursiveFallbackLocator, WellKnownLocator};
    use crate::testkit::{ContractExpectations, run_vendor_contract};
    use skills_core::manifest::MANIFEST_NAME;
    use skills_core::pipeline::ctx::{PrepareOptions, prepare};
    use skills_core::traits::SkillLocator;

    fn write(root: &Path, rel: &str, content: &str) {
        let path = rel.split('/').fold(root.to_path_buf(), |d, s| d.join(s));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// Write `node_modules/<rel_dir>/package.json` with the given name.
    fn write_pkg(root: &Path, rel_dir: &str, name: &str) {
        write(
            root,
            &format!("node_modules/{rel_dir}/package.json"),
            &format!(r#"{{ "name": "{name}" }}"#),
        );
    }

    /// A discoverable skill under `node_modules/<rel_dir>/skills/<id>/`.
    fn write_skill(root: &Path, rel_dir: &str, id: &str) {
        write(
            root,
            &format!("node_modules/{rel_dir}/skills/{id}/SKILL.md"),
            "x",
        );
    }

    fn ctx_with(root: &Path, manifest: &str) -> Ctx {
        std::fs::write(root.join(MANIFEST_NAME), manifest).unwrap();
        prepare(root, PrepareOptions::default()).unwrap()
    }

    /// A ctx with npm discovery enabled (npm is disabled by default).
    fn ctx(root: &Path) -> Ctx {
        ctx_with(root, r#"{ "dependencies": { "npm": true } }"#)
    }

    #[tokio::test]
    async fn scoped_and_unscoped_packages_discovered() {
        let tmp = tempfile::tempdir().unwrap();
        write_pkg(tmp.path(), "lodash", "lodash");
        write_skill(tmp.path(), "lodash", "greeting");
        write_pkg(tmp.path(), "@myorg/thing", "@myorg/thing");
        write_skill(tmp.path(), "@myorg/thing", "hello");

        let refs = NpmProvider.discover(&ctx(tmp.path())).await.unwrap();
        // Sorted by name: "@myorg/thing" < "lodash".
        let names: Vec<&str> = refs.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["@myorg/thing", "lodash"]);
        assert!(refs.iter().all(|r| r.provider == ProviderId::Npm));
        assert!(refs.iter().all(|r| r.status == DonorStatus::Undeclared));
        assert!(refs.iter().all(|r| r.filter == SkillsFilter::All));
        assert_eq!(
            refs[0].origin,
            Origin::Local {
                path: "node_modules/@myorg/thing".to_string()
            }
        );
        assert_eq!(
            refs[1].origin,
            Origin::Local {
                path: "node_modules/lodash".to_string()
            }
        );
    }

    #[tokio::test]
    async fn scope_dir_expands_only_one_level() {
        // `node_modules/@scope/<pkg>` is a package; a deeper nested dir is
        // not enumerated.
        let tmp = tempfile::tempdir().unwrap();
        write_pkg(tmp.path(), "@scope/a", "@scope/a");
        write_skill(tmp.path(), "@scope/a", "one");
        write_pkg(tmp.path(), "@scope/b", "@scope/b");
        write_skill(tmp.path(), "@scope/b", "two");
        // A deeper package that must NOT be seen (v1 is flat top-level only).
        write_pkg(tmp.path(), "@scope/a/nested", "@scope/deeper");
        write_skill(tmp.path(), "@scope/a/nested", "hidden");

        let refs = NpmProvider.discover(&ctx(tmp.path())).await.unwrap();
        let names: Vec<&str> = refs.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["@scope/a", "@scope/b"]);
    }

    #[tokio::test]
    async fn direct_vs_transitive_tagging() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{
                "dependencies": { "prod-dep": "^1" },
                "devDependencies": { "dev-dep": "^2" },
                "optionalDependencies": { "opt-dep": "^3" },
                "peerDependencies": { "peer-dep": "^4" }
            }"#,
        )
        .unwrap();
        for pkg in ["prod-dep", "dev-dep", "opt-dep", "peer-dep", "transitive"] {
            write_pkg(tmp.path(), pkg, pkg);
            write_skill(tmp.path(), pkg, &format!("s-{pkg}"));
        }
        let refs = NpmProvider.discover(&ctx(tmp.path())).await.unwrap();
        let by_name: Vec<(&str, TrustBasis)> =
            refs.iter().map(|r| (r.name.as_str(), r.trust)).collect();
        assert_eq!(
            by_name,
            [
                ("dev-dep", TrustBasis::DirectDependency),
                ("opt-dep", TrustBasis::DirectDependency),
                // peerDependencies are NOT direct.
                ("peer-dep", TrustBasis::Transitive),
                ("prod-dep", TrustBasis::DirectDependency),
                ("transitive", TrustBasis::Transitive),
            ]
        );
    }

    #[tokio::test]
    async fn package_without_skills_is_invisible() {
        let tmp = tempfile::tempdir().unwrap();
        write_pkg(tmp.path(), "with-skills", "with-skills");
        write_skill(tmp.path(), "with-skills", "yes");
        // A plain library with a package.json but nothing discoverable.
        write_pkg(tmp.path(), "plain-lib", "plain-lib");
        write(tmp.path(), "node_modules/plain-lib/index.js", "//");
        // A directory without a package.json is not a package at all.
        std::fs::create_dir_all(tmp.path().join("node_modules").join("bare-dir")).unwrap();

        let refs = NpmProvider.discover(&ctx(tmp.path())).await.unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name.as_str(), "with-skills");
    }

    #[tokio::test]
    async fn missing_node_modules_yields_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(
            NpmProvider
                .discover(&ctx(tmp.path()))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn npm_disabled_by_default() {
        // No dependencies block ⇒ npm off ⇒ nothing discovered even with a
        // skill-shipping package present.
        let tmp = tempfile::tempdir().unwrap();
        write_pkg(tmp.path(), "lodash", "lodash");
        write_skill(tmp.path(), "lodash", "greeting");
        let ctx = ctx_with(tmp.path(), "{}");
        assert!(NpmProvider.discover(&ctx).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn npm_disabled_via_bool_and_object() {
        let tmp = tempfile::tempdir().unwrap();
        write_pkg(tmp.path(), "lodash", "lodash");
        write_skill(tmp.path(), "lodash", "greeting");

        let ctx = ctx_with(tmp.path(), r#"{ "dependencies": { "npm": false } }"#);
        assert!(NpmProvider.discover(&ctx).await.unwrap().is_empty());

        let ctx = ctx_with(
            tmp.path(),
            r#"{ "dependencies": { "npm": { "enabled": false } } }"#,
        );
        assert!(NpmProvider.discover(&ctx).await.unwrap().is_empty());

        // An object enabling npm turns it on.
        let ctx = ctx_with(
            tmp.path(),
            r#"{ "dependencies": { "npm": { "enabled": true } } }"#,
        );
        assert_eq!(NpmProvider.discover(&ctx).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn name_comes_from_package_json_not_dir() {
        // Directory name differs from the declared `name`; the manifest wins.
        let tmp = tempfile::tempdir().unwrap();
        write_pkg(tmp.path(), "dir-name", "@declared/actual-name");
        write_skill(tmp.path(), "dir-name", "one");
        let refs = NpmProvider.discover(&ctx(tmp.path())).await.unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name.as_str(), "@declared/actual-name");
        // Origin still reflects the on-disk directory, relative + `/`-joined.
        assert_eq!(
            refs[0].origin,
            Origin::Local {
                path: "node_modules/dir-name".to_string()
            }
        );
    }

    #[tokio::test]
    async fn nameless_package_json_falls_back_to_dir_name() {
        let tmp = tempfile::tempdir().unwrap();
        // package.json present but without a usable `name`.
        write(
            tmp.path(),
            "node_modules/@scope/pkg/package.json",
            r#"{ "version": "1.0.0" }"#,
        );
        write_skill(tmp.path(), "@scope/pkg", "one");
        let refs = NpmProvider.discover(&ctx(tmp.path())).await.unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name.as_str(), "@scope/pkg");
    }

    #[tokio::test]
    async fn materialize_is_noop_returning_the_dir() {
        let tmp = tempfile::tempdir().unwrap();
        write_pkg(tmp.path(), "lodash", "lodash");
        write_skill(tmp.path(), "lodash", "one");
        let refs = NpmProvider.discover(&ctx(tmp.path())).await.unwrap();
        let cache = Cache::new(tmp.path().join(".skills-cache"));
        let mv = refs[0].vendor.materialize(&cache).await.unwrap();
        assert_eq!(mv.root, tmp.path().join("node_modules").join("lodash"));
        assert_eq!(mv.ref_resolved, None);
        assert_eq!(mv.filter, SkillsFilter::All);
        assert_eq!(mv.source_hint, SourceHint::Discovery);
        // No cache dir created for local vendors.
        assert!(!cache.root.exists());
    }

    #[tokio::test]
    async fn dot_dirs_and_bin_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        // `.bin` and other dot-dirs must never be treated as packages, even
        // if they somehow carry a package.json + skills.
        write(
            tmp.path(),
            "node_modules/.bin/package.json",
            r#"{ "name": ".bin" }"#,
        );
        write_skill(tmp.path(), ".bin", "nope");
        write(
            tmp.path(),
            "node_modules/.cache/package.json",
            r#"{ "name": ".cache" }"#,
        );
        write_skill(tmp.path(), ".cache", "nope");
        write_pkg(tmp.path(), "real", "real");
        write_skill(tmp.path(), "real", "yes");
        let refs = NpmProvider.discover(&ctx(tmp.path())).await.unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name.as_str(), "real");
    }

    #[tokio::test]
    async fn satisfies_the_vendor_contract() {
        let tmp = tempfile::tempdir().unwrap();
        write_pkg(tmp.path(), "@myorg/thing", "@myorg/thing");
        write_skill(tmp.path(), "@myorg/thing", "greeting");
        write_skill(tmp.path(), "@myorg/thing", "farewell");
        let refs = NpmProvider.discover(&ctx(tmp.path())).await.unwrap();
        assert_eq!(refs.len(), 1);

        let cache = Cache::new(tmp.path().join(".skills-cache"));
        let locators: Vec<Arc<dyn SkillLocator>> = vec![
            Arc::new(WellKnownLocator),
            Arc::new(RecursiveFallbackLocator::new()),
        ];
        run_vendor_contract(
            refs[0].vendor.as_ref(),
            locators,
            &cache,
            &ContractExpectations {
                skill_ids: vec!["farewell".to_string(), "greeting".to_string()],
            },
        )
        .await;
    }
}
