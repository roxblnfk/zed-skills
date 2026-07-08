//! Bounded skill-tree scanning, shared by the discovery locators and the
//! composer provider's candidate detection.
//!
//! Mirrors the PHP `SkillTreeScanner`:
//!
//! 1. Probe the well-known containers; accept `SKILL.md` at depth 1
//!    (`<container>/<name>/`) or depth 2 (`<container>/<category>/<name>/`).
//! 2. Only when no container yielded a skill, walk the package tree bounded
//!    by [`FALLBACK_MAX_DEPTH`], skipping [`SKIP_DIRS`] and dot-prefixed
//!    directories, never descending into a subdirectory carrying its own
//!    `composer.json`; the first `SKILL.md` on a branch stops descent.
//!
//! Per-directory IO errors are swallowed (an unreadable subtree simply
//! contributes nothing). Junction/realpath containment and canonical-path
//! dedup are enforced downstream by the scan stage.

use std::path::{Path, PathBuf};

use crate::locate::CONTAINER_ROOTS;

/// Directory names never descended into during the fallback walk.
pub const SKIP_DIRS: [&str; 3] = ["vendor", "node_modules", ".git"];

/// Depth ceiling for the fallback walk, in levels below the package root.
pub const FALLBACK_MAX_DEPTH: usize = 5;

/// Sorted immediate subdirectories of `dir`; empty on any IO error.
fn subdirs(dir: &Path) -> Vec<PathBuf> {
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

fn is_skill_dir(dir: &Path) -> bool {
    dir.join("SKILL.md").is_file()
}

/// Skill directories found under the well-known containers: depth 1 (flat)
/// and depth 2 (catalog). A recognized skill dir is never descended into.
pub fn container_skill_dirs(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for container in CONTAINER_ROOTS {
        let container_dir = container
            .split('/')
            .fold(root.to_path_buf(), |d, s| d.join(s));
        if !container_dir.is_dir() {
            continue;
        }
        for child in subdirs(&container_dir) {
            if is_skill_dir(&child) {
                found.push(child);
                continue;
            }
            // Catalog layout: <container>/<category>/<name>/SKILL.md.
            for leaf in subdirs(&child) {
                if is_skill_dir(&leaf) {
                    found.push(leaf);
                }
            }
        }
    }
    found
}

/// Bounded fallback walk finding skill dirs in non-conventional locations.
pub fn fallback_skill_dirs(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    walk(root, 0, &mut found);
    found
}

fn walk(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    if depth >= FALLBACK_MAX_DEPTH {
        return;
    }
    for child in subdirs(dir) {
        let name = child
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Dependency/VCS trees and hidden directories never carry
        // first-party skills worth a deep walk.
        if SKIP_DIRS.contains(&name.as_str()) || name.starts_with('.') {
            continue;
        }
        // A nested package (its own composer.json) is a separate unit of
        // distribution — never descend across that boundary.
        if child.join("composer.json").is_file() {
            continue;
        }
        if is_skill_dir(&child) {
            // First SKILL.md on a branch stops descent (shadowing).
            found.push(child);
            continue;
        }
        walk(&child, depth + 1, found);
    }
}

/// Whether a package that declares no skills source ships anything the
/// discovery chain could pick up (candidate detection for the `[hint]`).
pub fn has_discoverable_skills(root: &Path) -> bool {
    if !container_skill_dirs(root).is_empty() {
        return true;
    }
    !fallback_skill_dirs(root).is_empty()
}

/// Distinct parents of `dirs`, first-seen order preserved. Skill locators
/// report skills roots (dirs whose immediate subdirs are skills), so a set
/// of skill dirs maps back to its parents.
pub fn parent_roots(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for dir in dirs {
        if let Some(parent) = dir.parent()
            && !out.iter().any(|p| p == parent)
        {
            out.push(parent.to_path_buf());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_skill(root: &Path, rel: &str) {
        let dir = rel.split('/').fold(root.to_path_buf(), |d, s| d.join(s));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "x").unwrap();
    }

    #[test]
    fn containers_find_flat_and_catalog_skills() {
        let tmp = tempfile::tempdir().unwrap();
        make_skill(tmp.path(), ".claude/skills/alpha");
        make_skill(tmp.path(), "skills/php/catalogued");
        let dirs = container_skill_dirs(tmp.path());
        assert_eq!(
            dirs,
            vec![
                tmp.path().join(".claude").join("skills").join("alpha"),
                tmp.path().join("skills").join("php").join("catalogued"),
            ]
        );
    }

    #[test]
    fn skill_dir_is_never_treated_as_catalog_category() {
        let tmp = tempfile::tempdir().unwrap();
        make_skill(tmp.path(), "skills/outer");
        make_skill(tmp.path(), "skills/outer/inner");
        assert_eq!(
            container_skill_dirs(tmp.path()),
            vec![tmp.path().join("skills").join("outer")]
        );
    }

    #[test]
    fn fallback_respects_depth_cap() {
        let tmp = tempfile::tempdir().unwrap();
        // Depth 5 below the root: found.
        make_skill(tmp.path(), "a/b/c/d/at-five");
        // Depth 6: beyond the cap.
        make_skill(tmp.path(), "z/b/c/d/e/at-six");
        let dirs = fallback_skill_dirs(tmp.path());
        assert_eq!(
            dirs,
            vec![
                tmp.path()
                    .join("a")
                    .join("b")
                    .join("c")
                    .join("d")
                    .join("at-five")
            ]
        );
    }

    #[test]
    fn fallback_skips_vendor_node_modules_git_and_dot_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        make_skill(tmp.path(), "vendor/hidden");
        make_skill(tmp.path(), "node_modules/hidden");
        make_skill(tmp.path(), ".git/hidden");
        make_skill(tmp.path(), ".secret/hidden");
        make_skill(tmp.path(), "lib/visible");
        let dirs = fallback_skill_dirs(tmp.path());
        assert_eq!(dirs, vec![tmp.path().join("lib").join("visible")]);
    }

    #[test]
    fn fallback_never_crosses_a_composer_json_boundary() {
        let tmp = tempfile::tempdir().unwrap();
        make_skill(tmp.path(), "embedded/pkg/skills/theirs");
        std::fs::write(
            tmp.path()
                .join("embedded")
                .join("pkg")
                .join("composer.json"),
            "{}",
        )
        .unwrap();
        make_skill(tmp.path(), "mine/ours");
        let dirs = fallback_skill_dirs(tmp.path());
        assert_eq!(dirs, vec![tmp.path().join("mine").join("ours")]);
    }

    #[test]
    fn first_skill_md_on_a_branch_stops_descent() {
        let tmp = tempfile::tempdir().unwrap();
        make_skill(tmp.path(), "lib/outer");
        make_skill(tmp.path(), "lib/outer/nested");
        let dirs = fallback_skill_dirs(tmp.path());
        assert_eq!(dirs, vec![tmp.path().join("lib").join("outer")]);
    }

    #[test]
    fn candidate_detection_prefers_containers_then_falls_back() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!has_discoverable_skills(tmp.path()));
        make_skill(tmp.path(), "prompts/odd-spot");
        assert!(has_discoverable_skills(tmp.path()));
        make_skill(tmp.path(), "skills/conventional");
        assert!(has_discoverable_skills(tmp.path()));
    }

    #[test]
    fn parent_roots_dedupes_preserving_order() {
        let dirs = vec![
            PathBuf::from("c/one"),
            PathBuf::from("c/two"),
            PathBuf::from("d/three"),
        ];
        assert_eq!(
            parent_roots(&dirs),
            vec![PathBuf::from("c"), PathBuf::from("d")]
        );
    }
}
