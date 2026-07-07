//! `DirProvider` — donors declared as `sources[]` entries with
//! `from: "dir"`.
//!
//! Each entry's `path` (relative to the project root, confined to it) is one
//! vendor whose skills root is the directory itself: every immediate
//! subdirectory containing `SKILL.md` is a skill. Vendor name: the entry's
//! `package` override if set, else `dir/<dirname>`. Materialization is a
//! no-op (Local origin).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use skills_core::domain::{
    DonorStatus, MaterializedVendor, Origin, ProviderId, SkillsFilter, SourceHint, TrustBasis,
    VendorName, VendorRef,
};
use skills_core::error::{DiscoverError, MaterializeError};
use skills_core::paths::join_declared;
use skills_core::pipeline::ctx::Ctx;
use skills_core::traits::{Cache, Vendor, VendorProvider};

pub struct DirProvider;

#[async_trait]
impl VendorProvider for DirProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Dir
    }

    async fn discover(&self, ctx: &Ctx) -> Result<Vec<VendorRef>, DiscoverError> {
        let mut refs: Vec<VendorRef> = Vec::new();
        for entry in ctx.manifest.sources().iter().filter(|e| e.from == "dir") {
            // Manifest validation guarantees `path` is present for dir
            // sources; default to "" defensively.
            let declared = entry.path.clone().unwrap_or_default();
            let root = join_declared(&ctx.project_root, &declared);
            if !root.is_dir() {
                return Err(DiscoverError::Provider {
                    provider: self.id(),
                    message: format!("sources dir path does not exist: '{declared}'"),
                });
            }
            // Defense in depth: manifest validation already rejected lexical
            // escapes (absolute paths, `..`). Canonicalize both sides and
            // confirm containment to catch symlink/junction escapes that
            // lexical normalization cannot see.
            ensure_within_root(&ctx.project_root, &root, &declared).map_err(|message| {
                DiscoverError::Provider {
                    provider: self.id(),
                    message,
                }
            })?;

            let name = match &entry.package {
                Some(package) => VendorName::new(package),
                None => {
                    let Some(dir_name) = root.file_name() else {
                        return Err(DiscoverError::Provider {
                            provider: self.id(),
                            message: format!(
                                "sources dir path has no directory name: '{declared}'"
                            ),
                        });
                    };
                    VendorName::new(format!("dir/{}", dir_name.to_string_lossy()))
                }
            };
            if refs.iter().any(|r| r.name == name) {
                return Err(DiscoverError::Provider {
                    provider: self.id(),
                    message: format!(
                        "two sources dir entries resolve to the same vendor name '{name}'"
                    ),
                });
            }
            let origin = Origin::Local {
                path: declared.clone(),
            };
            let filter = SkillsFilter::from_manifest(entry.skills.clone());
            refs.push(VendorRef {
                provider: self.id(),
                name: name.clone(),
                origin: origin.clone(),
                filter: filter.clone(),
                // Declared by the user in skills.json — implicitly trusted.
                trust: TrustBasis::UserDeclared,
                status: DonorStatus::Declared,
                vendor: Arc::new(DirVendor {
                    name,
                    origin,
                    root,
                    filter,
                }),
            });
        }
        Ok(refs)
    }
}

/// Canonicalize `dir` and `project_root` and confirm the directory sits
/// inside the project root. On Windows both sides carry the `\\?\` verbatim
/// prefix, so the `starts_with` comparison stays consistent.
fn ensure_within_root(project_root: &Path, dir: &Path, declared: &str) -> Result<(), String> {
    let canonical_root = std::fs::canonicalize(project_root)
        .map_err(|e| format!("cannot resolve the project root: {e}"))?;
    let canonical_dir = std::fs::canonicalize(dir)
        .map_err(|e| format!("sources dir path cannot be resolved: '{declared}': {e}"))?;
    if canonical_dir.starts_with(&canonical_root) {
        Ok(())
    } else {
        Err(format!(
            "sources dir path escapes the project root: '{declared}'"
        ))
    }
}

/// A single `dir` source donor. Already on disk, so materialization only
/// verifies the directory still exists.
pub struct DirVendor {
    name: VendorName,
    origin: Origin,
    root: PathBuf,
    filter: SkillsFilter,
}

impl DirVendor {
    pub fn new(name: VendorName, declared: String, root: PathBuf, filter: SkillsFilter) -> Self {
        DirVendor {
            name,
            origin: Origin::Local { path: declared },
            root,
            filter,
        }
    }
}

#[async_trait]
impl Vendor for DirVendor {
    fn name(&self) -> &VendorName {
        &self.name
    }

    fn origin(&self) -> &Origin {
        &self.origin
    }

    async fn materialize(&self, _cache: &Cache) -> Result<MaterializedVendor, MaterializeError> {
        let root = self.root.clone();
        let name = self.name.clone();
        let exists = tokio::task::spawn_blocking(move || root.is_dir())
            .await
            .map_err(|e| MaterializeError::Task(e.to_string()))?;
        if !exists {
            return Err(MaterializeError::Vendor {
                vendor: name,
                message: format!("directory disappeared: {}", self.root.display()),
            });
        }
        Ok(MaterializedVendor {
            name: self.name.clone(),
            origin: self.origin.clone(),
            root: self.root.clone(),
            ref_resolved: None,
            filter: self.filter.clone(),
            source_hint: SourceHint::ExplicitRoot,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skills_core::manifest::MANIFEST_NAME;
    use skills_core::pipeline::ctx::{PrepareOptions, prepare};

    fn project(manifest: &str) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(MANIFEST_NAME), manifest).unwrap();
        tmp
    }

    fn ctx(tmp: &tempfile::TempDir) -> Ctx {
        prepare(tmp.path(), PrepareOptions::default()).unwrap()
    }

    #[tokio::test]
    async fn discovers_one_vendor_per_dir_entry() {
        let tmp = project(
            r#"{ "sources": [
                { "from": "dir", "path": "./skills-src" },
                { "from": "dir", "path": "./more" }
            ] }"#,
        );
        std::fs::create_dir_all(tmp.path().join("skills-src")).unwrap();
        std::fs::create_dir_all(tmp.path().join("more")).unwrap();
        let refs = DirProvider.discover(&ctx(&tmp)).await.unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].name.as_str(), "dir/skills-src");
        assert_eq!(refs[1].name.as_str(), "dir/more");
        assert_eq!(
            refs[0].origin,
            Origin::Local {
                path: "./skills-src".to_string()
            }
        );
        assert_eq!(refs[0].filter, SkillsFilter::All);
    }

    #[tokio::test]
    async fn no_dir_sources_yields_no_vendors() {
        let tmp = project("{}");
        assert!(DirProvider.discover(&ctx(&tmp)).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn non_dir_sources_are_ignored() {
        // A legacy `remote` manifest with github entries yields no dir donors
        // and does not error.
        let tmp = project(r#"{ "remote": [ { "from": "github", "package": "a/b" } ] }"#);
        assert!(DirProvider.discover(&ctx(&tmp)).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn missing_dir_is_a_provider_error() {
        let tmp = project(r#"{ "sources": [ { "from": "dir", "path": "./nope" } ] }"#);
        let err = DirProvider.discover(&ctx(&tmp)).await.unwrap_err();
        assert!(err.to_string().contains("./nope"), "{err}");
        assert!(err.to_string().contains("does not exist"), "{err}");
    }

    #[tokio::test]
    async fn colliding_vendor_names_rejected() {
        let tmp = project(
            r#"{ "sources": [
                { "from": "dir", "path": "./a/skills" },
                { "from": "dir", "path": "./b/skills" }
            ] }"#,
        );
        std::fs::create_dir_all(tmp.path().join("a").join("skills")).unwrap();
        std::fs::create_dir_all(tmp.path().join("b").join("skills")).unwrap();
        let err = DirProvider.discover(&ctx(&tmp)).await.unwrap_err();
        assert!(err.to_string().contains("same vendor name"), "{err}");
    }

    #[tokio::test]
    async fn package_override_sets_the_vendor_name() {
        let tmp = project(
            r#"{ "sources": [
                { "from": "dir", "path": "./skills-src", "package": "acme/local" }
            ] }"#,
        );
        std::fs::create_dir_all(tmp.path().join("skills-src")).unwrap();
        let refs = DirProvider.discover(&ctx(&tmp)).await.unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name.as_str(), "acme/local");
    }

    #[tokio::test]
    async fn skills_allowlist_lands_in_the_filter() {
        let tmp = project(
            r#"{ "sources": [
                { "from": "dir", "path": "./skills-src", "skills": ["one", "two"] }
            ] }"#,
        );
        std::fs::create_dir_all(tmp.path().join("skills-src")).unwrap();
        let refs = DirProvider.discover(&ctx(&tmp)).await.unwrap();
        let expected = SkillsFilter::Only(vec!["one".to_string(), "two".to_string()]);
        // Filter lands on the VendorRef...
        assert_eq!(refs[0].filter, expected);
        // ...and threads through materialize onto the MaterializedVendor.
        let cache = Cache::new(tmp.path().join(".skills-cache"));
        let mv = refs[0].vendor.materialize(&cache).await.unwrap();
        assert_eq!(mv.filter, expected);
    }

    #[tokio::test]
    async fn materialize_is_noop_returning_the_dir() {
        let tmp = project(r#"{ "sources": [ { "from": "dir", "path": "./src" } ] }"#);
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        let refs = DirProvider.discover(&ctx(&tmp)).await.unwrap();
        let cache = Cache::new(tmp.path().join(".skills-cache"));
        let mv = refs[0].vendor.materialize(&cache).await.unwrap();
        assert_eq!(mv.root, tmp.path().join("src"));
        assert_eq!(mv.ref_resolved, None);
        assert_eq!(mv.filter, SkillsFilter::All);
        // No cache dir created for local vendors.
        assert!(!cache.root.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_escaping_the_root_is_rejected() {
        // A dir source that lexically stays inside the root but is a symlink
        // pointing outside must be rejected by the canonicalize check.
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(outside.path().join("secret")).unwrap();
        let tmp = project(r#"{ "sources": [ { "from": "dir", "path": "./link" } ] }"#);
        std::os::unix::fs::symlink(outside.path().join("secret"), tmp.path().join("link")).unwrap();
        let err = DirProvider.discover(&ctx(&tmp)).await.unwrap_err();
        assert!(
            err.to_string().contains("escapes the project root"),
            "{err}"
        );
    }
}
