//! Stage 5 — Locate+Scan: find skills roots via the locator chain, then scan
//! each root for skills (frontmatter, file list, content hash).

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use crate::domain::{MaterializedVendor, ScannedSkill, SkillId, SkillsRoot, VendorName};
use crate::error::ScanError;
use crate::frontmatter::read_frontmatter;
use crate::fsutil;
use crate::traits::{Located, SkillLocator};

/// Locate and scan a single materialized vendor.
pub async fn scan_vendor(
    vendor: MaterializedVendor,
    locators: Vec<Arc<dyn SkillLocator>>,
) -> Result<Vec<ScannedSkill>, ScanError> {
    tokio::task::spawn_blocking(move || scan_vendor_blocking(&vendor, &locators))
        .await
        .map_err(|e| ScanError::Task(e.to_string()))?
}

/// Locate and scan all vendors, preserving vendor order.
pub async fn locate_and_scan(
    vendors: &[MaterializedVendor],
    locators: &[Arc<dyn SkillLocator>],
) -> Result<Vec<ScannedSkill>, ScanError> {
    let mut out = Vec::new();
    for vendor in vendors {
        out.extend(scan_vendor(vendor.clone(), locators.to_vec()).await?);
    }
    Ok(out)
}

fn scan_vendor_blocking(
    vendor: &MaterializedVendor,
    locators: &[Arc<dyn SkillLocator>],
) -> Result<Vec<ScannedSkill>, ScanError> {
    let roots = locate(vendor, locators)?;
    let vendor_canonical = vendor.root.canonicalize().map_err(|source| ScanError::Io {
        vendor: vendor.name.clone(),
        path: vendor.root.clone(),
        source,
    })?;

    let mut seen_canonical: HashSet<std::path::PathBuf> = HashSet::new();
    let mut skills = Vec::new();
    for root in roots {
        skills.extend(scan_root(
            vendor,
            &root,
            &vendor_canonical,
            &mut seen_canonical,
        )?);
    }
    skills.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(skills)
}

fn locate(
    vendor: &MaterializedVendor,
    locators: &[Arc<dyn SkillLocator>],
) -> Result<Vec<SkillsRoot>, ScanError> {
    for locator in locators {
        match locator.locate(vendor)? {
            Located::Found(roots) => return Ok(roots),
            Located::NotApplicable => continue,
        }
    }
    Err(ScanError::NoLocator {
        vendor: vendor.name.clone(),
    })
}

/// Scan one skills root: every immediate subdirectory containing `SKILL.md`
/// is a skill. Loose files are ignored. Junction/symlink safety: the skill
/// dir must canonicalize to inside the vendor root, duplicates (by canonical
/// path) are skipped.
fn scan_root(
    vendor: &MaterializedVendor,
    root: &SkillsRoot,
    vendor_canonical: &Path,
    seen_canonical: &mut HashSet<std::path::PathBuf>,
) -> Result<Vec<ScannedSkill>, ScanError> {
    let io_err = |path: &Path, source: std::io::Error| ScanError::Io {
        vendor: vendor.name.clone(),
        path: path.to_path_buf(),
        source,
    };

    let mut skills = Vec::new();
    let entries = std::fs::read_dir(&root.path).map_err(|e| io_err(&root.path, e))?;
    for entry in entries {
        let entry = entry.map_err(|e| io_err(&root.path, e))?;
        let path = entry.path();
        if !path.is_dir() || !path.join("SKILL.md").is_file() {
            continue;
        }
        // Silently reject dirs escaping the vendor root and canonical dups.
        let Ok(canonical) = path.canonicalize() else {
            continue;
        };
        if !canonical.starts_with(vendor_canonical) || !seen_canonical.insert(canonical) {
            continue;
        }

        let dir_name = entry.file_name().to_string_lossy().into_owned();
        let fm = read_frontmatter(&path.join("SKILL.md"));
        let files = fsutil::list_files(&path).map_err(|e| io_err(&path, e))?;
        let content_hash = fsutil::content_hash(&path, &files).map_err(|e| io_err(&path, e))?;

        skills.push(ScannedSkill {
            canonical_name: fm.name.unwrap_or_else(|| dir_name.clone()),
            id: SkillId::new(dir_name),
            description: fm.description,
            vendor: vendor.name.clone(),
            origin: vendor.origin.clone(),
            ref_resolved: vendor.ref_resolved.clone(),
            path,
            files,
            content_hash,
        });
    }
    Ok(skills)
}

/// Test seam: scan a root directory directly with vendor metadata.
pub fn scan_root_for_vendor(
    vendor: &MaterializedVendor,
    root: &Path,
) -> Result<Vec<ScannedSkill>, ScanError> {
    let vendor_canonical = vendor.root.canonicalize().map_err(|source| ScanError::Io {
        vendor: VendorName::new(vendor.name.as_str()),
        path: vendor.root.clone(),
        source,
    })?;
    let mut seen = HashSet::new();
    let mut skills = scan_root(
        vendor,
        &SkillsRoot {
            path: root.to_path_buf(),
        },
        &vendor_canonical,
        &mut seen,
    )?;
    skills.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(skills)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Origin, SkillsFilter};

    fn vendor_at(root: &Path) -> MaterializedVendor {
        MaterializedVendor {
            name: VendorName::new("dir/test"),
            origin: Origin::Local {
                path: "./test".to_string(),
            },
            root: root.to_path_buf(),
            ref_resolved: None,
            filter: SkillsFilter::All,
            source_hint: crate::domain::SourceHint::ExplicitRoot,
        }
    }

    fn make_skill(root: &Path, name: &str, frontmatter: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), frontmatter).unwrap();
    }

    #[test]
    fn scans_immediate_subdirs_with_skill_md() {
        let tmp = tempfile::tempdir().unwrap();
        make_skill(
            tmp.path(),
            "alpha",
            "---\nname: alpha-skill\ndescription: A\n---\n",
        );
        make_skill(tmp.path(), "beta", "# no frontmatter\n");
        // A dir without SKILL.md is not a skill.
        std::fs::create_dir_all(tmp.path().join("not-a-skill")).unwrap();
        // Loose files are ignored.
        std::fs::write(tmp.path().join("README.md"), "hi").unwrap();
        // Nested skills (depth 2) are not picked up by root scanning.
        make_skill(&tmp.path().join("not-a-skill"), "nested", "x");

        let vendor = vendor_at(tmp.path());
        let skills = scan_root_for_vendor(&vendor, tmp.path()).unwrap();
        let ids: Vec<&str> = skills.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["alpha", "beta"]);

        let alpha = &skills[0];
        assert_eq!(alpha.canonical_name, "alpha-skill");
        assert_eq!(alpha.description.as_deref(), Some("A"));
        assert_eq!(alpha.files, ["SKILL.md"]);
        assert!(!alpha.content_hash.is_empty());

        // Canonical name falls back to dir name without frontmatter.
        let beta = &skills[1];
        assert_eq!(beta.canonical_name, "beta");
        assert_eq!(beta.description, None);
    }

    #[test]
    fn lists_nested_files_sorted() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("skill");
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::create_dir_all(dir.join("references")).unwrap();
        std::fs::write(dir.join("SKILL.md"), "x").unwrap();
        std::fs::write(dir.join("scripts").join("run.ps1"), "y").unwrap();
        std::fs::write(dir.join("references").join("guide.md"), "z").unwrap();

        let vendor = vendor_at(tmp.path());
        let skills = scan_root_for_vendor(&vendor, tmp.path()).unwrap();
        assert_eq!(
            skills[0].files,
            ["SKILL.md", "references/guide.md", "scripts/run.ps1"]
        );
    }

    #[test]
    fn missing_root_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let vendor = vendor_at(tmp.path());
        let missing = tmp.path().join("nope");
        assert!(matches!(
            scan_root_for_vendor(&vendor, &missing),
            Err(ScanError::Io { .. })
        ));
    }

    #[tokio::test]
    async fn no_locator_applies_errors() {
        struct Never;
        impl SkillLocator for Never {
            fn locate(&self, _v: &MaterializedVendor) -> Result<Located, ScanError> {
                Ok(Located::NotApplicable)
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let vendor = vendor_at(tmp.path());
        let err = scan_vendor(vendor, vec![Arc::new(Never)])
            .await
            .unwrap_err();
        assert!(matches!(err, ScanError::NoLocator { .. }));
    }

    #[tokio::test]
    async fn locator_chain_falls_through() {
        struct Never;
        impl SkillLocator for Never {
            fn locate(&self, _v: &MaterializedVendor) -> Result<Located, ScanError> {
                Ok(Located::NotApplicable)
            }
        }
        struct Root;
        impl SkillLocator for Root {
            fn locate(&self, v: &MaterializedVendor) -> Result<Located, ScanError> {
                Ok(Located::Found(vec![SkillsRoot {
                    path: v.root.clone(),
                }]))
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        make_skill(tmp.path(), "one", "x");
        let vendor = vendor_at(tmp.path());
        let skills = scan_vendor(vendor, vec![Arc::new(Never), Arc::new(Root)])
            .await
            .unwrap();
        assert_eq!(skills.len(), 1);
    }
}
