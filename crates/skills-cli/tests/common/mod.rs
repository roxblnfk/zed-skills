//! Shared helpers for the CLI end-to-end tests.

#![allow(dead_code)] // each integration test binary uses a subset

use std::path::{Path, PathBuf};
use std::sync::Arc;

use skills_core::error::PipelineError;
use skills_core::pipeline::ctx::{Ctx, PrepareOptions, prepare};
use skills_core::pipeline::{SyncReport, run_update};
use skills_core::traits::{SkillLocator, VendorProvider};
use skills_providers::{DeclaredLocator, DirProvider};

pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

/// Copy a named fixture project into a fresh temp dir.
pub fn fixture_project(name: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    copy_dir(&fixtures_dir().join(name), tmp.path());
    tmp
}

pub fn copy_dir(src: &Path, dst: &Path) {
    for entry in walkdir::WalkDir::new(src) {
        let entry = entry.unwrap();
        let rel = entry.path().strip_prefix(src).unwrap();
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target).unwrap();
        } else {
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

pub fn providers() -> Vec<Arc<dyn VendorProvider>> {
    vec![Arc::new(DirProvider)]
}

pub fn locators() -> Vec<Arc<dyn SkillLocator>> {
    vec![Arc::new(DeclaredLocator)]
}

pub fn make_ctx(project_root: &Path, dry_run: bool) -> Result<Ctx, PipelineError> {
    Ok(prepare(
        project_root,
        PrepareOptions {
            target_override: None,
            dry_run,
        },
    )?)
}

/// Run the full pipeline through the library API.
pub async fn update(project_root: &Path, dry_run: bool) -> Result<SyncReport, PipelineError> {
    let ctx = make_ctx(project_root, dry_run)?;
    run_update(&ctx, &providers(), &locators(), &skills_audit::noop_chain()).await
}

/// Sorted listing of all files under `root` (relative, `/`-separated).
pub fn tree_listing(root: &Path) -> Vec<String> {
    if !root.exists() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry.unwrap();
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(root)
            .unwrap()
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        out.push(rel);
    }
    out.sort();
    out
}

/// Fingerprint of a whole tree: sorted `path => content bytes` pairs.
/// Two identical fingerprints mean byte-identical trees.
pub fn tree_fingerprint(root: &Path) -> Vec<(String, Vec<u8>)> {
    tree_listing(root)
        .into_iter()
        .map(|rel| {
            let full = root.join(rel.split('/').collect::<PathBuf>());
            let bytes = std::fs::read(&full).unwrap();
            (rel, bytes)
        })
        .collect()
}

/// Redact volatile values (content hashes) from a lockfile string so
/// snapshots stay stable across line-ending configurations.
pub fn redact_lock(lock: &str) -> String {
    let mut out = String::new();
    for line in lock.lines() {
        if let Some(idx) = line.find("\"content_hash\": \"") {
            out.push_str(&line[..idx]);
            out.push_str("\"content_hash\": \"[hash]\",");
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}
