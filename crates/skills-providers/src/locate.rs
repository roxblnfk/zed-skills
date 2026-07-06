//! Skill locators.
//!
//! Chain order (wired by the CLI): `ComposerDeclaredLocator` →
//! `WellKnownLocator` → `DeclaredLocator`.
//!
//! - `ComposerDeclaredLocator` — remote vendors shipping a `composer.json`
//!   with `extra.skills.source` (SPEC §6.1). Malformed or absent
//!   declarations fall through to the next locator, never block the run.
//! - `WellKnownLocator` — remote vendors shipping conventional containers
//!   (SPEC §6.2): `.agents/skills`, `.claude/skills`, `.cursor/skills`,
//!   `skills`, `resources/skills`; skills at depth 1 or in a depth-2
//!   catalog layout.
//! - `DeclaredLocator` — explicit-root flavor for local `local.dir` donors:
//!   the vendor root itself is the skills root.
//!
//! `RecursiveFallbackLocator` (discovery-gated bounded walk) lands in M3.

use std::path::Path;

use skills_core::domain::{MaterializedVendor, Origin, SkillsRoot};
use skills_core::error::ScanError;
use skills_core::traits::{Located, SkillLocator};

/// Declared locator, explicit-root flavor: a *local* vendor's root is
/// itself the skills root. Immediate subdirectories containing `SKILL.md`
/// become skills. Not applicable to remote vendors — a repo root is not a
/// skills root.
pub struct DeclaredLocator;

impl SkillLocator for DeclaredLocator {
    fn locate(&self, vendor: &MaterializedVendor) -> Result<Located, ScanError> {
        if !matches!(vendor.origin, Origin::Local { .. }) {
            return Ok(Located::NotApplicable);
        }
        Ok(Located::Found(vec![SkillsRoot {
            path: vendor.root.clone(),
        }]))
    }
}

/// Composer-declared locator: `composer.json` with `extra.skills.source`
/// names the skills root. The source must be a non-empty relative path that
/// does not escape the package root. Anything malformed (bad JSON,
/// `extra.skills` without `source`, absolute/escaping path, missing dir)
/// makes this locator not applicable — the chain moves on.
pub struct ComposerDeclaredLocator;

impl SkillLocator for ComposerDeclaredLocator {
    fn locate(&self, vendor: &MaterializedVendor) -> Result<Located, ScanError> {
        if matches!(vendor.origin, Origin::Local { .. }) {
            // Local `local.dir` donors keep their explicit-root semantics;
            // the composer *provider* lands in M3.
            return Ok(Located::NotApplicable);
        }
        let Some(source) = declared_source(&vendor.root) else {
            return Ok(Located::NotApplicable);
        };
        let root = vendor.root.join(source);
        if !root.is_dir() {
            return Ok(Located::NotApplicable);
        }
        Ok(Located::Found(vec![SkillsRoot { path: root }]))
    }
}

/// Read `extra.skills.source` from `<root>/composer.json`, validating the
/// path shape. `None` on any deviation.
fn declared_source(root: &Path) -> Option<std::path::PathBuf> {
    let raw = std::fs::read_to_string(root.join("composer.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let source = value.get("extra")?.get("skills")?.get("source")?.as_str()?;
    let trimmed = source.trim();
    if trimmed.is_empty() || skills_core::paths::is_absolute_like(trimmed) {
        return None;
    }
    let mut out = std::path::PathBuf::new();
    let mut depth: i32 = 0;
    for segment in trimmed.split(['/', '\\']) {
        match segment {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return None; // escapes the package root
                }
                out.pop();
            }
            s => {
                depth += 1;
                out.push(s);
            }
        }
    }
    (depth > 0).then_some(out)
}

/// Well-known container locator for remote vendors. Every existing
/// container contributes:
/// - itself as a root (skills at depth 1: `<c>/<name>/SKILL.md`), and
/// - each immediate subdirectory *without* its own `SKILL.md` that holds
///   skill dirs one level deeper (depth-2 catalog:
///   `<c>/<category>/<name>/SKILL.md`).
///
/// A recognized skill dir is never treated as a category (no nested
/// skills).
pub struct WellKnownLocator;

/// Conventional container roots, probed in order (SPEC §6.2).
pub const CONTAINER_ROOTS: [&str; 5] = [
    ".agents/skills",
    ".claude/skills",
    ".cursor/skills",
    "skills",
    "resources/skills",
];

impl SkillLocator for WellKnownLocator {
    fn locate(&self, vendor: &MaterializedVendor) -> Result<Located, ScanError> {
        if matches!(vendor.origin, Origin::Local { .. }) {
            return Ok(Located::NotApplicable);
        }
        let io_err = |path: &Path, source: std::io::Error| ScanError::Io {
            vendor: vendor.name.clone(),
            path: path.to_path_buf(),
            source,
        };

        let mut roots: Vec<SkillsRoot> = Vec::new();
        for container in CONTAINER_ROOTS {
            let container_dir = container
                .split('/')
                .fold(vendor.root.clone(), |dir, seg| dir.join(seg));
            if !container_dir.is_dir() {
                continue;
            }
            roots.push(SkillsRoot {
                path: container_dir.clone(),
            });

            // Depth-2 catalog: category dirs (no SKILL.md of their own)
            // whose children are skills. Sorted for determinism.
            let mut categories: Vec<std::path::PathBuf> = Vec::new();
            for entry in std::fs::read_dir(&container_dir).map_err(|e| io_err(&container_dir, e))? {
                let entry = entry.map_err(|e| io_err(&container_dir, e))?;
                let path = entry.path();
                if !path.is_dir() || path.join("SKILL.md").is_file() {
                    continue;
                }
                let holds_skills = std::fs::read_dir(&path)
                    .map_err(|e| io_err(&path, e))?
                    .filter_map(|e| e.ok())
                    .any(|leaf| leaf.path().is_dir() && leaf.path().join("SKILL.md").is_file());
                if holds_skills {
                    categories.push(path);
                }
            }
            categories.sort();
            roots.extend(categories.into_iter().map(|path| SkillsRoot { path }));
        }

        if roots.is_empty() {
            Ok(Located::NotApplicable)
        } else {
            Ok(Located::Found(roots))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skills_core::domain::{SkillsFilter, VendorName};

    fn local_vendor(root: &Path) -> MaterializedVendor {
        MaterializedVendor {
            name: VendorName::new("dir/x"),
            origin: Origin::Local { path: "./x".into() },
            root: root.to_path_buf(),
            ref_resolved: None,
            filter: SkillsFilter::All,
        }
    }

    fn remote_vendor(root: &Path) -> MaterializedVendor {
        MaterializedVendor {
            name: VendorName::new("acme/skills"),
            origin: Origin::Remote {
                host: "github.com".into(),
                package: "acme/skills".into(),
                r#ref: None,
            },
            root: root.to_path_buf(),
            ref_resolved: Some("v1.0.0".into()),
            filter: SkillsFilter::All,
        }
    }

    fn make_skill(root: &Path, rel: &str) {
        let dir = rel.split('/').fold(root.to_path_buf(), |d, s| d.join(s));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "x").unwrap();
    }

    // --- DeclaredLocator -----------------------------------------------

    #[test]
    fn declared_locator_returns_the_vendor_root() {
        let vendor = local_vendor(Path::new("some/root"));
        let located = DeclaredLocator.locate(&vendor).unwrap();
        assert_eq!(
            located,
            Located::Found(vec![SkillsRoot {
                path: std::path::PathBuf::from("some/root")
            }])
        );
    }

    #[test]
    fn declared_locator_skips_remote_vendors() {
        let vendor = remote_vendor(Path::new("some/root"));
        assert_eq!(
            DeclaredLocator.locate(&vendor).unwrap(),
            Located::NotApplicable
        );
    }

    // --- ComposerDeclaredLocator ----------------------------------------

    #[test]
    fn composer_declared_source_is_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("composer.json"),
            r#"{ "name": "acme/skills", "extra": { "skills": { "source": "resources/my-skills" } } }"#,
        )
        .unwrap();
        make_skill(tmp.path(), "resources/my-skills/alpha");
        let located = ComposerDeclaredLocator
            .locate(&remote_vendor(tmp.path()))
            .unwrap();
        assert_eq!(
            located,
            Located::Found(vec![SkillsRoot {
                path: tmp.path().join("resources").join("my-skills")
            }])
        );
    }

    #[test]
    fn composer_declared_not_applicable_cases() {
        let tmp = tempfile::tempdir().unwrap();
        let vendor = remote_vendor(tmp.path());
        // No composer.json at all.
        assert_eq!(
            ComposerDeclaredLocator.locate(&vendor).unwrap(),
            Located::NotApplicable
        );
        // Malformed JSON.
        std::fs::write(tmp.path().join("composer.json"), "{ nope").unwrap();
        assert_eq!(
            ComposerDeclaredLocator.locate(&vendor).unwrap(),
            Located::NotApplicable
        );
        // extra.skills without source (rootlike, skipped silently).
        std::fs::write(
            tmp.path().join("composer.json"),
            r#"{ "extra": { "skills": { "target": "x" } } }"#,
        )
        .unwrap();
        assert_eq!(
            ComposerDeclaredLocator.locate(&vendor).unwrap(),
            Located::NotApplicable
        );
        // Absolute source rejected.
        std::fs::write(
            tmp.path().join("composer.json"),
            r#"{ "extra": { "skills": { "source": "/abs" } } }"#,
        )
        .unwrap();
        assert_eq!(
            ComposerDeclaredLocator.locate(&vendor).unwrap(),
            Located::NotApplicable
        );
        // Escaping source rejected.
        std::fs::write(
            tmp.path().join("composer.json"),
            r#"{ "extra": { "skills": { "source": "../outside" } } }"#,
        )
        .unwrap();
        assert_eq!(
            ComposerDeclaredLocator.locate(&vendor).unwrap(),
            Located::NotApplicable
        );
        // Declared dir that does not exist.
        std::fs::write(
            tmp.path().join("composer.json"),
            r#"{ "extra": { "skills": { "source": "missing" } } }"#,
        )
        .unwrap();
        assert_eq!(
            ComposerDeclaredLocator.locate(&vendor).unwrap(),
            Located::NotApplicable
        );
    }

    #[test]
    fn composer_declared_skips_local_vendors() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("composer.json"),
            r#"{ "extra": { "skills": { "source": "skills" } } }"#,
        )
        .unwrap();
        make_skill(tmp.path(), "skills/alpha");
        assert_eq!(
            ComposerDeclaredLocator
                .locate(&local_vendor(tmp.path()))
                .unwrap(),
            Located::NotApplicable
        );
    }

    // --- WellKnownLocator -------------------------------------------------

    #[test]
    fn well_known_finds_containers_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        make_skill(tmp.path(), ".claude/skills/alpha");
        make_skill(tmp.path(), "skills/beta");
        let located = WellKnownLocator.locate(&remote_vendor(tmp.path())).unwrap();
        let Located::Found(roots) = located else {
            panic!("expected Found");
        };
        assert_eq!(
            roots,
            vec![
                SkillsRoot {
                    path: tmp.path().join(".claude").join("skills")
                },
                SkillsRoot {
                    path: tmp.path().join("skills")
                },
            ]
        );
    }

    #[test]
    fn well_known_adds_depth_2_catalog_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        make_skill(tmp.path(), "skills/flat");
        make_skill(tmp.path(), "skills/backend/api-design");
        make_skill(tmp.path(), "skills/frontend/css");
        let located = WellKnownLocator.locate(&remote_vendor(tmp.path())).unwrap();
        let Located::Found(roots) = located else {
            panic!("expected Found");
        };
        let paths: Vec<_> = roots.iter().map(|r| r.path.clone()).collect();
        assert_eq!(
            paths,
            vec![
                tmp.path().join("skills"),
                tmp.path().join("skills").join("backend"),
                tmp.path().join("skills").join("frontend"),
            ]
        );
    }

    #[test]
    fn recognized_skill_dir_is_never_a_category() {
        let tmp = tempfile::tempdir().unwrap();
        // "outer" is a skill AND contains something SKILL.md-shaped below —
        // it must not be descended into.
        make_skill(tmp.path(), "skills/outer");
        make_skill(tmp.path(), "skills/outer/inner");
        let located = WellKnownLocator.locate(&remote_vendor(tmp.path())).unwrap();
        let Located::Found(roots) = located else {
            panic!("expected Found");
        };
        assert_eq!(
            roots,
            vec![SkillsRoot {
                path: tmp.path().join("skills")
            }]
        );
    }

    #[test]
    fn well_known_not_applicable_without_containers() {
        let tmp = tempfile::tempdir().unwrap();
        make_skill(tmp.path(), "elsewhere/alpha");
        assert_eq!(
            WellKnownLocator.locate(&remote_vendor(tmp.path())).unwrap(),
            Located::NotApplicable
        );
    }

    #[test]
    fn well_known_skips_local_vendors() {
        let tmp = tempfile::tempdir().unwrap();
        make_skill(tmp.path(), "skills/alpha");
        assert_eq!(
            WellKnownLocator.locate(&local_vendor(tmp.path())).unwrap(),
            Located::NotApplicable
        );
    }
}
