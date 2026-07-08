//! End-to-end tests for local `dir` donors through the `skills` binary:
//! `skills add ./path` registration + sync, ref rejection, missing-dir
//! handling, and an outward (`../`) dir source. All offline.

mod common;

use std::io::Write as _;
use std::path::Path;

use assert_cmd::Command;

fn skills_cmd(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("skills").unwrap();
    cmd.current_dir(dir);
    cmd
}

/// Create `<parent>/<name>/SKILL.md` with a minimal valid frontmatter.
fn write_skill(parent: &Path, name: &str) {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let mut f = std::fs::File::create(dir.join("SKILL.md")).unwrap();
    writeln!(
        f,
        "---\nname: {name}\ndescription: A local skill\n---\nBody"
    )
    .unwrap();
}

#[test]
fn add_local_dir_registers_and_syncs() {
    let project = tempfile::tempdir().unwrap();
    // A donor directory shipping one skill.
    write_skill(&project.path().join("local-skills"), "local-skill");

    let assert = skills_cmd(project.path())
        .args(["add", "./local-skills"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    // No ref suffix for a local directory; the `./` prefix is normalized away.
    assert!(stdout.contains("Registered dir:local-skills"), "{stdout}");

    // Manifest entry: dir + path only, no ref/host/package/skills.
    let manifest = std::fs::read_to_string(project.path().join("skills.json")).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    assert_eq!(doc["$schema"], skills_core::manifest::SCHEMA_URL);
    let sources = doc["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 1, "{manifest}");
    let entry = &sources[0];
    assert_eq!(entry["from"], "dir");
    assert_eq!(entry["path"], "local-skills");
    assert!(entry.get("ref").is_none(), "no ref for dir: {manifest}");
    assert!(entry.get("host").is_none(), "no host for dir: {manifest}");
    assert!(entry.get("package").is_none(), "no package: {manifest}");
    assert!(entry.get("skills").is_none(), "no allowlist: {manifest}");

    // Synced into the target + lockfile written.
    let target = project.path().join(".agents").join("skills");
    assert!(target.join("local-skill").join("SKILL.md").is_file());
    assert!(project.path().join("skills.lock").is_file());

    // A second identical add is a no-op upsert: still exactly one entry.
    skills_cmd(project.path())
        .args(["add", "./local-skills"])
        .assert()
        .success();
    let manifest = std::fs::read_to_string(project.path().join("skills.json")).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    assert_eq!(
        doc["sources"].as_array().unwrap().len(),
        1,
        "re-add must not duplicate the entry: {manifest}"
    );
}

#[test]
fn add_missing_dir_is_config_error() {
    let project = tempfile::tempdir().unwrap();
    let assert = skills_cmd(project.path())
        .args(["add", "./missing"])
        .assert()
        .failure()
        .code(1);
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("does not exist"), "{stderr}");
    // Nothing registered, nothing synced.
    assert!(!project.path().join("skills.json").exists());
    assert!(!project.path().join(".agents").exists());
}

#[test]
fn add_dir_with_ref_is_rejected() {
    let project = tempfile::tempdir().unwrap();
    write_skill(&project.path().join("local-skills"), "local-skill");

    let assert = skills_cmd(project.path())
        .args(["add", "./local-skills", "--ref", "v1"])
        .assert()
        .failure()
        .code(1);
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("not applicable to a dir source"),
        "{stderr}"
    );
    // Rejected before any write.
    assert!(!project.path().join("skills.json").exists());
    assert!(!project.path().join(".agents").exists());
}

#[test]
fn add_dir_without_skills_is_refused() {
    let project = tempfile::tempdir().unwrap();
    // An existing but skill-less directory.
    std::fs::create_dir_all(project.path().join("empty-dir")).unwrap();

    let assert = skills_cmd(project.path())
        .args(["add", "./empty-dir"])
        .assert()
        .failure()
        .code(4);
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("refusing to register"), "{stderr}");
    assert!(!project.path().join("skills.json").exists());
}

#[test]
fn outward_dir_source_syncs() {
    // The donor lives OUTSIDE the project root (`../shared-skills`); an
    // outward read must sync normally.
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    write_skill(&tmp.path().join("shared-skills"), "shared-skill");
    std::fs::write(
        project.join("skills.json"),
        r#"{ "sources": [ { "from": "dir", "path": "../shared-skills" } ] }"#,
    )
    .unwrap();

    skills_cmd(&project).arg("update").assert().success();
    assert!(
        project
            .join(".agents")
            .join("skills")
            .join("shared-skill")
            .join("SKILL.md")
            .is_file()
    );
}
