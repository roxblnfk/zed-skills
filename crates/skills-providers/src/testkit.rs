//! Reusable provider contract suite.
//!
//! Every `Vendor` implementation must behave identically after
//! `materialize()`. Run this against each provider (DirProvider in M1;
//! GitHub/GitLab reuse it in M2 with wiremock-backed vendors).

use std::sync::Arc;

use skills_core::domain::MaterializedVendor;
use skills_core::pipeline::scan::scan_vendor;
use skills_core::traits::{Cache, SkillLocator, Vendor};

/// What the fixture behind the vendor is expected to contain.
pub struct ContractExpectations {
    /// Expected skill dir names, sorted.
    pub skill_ids: Vec<String>,
}

/// Assert the full vendor contract; returns the materialized vendor so
/// callers can add provider-specific checks.
pub async fn run_vendor_contract(
    vendor: &dyn Vendor,
    locators: Vec<Arc<dyn SkillLocator>>,
    cache: &Cache,
    expected: &ContractExpectations,
) -> MaterializedVendor {
    // Materialize yields an existing directory carrying the vendor identity.
    let mv = vendor
        .materialize(cache)
        .await
        .expect("materialize must succeed");
    assert!(mv.root.is_dir(), "materialized root must exist on disk");
    assert_eq!(&mv.name, vendor.name(), "materialized name must match");
    assert_eq!(
        &mv.origin,
        vendor.origin(),
        "materialized origin must match"
    );

    // Materialize is idempotent: same root, same identity.
    let again = vendor
        .materialize(cache)
        .await
        .expect("second materialize must succeed");
    assert_eq!(again.root, mv.root, "materialize must be idempotent");
    assert_eq!(again.origin, mv.origin);

    // After materialization all vendors scan identically.
    let skills = scan_vendor(mv.clone(), locators.clone())
        .await
        .expect("scan must succeed");
    let ids: Vec<String> = skills.iter().map(|s| s.id.as_str().to_string()).collect();
    assert_eq!(ids, expected.skill_ids, "scanned skill set must match");

    for skill in &skills {
        assert!(
            skill.files.contains(&"SKILL.md".to_string()),
            "every skill ships SKILL.md"
        );
        let mut sorted = skill.files.clone();
        sorted.sort();
        assert_eq!(skill.files, sorted, "file list must be sorted");
        assert!(
            skill.files.iter().all(|f| !f.contains('\\')),
            "relative paths use forward slashes"
        );
        assert_eq!(skill.content_hash.len(), 64, "sha-256 hex hash");
        assert_eq!(skill.vendor, mv.name);
        assert_eq!(skill.origin, mv.origin);
        assert!(skill.path.is_dir());
        assert!(!skill.canonical_name.is_empty());
    }

    // Scanning is deterministic.
    let rescan = scan_vendor(mv.clone(), locators).await.expect("rescan");
    assert_eq!(skills, rescan, "scan must be deterministic");

    mv
}
