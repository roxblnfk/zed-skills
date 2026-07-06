//! Prepare stage: load manifest + lockfile, resolve the target, set up the
//! run context.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::error::PrepareError;
use crate::lockfile::{LOCKFILE_NAME, Lockfile};
use crate::manifest::{MANIFEST_NAME, Manifest};
use crate::paths::{normalize_rel, rel_to_path};
use crate::pattern::VendorPattern;
use crate::traits::Cache;

/// Directory (relative to the project root) used to cache remote vendor
/// content. Created lazily; unused by local providers.
pub const CACHE_DIR: &str = ".skills-cache";

/// Per-invocation options that shape the run (positional filters, trust
/// grants, discovery opt-in). Lives in [`Ctx`] so the TrustFilter stage can
/// consult it.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// Positional `PACKAGE` / `VENDOR/*` arguments: filter + implicit trust
    /// + per-package discovery grant. Empty = all donors.
    pub packages: Vec<VendorPattern>,
    /// `--trust=PATTERN` entries, added on top of project + builtin lists.
    pub trust: Vec<VendorPattern>,
    /// `--discovery` CLI override; `None` defers to the manifest flag.
    pub discovery: Option<bool>,
    /// A `--from=ID` provider scope is active: donors outside the scope keep
    /// their lockfile entries instead of being pruned.
    pub scoped: bool,
    /// `--re-audit`: bypass the lockfile verdict cache and re-run the audit
    /// chain (`--refresh` intentionally does not).
    pub re_audit: bool,
}

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
    /// Effective aliases (normalized, `/`-separated, relative to the project
    /// root). Each is mirrored to the target via a junction / symlink after
    /// the copy step. CLI `--alias` replaces the manifest list entirely.
    pub aliases: Vec<String>,
    pub cache: Cache,
    pub dry_run: bool,
    pub run: RunOptions,
}

impl Ctx {
    /// Effective discovery flag: CLI override beats the manifest value.
    pub fn discovery_enabled(&self) -> bool {
        self.run
            .discovery
            .unwrap_or_else(|| self.manifest.discovery.unwrap_or(false))
    }

    /// A partial run (positional filters or `--from` scope) never prunes
    /// lockfile entries of out-of-scope donors.
    pub fn partial_sync(&self) -> bool {
        self.run.scoped || !self.run.packages.is_empty()
    }
}

#[derive(Debug, Clone, Default)]
pub struct PrepareOptions {
    /// CLI `--target` override; beats the manifest value.
    pub target_override: Option<String>,
    /// CLI `--alias` override. `Some(_)` (even empty) replaces the manifest
    /// `aliases` entirely; `None` defers to the manifest.
    pub alias_override: Option<Vec<String>>,
    pub dry_run: bool,
    /// CLI `--refresh`: force re-download of cached remote archives.
    pub refresh: bool,
    /// Per-invocation filters and trust grants.
    pub run: RunOptions,
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

    let aliases = resolve_aliases(&manifest, &options.alias_override, &target_rel)?;

    Ok(Ctx {
        project_root: project_root.to_path_buf(),
        manifest,
        lockfile,
        target_rel,
        target_abs,
        aliases,
        cache: Cache {
            root: project_root.join(CACHE_DIR),
            refresh: options.refresh,
            offline: false,
        },
        dry_run: options.dry_run,
        run: options.run,
    })
}

/// Compute the effective, validated alias list. CLI `--alias` (`override`)
/// replaces the manifest `aliases` entirely; otherwise the manifest list is
/// used. Every alias is normalized, must stay inside the project root, and
/// must differ from the effective target and from every other alias.
///
/// Config errors are detected here, before any filesystem write.
fn resolve_aliases(
    manifest: &Manifest,
    override_list: &Option<Vec<String>>,
    target_rel: &str,
) -> Result<Vec<String>, PrepareError> {
    let raw = match override_list {
        Some(list) => list.as_slice(),
        None => manifest.aliases.as_deref().unwrap_or(&[]),
    };
    let mut out = Vec::with_capacity(raw.len());
    let mut seen = HashSet::new();
    for alias in raw {
        // normalize_rel rejects empty, absolute and root-escaping paths.
        let norm = normalize_rel(alias).map_err(PrepareError::InvalidAlias)?;
        if norm == target_rel {
            return Err(PrepareError::InvalidAlias(format!(
                "alias '{norm}' must not equal the target '{target_rel}'"
            )));
        }
        if !seen.insert(norm.clone()) {
            return Err(PrepareError::InvalidAlias(format!(
                "duplicate alias '{norm}'"
            )));
        }
        out.push(norm);
    }
    Ok(out)
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
                ..Default::default()
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
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, PrepareError::InvalidTarget(_)));
    }

    #[test]
    fn aliases_come_from_manifest_normalized() {
        let tmp = project(r#"{ "aliases": ["./.claude/skills", ".cursor\\skills"] }"#);
        let ctx = prepare(tmp.path(), PrepareOptions::default()).unwrap();
        assert_eq!(ctx.aliases, [".claude/skills", ".cursor/skills"]);
    }

    #[test]
    fn cli_alias_override_replaces_manifest_aliases() {
        let tmp = project(r#"{ "aliases": [".claude/skills"] }"#);
        let ctx = prepare(
            tmp.path(),
            PrepareOptions {
                alias_override: Some(vec![".cursor/skills".to_string()]),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(ctx.aliases, [".cursor/skills"]);
    }

    #[test]
    fn empty_cli_alias_override_clears_manifest_aliases() {
        let tmp = project(r#"{ "aliases": [".claude/skills"] }"#);
        let ctx = prepare(
            tmp.path(),
            PrepareOptions {
                alias_override: Some(vec![]),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(ctx.aliases.is_empty());
    }

    #[test]
    fn cli_alias_equal_to_effective_target_rejected() {
        // `--target` shifts the effective target; an alias that now equals it
        // must still be rejected even though the manifest was fine.
        let tmp = project(r#"{ "target": ".agents/skills" }"#);
        let err = prepare(
            tmp.path(),
            PrepareOptions {
                target_override: Some(".claude/skills".to_string()),
                alias_override: Some(vec!["./.claude/skills".to_string()]),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            matches!(&err, PrepareError::InvalidAlias(m) if m.contains("must not equal the target")),
            "{err:?}"
        );
    }

    #[test]
    fn cli_alias_escaping_root_rejected() {
        let tmp = project("{}");
        let err = prepare(
            tmp.path(),
            PrepareOptions {
                alias_override: Some(vec!["../escape".to_string()]),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, PrepareError::InvalidAlias(_)), "{err:?}");
    }

    #[test]
    fn missing_manifest_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let err = prepare(tmp.path(), PrepareOptions::default()).unwrap_err();
        assert!(matches!(err, PrepareError::Manifest(_)));
    }
}
