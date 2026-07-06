//! `DirProvider` — donors declared as plain directories via
//! `local.dir: ["path"]` in the manifest.
//!
//! Each declared path is one vendor whose skills root is the directory
//! itself: every immediate subdirectory containing `SKILL.md` is a skill.
//! Vendor name: `dir/<dirname>`. Materialization is a no-op (Local origin).

use std::path::PathBuf;
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
        for declared in ctx.manifest.local_dirs() {
            let root = join_declared(&ctx.project_root, declared);
            if !root.is_dir() {
                return Err(DiscoverError::Provider {
                    provider: self.id(),
                    message: format!("local.dir path does not exist: '{declared}'"),
                });
            }
            let Some(dir_name) = root.file_name() else {
                return Err(DiscoverError::Provider {
                    provider: self.id(),
                    message: format!("local.dir path has no directory name: '{declared}'"),
                });
            };
            let name = VendorName::new(format!("dir/{}", dir_name.to_string_lossy()));
            if refs.iter().any(|r| r.name == name) {
                return Err(DiscoverError::Provider {
                    provider: self.id(),
                    message: format!(
                        "two local.dir entries resolve to the same vendor name '{name}'"
                    ),
                });
            }
            let origin = Origin::Local {
                path: declared.clone(),
            };
            refs.push(VendorRef {
                provider: self.id(),
                name: name.clone(),
                origin: origin.clone(),
                // local.dir entries have no per-donor allowlist syntax.
                filter: SkillsFilter::All,
                // Declared by the user in skills.json — implicitly trusted.
                trust: TrustBasis::UserDeclared,
                status: DonorStatus::Declared,
                vendor: Arc::new(DirVendor { name, origin, root }),
            });
        }
        Ok(refs)
    }
}

/// A single `local.dir` donor. Already on disk, so materialization only
/// verifies the directory still exists.
pub struct DirVendor {
    name: VendorName,
    origin: Origin,
    root: PathBuf,
}

impl DirVendor {
    pub fn new(name: VendorName, declared: String, root: PathBuf) -> Self {
        DirVendor {
            name,
            origin: Origin::Local { path: declared },
            root,
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
            filter: SkillsFilter::All,
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
        let tmp = project(r#"{ "local": { "dir": ["./skills-src", "./more"] } }"#);
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
    async fn no_local_dir_yields_no_vendors() {
        let tmp = project("{}");
        assert!(DirProvider.discover(&ctx(&tmp)).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn missing_dir_is_a_provider_error() {
        let tmp = project(r#"{ "local": { "dir": ["./nope"] } }"#);
        let err = DirProvider.discover(&ctx(&tmp)).await.unwrap_err();
        assert!(err.to_string().contains("./nope"), "{err}");
    }

    #[tokio::test]
    async fn colliding_vendor_names_rejected() {
        let tmp = project(r#"{ "local": { "dir": ["./a/skills", "./b/skills"] } }"#);
        std::fs::create_dir_all(tmp.path().join("a").join("skills")).unwrap();
        std::fs::create_dir_all(tmp.path().join("b").join("skills")).unwrap();
        let err = DirProvider.discover(&ctx(&tmp)).await.unwrap_err();
        assert!(err.to_string().contains("same vendor name"), "{err}");
    }

    #[tokio::test]
    async fn materialize_is_noop_returning_the_dir() {
        let tmp = project(r#"{ "local": { "dir": ["./src"] } }"#);
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        let refs = DirProvider.discover(&ctx(&tmp)).await.unwrap();
        let cache = Cache::new(tmp.path().join(".skills-cache"));
        let mv = refs[0].vendor.materialize(&cache).await.unwrap();
        assert_eq!(mv.root, tmp.path().join("src"));
        assert_eq!(mv.ref_resolved, None);
        // No cache dir created for local vendors.
        assert!(!cache.root.exists());
    }
}
