//! Skill locators.
//!
//! Chain order (wired by the CLI): `ComposerDeclaredLocator` →
//! `WellKnownLocator` → `RecursiveFallbackLocator` → `DeclaredLocator`.
//! Applicability is routed by [`SourceHint`] on the materialized vendor:
//!
//! - `ExplicitRoot` (local `dir` source donors) — the vendor root itself is
//!   the skills root ([`DeclaredLocator`]).
//! - `Declared(source)` (composer donors with `extra.skills.source`) — the
//!   declared directory is the skills root ([`ComposerDeclaredLocator`]).
//! - `Probe` (remote/url donors) — probe a shipped `composer.json`
//!   declaration, then the well-known containers; the recursive fallback
//!   applies only when discovery is enabled.
//! - `Discovery` (undeclared composer donors admitted via discovery) —
//!   well-known containers first; the bounded recursive fallback only when
//!   the containers yielded nothing.

use std::path::Path;

use skills_core::domain::{MaterializedVendor, SkillsRoot, SourceHint};
use skills_core::error::ScanError;
use skills_core::paths::rel_to_path;
use skills_core::traits::{Located, SkillLocator};

use crate::treescan;

/// Declared locator, explicit-root flavor: a `dir` source vendor's root is
/// itself the skills root. Immediate subdirectories containing `SKILL.md`
/// become skills.
pub struct DeclaredLocator;

impl SkillLocator for DeclaredLocator {
    fn locate(&self, vendor: &MaterializedVendor) -> Result<Located, ScanError> {
        if vendor.source_hint != SourceHint::ExplicitRoot {
            return Ok(Located::NotApplicable);
        }
        Ok(Located::Found(vec![SkillsRoot {
            path: vendor.root.clone(),
        }]))
    }
}

/// Composer-declared locator: `extra.skills.source` names the skills root.
///
/// - Composer donors carry their pre-validated source on the vendor
///   ([`SourceHint::Declared`]); a declared-but-missing directory yields
///   zero skills (the donor never falls through to discovery).
/// - Remote/url donors ([`SourceHint::Probe`]) are probed via the shipped
///   `composer.json`; anything malformed (bad JSON, `extra.skills` without
///   `source`, absolute/escaping path, missing dir) makes this locator not
///   applicable — the chain moves on.
pub struct ComposerDeclaredLocator;

impl SkillLocator for ComposerDeclaredLocator {
    fn locate(&self, vendor: &MaterializedVendor) -> Result<Located, ScanError> {
        match &vendor.source_hint {
            SourceHint::Declared(source) => {
                let root = vendor.root.join(rel_to_path(source));
                if root.is_dir() {
                    Ok(Located::Found(vec![SkillsRoot { path: root }]))
                } else {
                    Ok(Located::Found(vec![]))
                }
            }
            SourceHint::Probe => {
                let Some(source) = declared_source(&vendor.root) else {
                    return Ok(Located::NotApplicable);
                };
                let root = vendor.root.join(source);
                if !root.is_dir() {
                    return Ok(Located::NotApplicable);
                }
                Ok(Located::Found(vec![SkillsRoot { path: root }]))
            }
            _ => Ok(Located::NotApplicable),
        }
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

/// Well-known container locator. Every existing container contributes:
/// - itself as a root (skills at depth 1: `<c>/<name>/SKILL.md`), and
/// - each immediate subdirectory *without* its own `SKILL.md` that holds
///   skill dirs one level deeper (depth-2 catalog:
///   `<c>/<category>/<name>/SKILL.md`).
///
/// A recognized skill dir is never treated as a category (no nested
/// skills). For discovery donors ([`SourceHint::Discovery`]) the locator is
/// applicable only when the containers hold at least one actual skill —
/// otherwise the chain proceeds to the recursive fallback ("fallback only
/// when no container yielded anything").
pub struct WellKnownLocator;

/// Conventional container roots, probed in order.
pub const CONTAINER_ROOTS: [&str; 5] = [
    ".agents/skills",
    ".claude/skills",
    ".cursor/skills",
    "skills",
    "resources/skills",
];

impl SkillLocator for WellKnownLocator {
    fn locate(&self, vendor: &MaterializedVendor) -> Result<Located, ScanError> {
        match vendor.source_hint {
            SourceHint::Probe => {}
            SourceHint::Discovery => {
                // Roots = parents of the actual container skills; empty
                // containers do not block the recursive fallback.
                let skills = treescan::container_skill_dirs(&vendor.root);
                if skills.is_empty() {
                    return Ok(Located::NotApplicable);
                }
                return Ok(Located::Found(
                    treescan::parent_roots(&skills)
                        .into_iter()
                        .map(|path| SkillsRoot { path })
                        .collect(),
                ));
            }
            _ => return Ok(Located::NotApplicable),
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

/// Bounded recursive fallback: max depth 5 below the package
/// root, skipping `vendor/`, `node_modules/`, `.git/` and dot-prefixed
/// dirs, never descending into a subdir carrying its own `composer.json`;
/// the first `SKILL.md` on a branch stops descent.
///
/// Applies to discovery donors (undeclared composer packages — the chain
/// only reaches this point when the containers yielded nothing) and, when
/// the global discovery flag is on, to remote/url donors with neither a
/// declared source nor a well-known container.
pub struct RecursiveFallbackLocator {
    /// Effective discovery flag of the run — gates the remote/url path.
    pub discovery: bool,
}

impl RecursiveFallbackLocator {
    pub fn new(discovery: bool) -> Self {
        RecursiveFallbackLocator { discovery }
    }
}

impl SkillLocator for RecursiveFallbackLocator {
    fn locate(&self, vendor: &MaterializedVendor) -> Result<Located, ScanError> {
        let applies = match vendor.source_hint {
            SourceHint::Discovery => true,
            SourceHint::Probe => self.discovery,
            _ => false,
        };
        if !applies {
            return Ok(Located::NotApplicable);
        }
        let skills = treescan::fallback_skill_dirs(&vendor.root);
        Ok(Located::Found(
            treescan::parent_roots(&skills)
                .into_iter()
                .map(|path| SkillsRoot { path })
                .collect(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skills_core::domain::{Origin, SkillsFilter, VendorName};

    fn vendor_with(root: &Path, origin: Origin, hint: SourceHint) -> MaterializedVendor {
        MaterializedVendor {
            name: VendorName::new("test/vendor"),
            origin,
            root: root.to_path_buf(),
            ref_resolved: None,
            filter: SkillsFilter::All,
            source_hint: hint,
        }
    }

    fn local_vendor(root: &Path) -> MaterializedVendor {
        vendor_with(
            root,
            Origin::Local { path: "./x".into() },
            SourceHint::ExplicitRoot,
        )
    }

    fn remote_vendor(root: &Path) -> MaterializedVendor {
        vendor_with(
            root,
            Origin::Remote {
                host: "github.com".into(),
                package: "acme/skills".into(),
                r#ref: None,
            },
            SourceHint::Probe,
        )
    }

    fn discovery_vendor(root: &Path) -> MaterializedVendor {
        vendor_with(
            root,
            Origin::Local {
                path: "vendor/acme/undeclared".into(),
            },
            SourceHint::Discovery,
        )
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
    fn composer_donor_uses_the_prevalidated_source() {
        let tmp = tempfile::tempdir().unwrap();
        make_skill(tmp.path(), "my-skills/alpha");
        let vendor = vendor_with(
            tmp.path(),
            Origin::Local {
                path: "vendor/acme/pro".into(),
            },
            SourceHint::Declared("my-skills".into()),
        );
        assert_eq!(
            ComposerDeclaredLocator.locate(&vendor).unwrap(),
            Located::Found(vec![SkillsRoot {
                path: tmp.path().join("my-skills")
            }])
        );
    }

    #[test]
    fn composer_donor_with_missing_declared_dir_yields_zero_roots() {
        let tmp = tempfile::tempdir().unwrap();
        // A declared donor never falls through to discovery locators.
        make_skill(tmp.path(), "skills/should-not-be-found");
        let vendor = vendor_with(
            tmp.path(),
            Origin::Local {
                path: "vendor/acme/pro".into(),
            },
            SourceHint::Declared("missing-dir".into()),
        );
        assert_eq!(
            ComposerDeclaredLocator.locate(&vendor).unwrap(),
            Located::Found(vec![])
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

    #[test]
    fn well_known_discovery_requires_actual_skills() {
        let tmp = tempfile::tempdir().unwrap();
        // Empty container: not applicable, the fallback gets its shot.
        std::fs::create_dir_all(tmp.path().join("skills")).unwrap();
        assert_eq!(
            WellKnownLocator
                .locate(&discovery_vendor(tmp.path()))
                .unwrap(),
            Located::NotApplicable
        );

        make_skill(tmp.path(), ".claude/skills/flat");
        make_skill(tmp.path(), "skills/php/catalogued");
        let located = WellKnownLocator
            .locate(&discovery_vendor(tmp.path()))
            .unwrap();
        assert_eq!(
            located,
            Located::Found(vec![
                SkillsRoot {
                    path: tmp.path().join(".claude").join("skills")
                },
                SkillsRoot {
                    path: tmp.path().join("skills").join("php")
                },
            ])
        );
    }

    // --- RecursiveFallbackLocator ---------------------------------------

    #[test]
    fn fallback_applies_to_discovery_vendors() {
        let tmp = tempfile::tempdir().unwrap();
        make_skill(tmp.path(), "lib/prompts/helper");
        let located = RecursiveFallbackLocator::new(false)
            .locate(&discovery_vendor(tmp.path()))
            .unwrap();
        assert_eq!(
            located,
            Located::Found(vec![SkillsRoot {
                path: tmp.path().join("lib").join("prompts")
            }])
        );
    }

    #[test]
    fn fallback_gates_remote_vendors_on_the_discovery_flag() {
        let tmp = tempfile::tempdir().unwrap();
        make_skill(tmp.path(), "lib/helper");
        let vendor = remote_vendor(tmp.path());
        assert_eq!(
            RecursiveFallbackLocator::new(false)
                .locate(&vendor)
                .unwrap(),
            Located::NotApplicable
        );
        assert_eq!(
            RecursiveFallbackLocator::new(true).locate(&vendor).unwrap(),
            Located::Found(vec![SkillsRoot {
                path: tmp.path().join("lib")
            }])
        );
    }

    #[test]
    fn fallback_never_applies_to_explicit_root_vendors() {
        let tmp = tempfile::tempdir().unwrap();
        make_skill(tmp.path(), "lib/helper");
        assert_eq!(
            RecursiveFallbackLocator::new(true)
                .locate(&local_vendor(tmp.path()))
                .unwrap(),
            Located::NotApplicable
        );
    }
}
