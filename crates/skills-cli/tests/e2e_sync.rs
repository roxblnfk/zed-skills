//! End-to-end pipeline tests through the library API, with insta snapshots
//! of the resulting tree and lockfile.

mod common;

use common::{fixture_project, tree_listing, update};
use skills_core::pipeline::SyncAction;

#[tokio::test]
async fn update_copies_skills_from_local_dir() {
    let project = fixture_project("basic");
    let report = update(project.path(), false).await.unwrap();

    assert_eq!(report.count(SyncAction::Add), 3);
    assert_eq!(report.count(SyncAction::Remove), 0);
    assert!(!report.dry_run);

    let target = project.path().join(".agents").join("skills");
    insta::assert_snapshot!("basic_target_tree", tree_listing(&target).join("\n"));

    // Contents are preserved byte-for-byte.
    let src = project
        .path()
        .join("skills-src")
        .join("code-review")
        .join("references")
        .join("checklist.md");
    let dst = target
        .join("code-review")
        .join("references")
        .join("checklist.md");
    assert_eq!(
        std::fs::read(&src).unwrap(),
        std::fs::read(&dst).unwrap(),
        "nested file must be copied verbatim"
    );
}

#[tokio::test]
async fn lockfile_snapshot() {
    let project = fixture_project("basic");
    update(project.path(), false).await.unwrap();

    let lock_raw = std::fs::read_to_string(project.path().join("skills.lock")).unwrap();
    // Lock must be machine-independent: no temp paths inside.
    let temp_path = project.path().to_string_lossy().replace('\\', "/");
    assert!(
        !lock_raw.replace('\\', "/").contains(&temp_path),
        "lockfile must not embed absolute paths"
    );
    insta::assert_snapshot!("basic_lockfile", common::redact_lock(&lock_raw));
}

#[tokio::test]
async fn second_update_skips_everything() {
    let project = fixture_project("basic");
    update(project.path(), false).await.unwrap();
    let report = update(project.path(), false).await.unwrap();
    assert_eq!(report.count(SyncAction::Add), 0);
    assert_eq!(report.count(SyncAction::Update), 0);
    assert_eq!(report.count(SyncAction::Skip), 3);
}

#[tokio::test]
async fn changed_donor_file_updates_skill() {
    let project = fixture_project("basic");
    update(project.path(), false).await.unwrap();

    let donor_file = project
        .path()
        .join("skills-src")
        .join("docs-helper")
        .join("SKILL.md");
    std::fs::write(&donor_file, "---\nname: docs-helper\n---\nv2\n").unwrap();

    let report = update(project.path(), false).await.unwrap();
    assert_eq!(report.count(SyncAction::Update), 1);
    assert_eq!(report.count(SyncAction::Skip), 2);

    let synced = project
        .path()
        .join(".agents")
        .join("skills")
        .join("docs-helper")
        .join("SKILL.md");
    assert!(std::fs::read_to_string(&synced).unwrap().contains("v2"));
}

#[tokio::test]
async fn frontmatter_flows_into_scan_results() {
    // The "plain" skill has no frontmatter: canonical name falls back to the
    // dir name and the skill is still synced (best-effort reader).
    let project = fixture_project("basic");
    update(project.path(), false).await.unwrap();
    let target = project.path().join(".agents").join("skills");
    assert!(target.join("plain").join("SKILL.md").is_file());
    assert!(target.join("code-review").join("SKILL.md").is_file());
}
