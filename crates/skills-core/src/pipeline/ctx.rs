//! Prepare stage: load manifest + lockfile, resolve the target, set up the
//! run context.

use std::path::{Path, PathBuf};

use crate::error::PrepareError;
use crate::lockfile::{LOCKFILE_NAME, Lockfile};
use crate::manifest::{MANIFEST_NAME, Manifest};
use crate::paths::{normalize_rel, rel_to_path};
use crate::traits::Cache;

/// Directory (relative to the project root) used to cache remote vendor
/// content. Created lazily; unused by local providers.
pub const CACHE_DIR: &str = ".skills-cache";

/// Immutable context threaded through all pipeline stages.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub project_root: PathBuf,
    pub manifest: Manifest,
    pub lockfile: Lockfile,
    /// Normalized `/`-separated target, relative to the project root.
    pub target_rel: String,
    /// Absolute target directory.
    pub target_abs: PathBuf,
    pub cache: Cache,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default)]
pub struct PrepareOptions {
    /// CLI `--target` override; beats the manifest value.
    pub target_override: Option<String>,
    pub dry_run: bool,
    /// CLI `--refresh`: force re-download of cached remote archives.
    pub refresh: bool,
}

/// Stage 1 — Prepare.
pub fn prepare(project_root: &Path, options: PrepareOptions) -> Result<Ctx, PrepareError> {
    let manifest = Manifest::load(&project_root.join(MANIFEST_NAME))?;
    let lockfile = Lockfile::load(&project_root.join(LOCKFILE_NAME))?.unwrap_or_default();

    let target_rel = match &options.target_override {
        Some(t) => normalize_rel(t).map_err(PrepareError::InvalidTarget)?,
        None => manifest.effective_target(),
    };
    let target_abs = project_root.join(rel_to_path(&target_rel));

    Ok(Ctx {
        project_root: project_root.to_path_buf(),
        manifest,
        lockfile,
        target_rel,
        target_abs,
        cache: Cache {
            root: project_root.join(CACHE_DIR),
            refresh: options.refresh,
        },
        dry_run: options.dry_run,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(manifest_json: &str) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(MANIFEST_NAME), manifest_json).unwrap();
        tmp
    }

    #[test]
    fn prepare_defaults() {
        let tmp = project("{}");
        let ctx = prepare(tmp.path(), PrepareOptions::default()).unwrap();
        assert_eq!(ctx.target_rel, ".agents/skills");
        assert_eq!(ctx.target_abs, tmp.path().join(".agents").join("skills"));
        assert!(ctx.lockfile.skills.is_empty());
        assert!(!ctx.dry_run);
    }

    #[test]
    fn prepare_uses_manifest_target() {
        let tmp = project(r#"{ "target": "custom/skills" }"#);
        let ctx = prepare(tmp.path(), PrepareOptions::default()).unwrap();
        assert_eq!(ctx.target_rel, "custom/skills");
    }

    #[test]
    fn cli_target_beats_manifest() {
        let tmp = project(r#"{ "target": "custom/skills" }"#);
        let ctx = prepare(
            tmp.path(),
            PrepareOptions {
                target_override: Some("./override/here".to_string()),
                dry_run: true,
                refresh: false,
            },
        )
        .unwrap();
        assert_eq!(ctx.target_rel, "override/here");
        assert!(ctx.dry_run);
    }

    #[test]
    fn invalid_cli_target_rejected() {
        let tmp = project("{}");
        let err = prepare(
            tmp.path(),
            PrepareOptions {
                target_override: Some("../escape".to_string()),
                dry_run: false,
                refresh: false,
            },
        )
        .unwrap_err();
        assert!(matches!(err, PrepareError::InvalidTarget(_)));
    }

    #[test]
    fn missing_manifest_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let err = prepare(tmp.path(), PrepareOptions::default()).unwrap_err();
        assert!(matches!(err, PrepareError::Manifest(_)));
    }
}
