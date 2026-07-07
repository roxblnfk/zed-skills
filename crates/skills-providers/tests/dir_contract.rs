//! Provider contract suite run against `DirProvider`.

use std::sync::Arc;

use skills_core::manifest::MANIFEST_NAME;
use skills_core::pipeline::ctx::{PrepareOptions, prepare};
use skills_core::traits::{Cache, SkillLocator, VendorProvider};
use skills_providers::testkit::{ContractExpectations, run_vendor_contract};
use skills_providers::{DeclaredLocator, DirProvider};

fn make_skill(root: &std::path::Path, name: &str) {
    let dir = root.join(name);
    std::fs::create_dir_all(dir.join("references")).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: The {name} skill\n---\nBody\n"),
    )
    .unwrap();
    std::fs::write(dir.join("references").join("guide.md"), "guide").unwrap();
}

#[tokio::test]
async fn dir_provider_satisfies_vendor_contract() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join(MANIFEST_NAME),
        r#"{ "sources": [ { "from": "dir", "path": "./skills-src" } ] }"#,
    )
    .unwrap();
    let donor = tmp.path().join("skills-src");
    make_skill(&donor, "alpha");
    make_skill(&donor, "beta");
    // Distractors: loose file + dir without SKILL.md.
    std::fs::write(donor.join("README.md"), "not a skill").unwrap();
    std::fs::create_dir_all(donor.join("not-a-skill")).unwrap();

    let ctx = prepare(tmp.path(), PrepareOptions::default()).unwrap();
    let refs = DirProvider.discover(&ctx).await.unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name.as_str(), "dir/skills-src");

    let cache = Cache::new(tmp.path().join(".skills-cache"));
    let locators: Vec<Arc<dyn SkillLocator>> = vec![Arc::new(DeclaredLocator)];
    let mv = run_vendor_contract(
        refs[0].vendor.as_ref(),
        locators,
        &cache,
        &ContractExpectations {
            skill_ids: vec!["alpha".to_string(), "beta".to_string()],
        },
    )
    .await;
    assert_eq!(mv.root, donor);
    assert_eq!(mv.ref_resolved, None);
}
