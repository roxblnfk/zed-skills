//! Pipeline invariants: idempotency, abort-leaves-FS-untouched, dry-run
//! writes nothing, non-destructive merge, pruning.

mod common;

use common::{fixture_project, tree_fingerprint, update};
use skills_core::error::{PipelineError, ResolveError};
use skills_core::lockfile::{Lockfile, SyncStatus, sync_status};
use skills_core::pipeline::SyncAction;

#[tokio::test]
async fn update_is_idempotent_tree_and_lock_bytes() {
    let project = fixture_project("basic");
    update(project.path(), false).await.unwrap();
    let tree1 = tree_fingerprint(project.path());
    let lock1 = std::fs::read(project.path().join("skills.lock")).unwrap();

    update(project.path(), false).await.unwrap();
    let tree2 = tree_fingerprint(project.path());
    let lock2 = std::fs::read(project.path().join("skills.lock")).unwrap();

    assert_eq!(tree1, tree2, "second update must not change any byte");
    assert_eq!(lock1, lock2, "lockfile must be byte-identical");
}

#[tokio::test]
async fn conflict_aborts_before_any_write() {
    let project = fixture_project("conflict");
    let before = tree_fingerprint(project.path());

    let err = update(project.path(), false).await.unwrap_err();
    let PipelineError::Resolve(ResolveError::Conflict(conflicts)) = err else {
        panic!("expected a conflict, got: {err}");
    };
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].ids.len(), 1);
    assert_eq!(conflicts[0].ids[0].as_str(), "clashing");
    let vendors: Vec<&str> = conflicts[0].vendors.iter().map(|v| v.as_str()).collect();
    assert_eq!(vendors, ["dir/vendor-a", "dir/vendor-b"]);

    // Filesystem untouched: no target, no lockfile, nothing changed.
    let after = tree_fingerprint(project.path());
    assert_eq!(
        before, after,
        "conflict must leave the filesystem untouched"
    );
    assert!(!project.path().join(".agents").exists());
    assert!(!project.path().join("skills.lock").exists());
}

/// Build a project in a tempdir at test runtime. Case-collision and
/// FS-dangerous skill dir names cannot live in a committed fixture (git on
/// Windows cannot even check them out), so these projects are constructed
/// programmatically.
fn runtime_project(donors: &[(&str, &[&str])]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = donors
        .iter()
        .map(|(donor, _)| format!("{{ \"from\": \"dir\", \"path\": \"./{donor}\" }}"))
        .collect::<Vec<_>>()
        .join(", ");
    std::fs::write(
        tmp.path().join("skills.json"),
        format!("{{ \"sources\": [{dirs}] }}\n"),
    )
    .unwrap();
    for (donor, skills) in donors {
        for skill in *skills {
            let dir = tmp.path().join(donor).join(skill);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\ndescription: {skill}\n---\n"),
            )
            .unwrap();
        }
    }
    tmp
}

#[tokio::test]
async fn case_variant_conflict_aborts_before_any_write() {
    // `Foo` and `foo` from two donors merge into one directory on
    // case-insensitive filesystems — a conflict on the normalized key.
    let project = runtime_project(&[("donor-a", &["Foo"]), ("donor-b", &["foo"])]);
    let before = tree_fingerprint(project.path());

    let err = update(project.path(), false).await.unwrap_err();
    let PipelineError::Resolve(ResolveError::Conflict(conflicts)) = err else {
        panic!("expected a conflict, got: {err}");
    };
    assert_eq!(conflicts.len(), 1);
    let ids: Vec<&str> = conflicts[0].ids.iter().map(|id| id.as_str()).collect();
    assert_eq!(ids, ["Foo", "foo"], "both original spellings listed");
    let vendors: Vec<&str> = conflicts[0].vendors.iter().map(|v| v.as_str()).collect();
    assert_eq!(vendors, ["dir/donor-a", "dir/donor-b"]);

    let after = tree_fingerprint(project.path());
    assert_eq!(
        before, after,
        "conflict must leave the filesystem untouched"
    );
    assert!(!project.path().join(".agents").exists());
    assert!(!project.path().join("skills.lock").exists());
}

#[tokio::test]
async fn dangerous_dir_name_aborts_before_any_write() {
    // U+007F (DEL) is the one FS-dangerous character a donor dir can carry
    // on every OS including Windows (reserved device names, trailing
    // dots/spaces and `<>:"/\|?*` cannot exist on an NTFS checkout at all —
    // those are covered by the resolve-stage unit matrix).
    let bad = "bad\u{7f}name";
    let project = runtime_project(&[("donor", &["fine", bad])]);
    let before = tree_fingerprint(project.path());

    let err = update(project.path(), false).await.unwrap_err();
    let PipelineError::Resolve(ResolveError::DangerousName(dangerous)) = err else {
        panic!("expected a dangerous-name abort, got: {err}");
    };
    assert_eq!(dangerous.len(), 1);
    assert_eq!(dangerous[0].id.as_str(), bad);
    assert_eq!(dangerous[0].vendor.as_str(), "dir/donor");
    assert!(
        dangerous[0].reason.contains("control character (U+007F)"),
        "{}",
        dangerous[0].reason
    );

    let after = tree_fingerprint(project.path());
    assert_eq!(
        before, after,
        "dangerous name must leave the filesystem untouched"
    );
    assert!(!project.path().join(".agents").exists());
    assert!(!project.path().join("skills.lock").exists());
}

#[tokio::test]
async fn dry_run_reports_conflicts_identically() {
    let project = fixture_project("conflict");
    let err = update(project.path(), true).await.unwrap_err();
    assert!(matches!(
        err,
        PipelineError::Resolve(ResolveError::Conflict(_))
    ));
}

#[tokio::test]
async fn dry_run_writes_nothing() {
    let project = fixture_project("basic");
    let before = tree_fingerprint(project.path());

    let report = update(project.path(), true).await.unwrap();
    assert!(report.dry_run);
    assert_eq!(report.count(SyncAction::Add), 3);

    assert_eq!(before, tree_fingerprint(project.path()));
    assert!(!project.path().join(".agents").exists());
    assert!(!project.path().join("skills.lock").exists());
}

#[tokio::test]
async fn user_added_file_survives_resync_and_is_not_drift() {
    let project = fixture_project("basic");
    update(project.path(), false).await.unwrap();

    let skill_dir = project
        .path()
        .join(".agents")
        .join("skills")
        .join("code-review");
    std::fs::write(skill_dir.join("my-notes.md"), "user content").unwrap();
    std::fs::create_dir_all(skill_dir.join("my-dir")).unwrap();
    std::fs::write(skill_dir.join("my-dir").join("extra.md"), "more").unwrap();

    // Force an update of this skill so merge logic actually runs.
    let donor_md = project
        .path()
        .join("skills-src")
        .join("code-review")
        .join("SKILL.md");
    std::fs::write(&donor_md, "---\nname: code-review\n---\nv2\n").unwrap();
    let report = update(project.path(), false).await.unwrap();
    assert_eq!(report.count(SyncAction::Update), 1);

    assert_eq!(
        std::fs::read_to_string(skill_dir.join("my-notes.md")).unwrap(),
        "user content"
    );
    assert_eq!(
        std::fs::read_to_string(skill_dir.join("my-dir").join("extra.md")).unwrap(),
        "more"
    );

    // User-added files are not drift.
    let lock = Lockfile::load(&project.path().join("skills.lock"))
        .unwrap()
        .unwrap();
    let locked = lock.skills.iter().find(|s| s.id == "code-review").unwrap();
    assert_eq!(sync_status(&skill_dir, locked), SyncStatus::Synced);
}

#[tokio::test]
async fn file_dropped_by_donor_is_pruned() {
    let project = fixture_project("basic");
    update(project.path(), false).await.unwrap();

    let donor_ref = project
        .path()
        .join("skills-src")
        .join("code-review")
        .join("references")
        .join("checklist.md");
    std::fs::remove_file(&donor_ref).unwrap();

    update(project.path(), false).await.unwrap();

    let target_skill = project
        .path()
        .join(".agents")
        .join("skills")
        .join("code-review");
    assert!(
        !target_skill.join("references").exists(),
        "stale file + empty dir pruned"
    );
    assert!(target_skill.join("SKILL.md").is_file());
    assert!(target_skill.join("scripts").join("run.ps1").is_file());
}

#[tokio::test]
async fn skill_removed_from_donor_is_pruned() {
    let project = fixture_project("basic");
    update(project.path(), false).await.unwrap();

    let donor_skill = project.path().join("skills-src").join("plain");
    std::fs::remove_dir_all(&donor_skill).unwrap();

    let report = update(project.path(), false).await.unwrap();
    assert_eq!(report.count(SyncAction::Remove), 1);

    let target_skill = project.path().join(".agents").join("skills").join("plain");
    assert!(!target_skill.exists(), "pruned skill dir should be gone");

    let lock = Lockfile::load(&project.path().join("skills.lock"))
        .unwrap()
        .unwrap();
    assert!(lock.skills.iter().all(|s| s.id != "plain"));
}

#[tokio::test]
async fn user_file_in_removed_skill_dir_survives_pruning() {
    let project = fixture_project("basic");
    update(project.path(), false).await.unwrap();

    let target_skill = project.path().join(".agents").join("skills").join("plain");
    std::fs::write(target_skill.join("keep-me.txt"), "user file").unwrap();

    std::fs::remove_dir_all(project.path().join("skills-src").join("plain")).unwrap();
    update(project.path(), false).await.unwrap();

    // Lock-listed files gone, user file (and thus the dir) kept.
    assert!(!target_skill.join("SKILL.md").exists());
    assert_eq!(
        std::fs::read_to_string(target_skill.join("keep-me.txt")).unwrap(),
        "user file"
    );
}

#[tokio::test]
async fn donor_removed_from_manifest_prunes_its_skills() {
    let project = fixture_project("basic");
    update(project.path(), false).await.unwrap();

    // Drop the donor from the manifest entirely.
    std::fs::write(project.path().join("skills.json"), "{}\n").unwrap();
    let report = update(project.path(), false).await.unwrap();
    assert_eq!(report.count(SyncAction::Remove), 3);

    let target = project.path().join(".agents").join("skills");
    assert!(!target.join("code-review").exists());
    assert!(!target.join("docs-helper").exists());
    assert!(!target.join("plain").exists());
    let lock = Lockfile::load(&project.path().join("skills.lock"))
        .unwrap()
        .unwrap();
    assert!(lock.skills.is_empty());
}
