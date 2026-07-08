//! `DirProvider` — donors declared as `sources[]` entries with
//! `from: "dir"`.
//!
//! Each entry's `path` is one vendor whose skills root is the directory
//! itself: every immediate subdirectory containing `SKILL.md` is a skill.
//!
//! A `dir` donor is *read-only*, so its path may be relative, absolute, or
//! point OUTSIDE the project root (`..` segments are legal). The manifest
//! validates the shape lexically (non-empty, not the project root, and — for
//! in-root relative spellings — no overlap with the sync target/aliases).
//! This provider adds the canonical runtime **self-reference guard**: it
//! canonicalizes the resolved directory and rejects it when it turns out to
//! be the project root or to overlap the sync target — this catches
//! absolute/outward spellings of in-project paths and symlinks/junctions
//! that point back into the project.
//!
//! Vendor name: the entry's `package` override if set, else derived from the
//! DECLARED path so lockfile vendor names stay machine-independent — the
//! last two plain segments of the normalized declared path, lowercased
//! (`./local/skills-src` → `local/skills-src`, `C:/shared/skills` →
//! `shared/skills`); a single plain segment gets a `dir/` prefix
//! (`./skills-src` → `dir/skills-src`). Only inherently machine-specific
//! outward paths (`../shared`, `/skills`) fall back to the canonical FS
//! `<parent>/<basename>`. Materialization is a no-op (Local origin).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use skills_core::domain::{
    DonorStatus, MaterializedVendor, Origin, ProviderId, SkillsFilter, SourceHint, TrustBasis,
    VendorName, VendorRef,
};
use skills_core::error::{DiscoverError, MaterializeError};
use skills_core::paths::{join_declared, normalize_declared, rel_to_path};
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
            // Canonicalize the existing donor dir. A resolve failure on an
            // existing directory surfaces as a provider error.
            let canonical_dir =
                std::fs::canonicalize(&root).map_err(|e| DiscoverError::Provider {
                    provider: self.id(),
                    message: format!("sources dir path cannot be resolved: '{declared}': {e}"),
                })?;

            // Canonical self-reference guard: complements the manifest's
            // lexical overlap check by catching absolute/outward spellings of
            // in-project paths and symlinks/junctions pointing back inside.
            ensure_not_self_reference(ctx, &canonical_dir, &declared).map_err(|message| {
                DiscoverError::Provider {
                    provider: self.id(),
                    message,
                }
            })?;

            let name = match &entry.package {
                Some(package) => VendorName::new(package),
                None => {
                    let Some(derived) = vendor_name_from_dir(&declared, &canonical_dir) else {
                        return Err(DiscoverError::Provider {
                            provider: self.id(),
                            message: format!(
                                "sources dir path has no directory name: '{declared}'"
                            ),
                        });
                    };
                    VendorName::new(derived)
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
            // The lockfile stays machine-independent: Origin keeps the
            // DECLARED string, never the canonicalized absolute path.
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

/// Reject a donor dir that is the project root itself or overlaps the sync
/// target (a self-reference / sync loop). Both `dir` and the project root are
/// canonicalized (on Windows both carry the `\\?\` verbatim prefix, so the
/// component comparison stays consistent). The target may not exist yet, so
/// it is formed by joining its relative path onto the *canonical* root rather
/// than canonicalizing the (possibly absent) target directly. An alias is a
/// link to the target, so a dir path routed through an alias canonicalizes
/// into the target and is caught by this same overlap comparison.
fn ensure_not_self_reference(
    ctx: &Ctx,
    canonical_dir: &Path,
    declared: &str,
) -> Result<(), String> {
    let canonical_root = std::fs::canonicalize(&ctx.project_root)
        .map_err(|e| format!("cannot resolve the project root: {e}"))?;
    if canonical_dir == canonical_root {
        return Err(format!(
            "sources dir path must not be the project root: '{declared}'"
        ));
    }
    let target = canonical_root.join(rel_to_path(&ctx.target_rel));
    if canonical_dir.starts_with(&target) || target.starts_with(canonical_dir) {
        return Err(format!(
            "sources dir path '{declared}' overlaps the sync target '{}'",
            ctx.target_rel
        ));
    }
    Ok(())
}

/// Derive a vendor name for a dir donor. Vendor names land in `skills.lock`,
/// which must stay machine-independent, so the DECLARED path is the primary
/// source (deriving from the canonical path would bake in the machine's
/// checkout dir name for donors directly under the project root):
///
/// 1. normalize the declared path ([`normalize_declared`]);
/// 2. last two segments both plain (not `..`, not empty, not a drive
///    prefix) → `"{parent}/{basename}"` lowercased — covers in-root nested
///    paths and absolute paths deterministically from manifest text alone;
/// 3. a single plain segment → `"dir/{segment}"` lowercased;
/// 4. otherwise (second-to-last is `..` or a root/drive prefix — e.g.
///    `../shared`, `/skills` — inherently machine-specific outward shapes)
///    → fall back to the canonical FS `<parent>/<basename>` lowercased, or
///    `dir/<basename>` when the FS parent has no usable name. `None` only
///    when even the canonical path has no basename (a root), which the
///    caller reports as a provider error.
pub fn vendor_name_from_dir(declared: &str, canonical_dir: &Path) -> Option<String> {
    let norm = normalize_declared(declared);
    let segments: Vec<&str> = norm.split('/').collect();
    match segments.as_slice() {
        [.., parent, basename] if is_plain_segment(parent) && is_plain_segment(basename) => {
            Some(format!("{parent}/{basename}").to_lowercase())
        }
        [only] if is_plain_segment(only) => Some(format!("dir/{only}").to_lowercase()),
        _ => {
            let basename = canonical_dir.file_name()?.to_string_lossy().to_string();
            match canonical_dir.parent().and_then(Path::file_name) {
                Some(parent) => {
                    Some(format!("{}/{}", parent.to_string_lossy(), basename).to_lowercase())
                }
                None => Some(format!("dir/{basename}").to_lowercase()),
            }
        }
    }
}

/// A plain, nameable path segment: non-empty, not `..`, not a drive prefix
/// (`C:` — `normalize_declared` keeps the drive as the first segment).
fn is_plain_segment(seg: &str) -> bool {
    let b = seg.as_bytes();
    !seg.is_empty() && seg != ".." && !(b.len() == 2 && b[0].is_ascii_alphabetic() && b[1] == b':')
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

    /// Build a manifest JSON string with a single dir source at `path`
    /// (escaping is handled by serde so absolute Windows paths work).
    fn dir_manifest(path: &str) -> String {
        serde_json::json!({ "sources": [ { "from": "dir", "path": path } ] }).to_string()
    }

    fn ctx(tmp: &tempfile::TempDir) -> Ctx {
        prepare(tmp.path(), PrepareOptions::default()).unwrap()
    }

    #[tokio::test]
    async fn discovers_one_vendor_per_dir_entry() {
        // The name derives from the DECLARED path's last two segments —
        // stable and machine-independent, no canonicalization involved.
        let tmp = project(
            r#"{ "sources": [
                { "from": "dir", "path": "./acme/skills-src" },
                { "from": "dir", "path": "./acme/more" }
            ] }"#,
        );
        std::fs::create_dir_all(tmp.path().join("acme").join("skills-src")).unwrap();
        std::fs::create_dir_all(tmp.path().join("acme").join("more")).unwrap();
        let refs = DirProvider.discover(&ctx(&tmp)).await.unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].name.as_str(), "acme/skills-src");
        assert_eq!(refs[1].name.as_str(), "acme/more");
        assert_eq!(
            refs[0].origin,
            Origin::Local {
                path: "./acme/skills-src".to_string()
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
    async fn single_segment_declared_gets_dir_prefix() {
        // A one-segment declared path has no usable declared parent; the
        // stable machine-independent fallback is `dir/<segment>`.
        let tmp = project(r#"{ "sources": [ { "from": "dir", "path": "./skills-src" } ] }"#);
        std::fs::create_dir_all(tmp.path().join("skills-src")).unwrap();
        let refs = DirProvider.discover(&ctx(&tmp)).await.unwrap();
        assert_eq!(refs[0].name.as_str(), "dir/skills-src");
    }

    #[tokio::test]
    async fn derived_names_are_lowercased() {
        let tmp = project(r#"{ "sources": [ { "from": "dir", "path": "./Acme/Skills" } ] }"#);
        std::fs::create_dir_all(tmp.path().join("Acme").join("Skills")).unwrap();
        let refs = DirProvider.discover(&ctx(&tmp)).await.unwrap();
        assert_eq!(refs[0].name.as_str(), "acme/skills");
    }

    #[tokio::test]
    async fn distinct_parent_dirs_get_distinct_names() {
        // Formerly a collision (`dir/skills` twice); now `a/skills` vs
        // `b/skills` — a positive, non-colliding case.
        let tmp = project(
            r#"{ "sources": [
                { "from": "dir", "path": "./a/skills" },
                { "from": "dir", "path": "./b/skills" }
            ] }"#,
        );
        std::fs::create_dir_all(tmp.path().join("a").join("skills")).unwrap();
        std::fs::create_dir_all(tmp.path().join("b").join("skills")).unwrap();
        let refs = DirProvider.discover(&ctx(&tmp)).await.unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].name.as_str(), "a/skills");
        assert_eq!(refs[1].name.as_str(), "b/skills");
    }

    #[tokio::test]
    async fn colliding_vendor_names_rejected() {
        // A `package` override deliberately clashing with another entry's
        // derived `<parent>/<basename>` name is a genuine collision.
        let tmp = project(
            r#"{ "sources": [
                { "from": "dir", "path": "./foo/skills" },
                { "from": "dir", "path": "./other", "package": "foo/skills" }
            ] }"#,
        );
        std::fs::create_dir_all(tmp.path().join("foo").join("skills")).unwrap();
        std::fs::create_dir_all(tmp.path().join("other")).unwrap();
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

    #[tokio::test]
    async fn absolute_outward_path_accepted() {
        // A donor dir living entirely outside the project is a read — allowed.
        let outside = tempfile::tempdir().unwrap();
        let donor = outside.path().join("shared").join("skills");
        std::fs::create_dir_all(&donor).unwrap();
        let declared = donor.to_string_lossy().to_string();
        let tmp = project(&dir_manifest(&declared));
        let refs = DirProvider.discover(&ctx(&tmp)).await.unwrap();
        assert_eq!(refs.len(), 1);
        // Absolute paths name from the declared text's last two segments —
        // no filesystem canonicalization involved.
        assert_eq!(refs[0].name.as_str(), "shared/skills");
        // Origin keeps the DECLARED (absolute) string verbatim.
        assert_eq!(
            refs[0].origin,
            Origin::Local {
                path: declared.clone()
            }
        );
        // And it materializes without touching the cache.
        let cache = Cache::new(tmp.path().join(".skills-cache"));
        refs[0].vendor.materialize(&cache).await.unwrap();
    }

    #[tokio::test]
    async fn sibling_relative_path_accepted() {
        // Nest the project one level down so `../sibling` resolves to a real
        // dir outside the project root.
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("project");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::create_dir_all(tmp.path().join("sibling")).unwrap();
        std::fs::write(proj.join(MANIFEST_NAME), dir_manifest("../sibling")).unwrap();
        let ctx = prepare(&proj, PrepareOptions::default()).unwrap();
        let refs = DirProvider.discover(&ctx).await.unwrap();
        assert_eq!(refs.len(), 1);
        // `../sibling` has `..` as its second-to-last segment, so naming
        // falls back to the canonical FS `<parent>/<basename>` (the parent is
        // the tempdir's random name — compute the expectation).
        let canonical = std::fs::canonicalize(tmp.path().join("sibling")).unwrap();
        let expected = format!(
            "{}/sibling",
            canonical
                .parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
        )
        .to_lowercase();
        assert_eq!(refs[0].name.as_str(), expected);
        assert_eq!(
            refs[0].origin,
            Origin::Local {
                path: "../sibling".to_string()
            }
        );
    }

    #[tokio::test]
    async fn dir_equal_to_target_rejected() {
        // Absolute spelling of the target dir bypasses the lexical manifest
        // check; the canonical runtime guard must still reject it.
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join(".agents").join("skills");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(
            tmp.path().join(MANIFEST_NAME),
            dir_manifest(&target.to_string_lossy()),
        )
        .unwrap();
        let ctx = prepare(tmp.path(), PrepareOptions::default()).unwrap();
        let err = DirProvider.discover(&ctx).await.unwrap_err();
        assert!(
            err.to_string().contains("overlaps the sync target"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn dir_inside_target_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let inside = tmp.path().join(".agents").join("skills").join("sub");
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::write(
            tmp.path().join(MANIFEST_NAME),
            dir_manifest(&inside.to_string_lossy()),
        )
        .unwrap();
        let ctx = prepare(tmp.path(), PrepareOptions::default()).unwrap();
        let err = DirProvider.discover(&ctx).await.unwrap_err();
        assert!(
            err.to_string().contains("overlaps the sync target"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn target_inside_dir_rejected() {
        // The donor dir contains the (future) sync target.
        let tmp = tempfile::tempdir().unwrap();
        let agents = tmp.path().join(".agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(
            tmp.path().join(MANIFEST_NAME),
            dir_manifest(&agents.to_string_lossy()),
        )
        .unwrap();
        let ctx = prepare(tmp.path(), PrepareOptions::default()).unwrap();
        let err = DirProvider.discover(&ctx).await.unwrap_err();
        assert!(
            err.to_string().contains("overlaps the sync target"),
            "{err}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_pointing_outside_the_root_is_allowed() {
        // Outward reads are allowed: a symlink that resolves outside the
        // project must discover + materialize normally.
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(outside.path().join("secret")).unwrap();
        let tmp = project(r#"{ "sources": [ { "from": "dir", "path": "./link" } ] }"#);
        std::os::unix::fs::symlink(outside.path().join("secret"), tmp.path().join("link")).unwrap();
        let refs = DirProvider.discover(&ctx(&tmp)).await.unwrap();
        assert_eq!(refs.len(), 1);
        // Naming follows the DECLARED spelling (single segment → `dir/`
        // prefix), not the symlink's resolution — two declared spellings of
        // the same dir may name differently, which is fine: identical
        // normalized declared paths already dedupe at the manifest level.
        assert_eq!(refs[0].name.as_str(), "dir/link");
        let cache = Cache::new(tmp.path().join(".skills-cache"));
        refs[0].vendor.materialize(&cache).await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_pointing_at_the_target_is_rejected() {
        // A lexically in-root symlink that canonicalizes into the sync target
        // (the shape an alias link takes) must be caught by the guard.
        let tmp = project(r#"{ "sources": [ { "from": "dir", "path": "./link" } ] }"#);
        let target = tmp.path().join(".agents").join("skills");
        std::fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(&target, tmp.path().join("link")).unwrap();
        let err = DirProvider.discover(&ctx(&tmp)).await.unwrap_err();
        assert!(
            err.to_string().contains("overlaps the sync target"),
            "{err}"
        );
    }
}
