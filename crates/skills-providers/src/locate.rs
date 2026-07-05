//! Skill locators.
//!
//! M1 implements the *explicit root* case of the Declared locator: the
//! vendor's materialized root is itself the skills root (used by
//! `DirProvider`). The composer `extra.skills.source` variant plus the
//! WellKnown and RecursiveFallback locators land in M3.

use skills_core::domain::{MaterializedVendor, SkillsRoot};
use skills_core::error::ScanError;
use skills_core::traits::{Located, SkillLocator};

/// Declared locator, explicit-root flavor: every vendor's root is a skills
/// root. Immediate subdirectories containing `SKILL.md` become skills.
pub struct DeclaredLocator;

impl SkillLocator for DeclaredLocator {
    fn locate(&self, vendor: &MaterializedVendor) -> Result<Located, ScanError> {
        Ok(Located::Found(vec![SkillsRoot {
            path: vendor.root.clone(),
        }]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skills_core::domain::{Origin, SkillsFilter, VendorName};

    #[test]
    fn declared_locator_returns_the_vendor_root() {
        let vendor = MaterializedVendor {
            name: VendorName::new("dir/x"),
            origin: Origin::Local { path: "./x".into() },
            root: std::path::PathBuf::from("some/root"),
            ref_resolved: None,
            filter: SkillsFilter::All,
        };
        let located = DeclaredLocator.locate(&vendor).unwrap();
        assert_eq!(
            located,
            Located::Found(vec![SkillsRoot {
                path: std::path::PathBuf::from("some/root")
            }])
        );
    }
}
