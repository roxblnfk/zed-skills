//! End-to-end tests for `skills update --check` through the actual binary.
//!
//! Note: this test binary must not contain "update" in its file name
//! (Windows UAC installer detection, os error 740).

mod common;

use assert_cmd::Command;
use common::{fixture_project, tree_fingerprint};

fn skills_cmd(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("skills").unwrap();
    cmd.current_dir(dir);
    cmd
}

fn stdout_of(assert: &assert_cmd::assert::Assert) -> String {
    String::from_utf8(assert.get_output().stdout.clone()).unwrap()
}

fn stderr_of(assert: &assert_cmd::assert::Assert) -> String {
    String::from_utf8(assert.get_output().stderr.clone()).unwrap()
}

#[test]
fn check_out_of_sync_exits_5_and_writes_nothing() {
    let project = fixture_project("basic");
    let before = tree_fingerprint(project.path());

    let assert = skills_cmd(project.path())
        .args(["update", "--check"])
        .assert()
        .failure()
        .code(5);
    insta::assert_snapshot!("bin_check_out_of_sync_stdout", stdout_of(&assert));
    // Exit 5 is a status, not an error: stderr stays silent.
    assert_eq!(stderr_of(&assert), "");

    // FS untouched: no target, no lockfile, byte-identical project tree.
    assert!(!project.path().join(".agents").exists());
    assert!(!project.path().join("skills.lock").exists());
    assert_eq!(before, tree_fingerprint(project.path()));
}

#[test]
fn check_in_sync_exits_0() {
    let project = fixture_project("basic");
    skills_cmd(project.path()).arg("update").assert().success();

    let before = tree_fingerprint(project.path());
    let assert = skills_cmd(project.path())
        .args(["update", "--check"])
        .assert()
        .success();
    assert_eq!(stdout_of(&assert), "skills: up to date (3 skills)\n");
    assert_eq!(before, tree_fingerprint(project.path()));
}

#[test]
fn check_detects_source_drift_as_update_and_remove() {
    let project = fixture_project("basic");
    skills_cmd(project.path()).arg("update").assert().success();

    // Donor drift while "the file was closed": one skill edited, one gone.
    let src = project.path().join("skills-src");
    std::fs::write(src.join("plain").join("SKILL.md"), "# changed upstream\n").unwrap();
    std::fs::remove_dir_all(src.join("docs-helper")).unwrap();

    let assert = skills_cmd(project.path())
        .args(["update", "--check"])
        .assert()
        .failure()
        .code(5);
    let out = stdout_of(&assert);
    assert!(
        out.contains("1 to update, 1 to remove (1 up to date)"),
        "{out}"
    );
    assert!(out.contains("~ plain (dir/skills-src)"), "{out}");
    assert!(out.contains("- docs-helper (dir/skills-src)"), "{out}");
    assert!(out.contains("run `skills update` to apply"), "{out}");

    // Still read-only: the synced target keeps the pre-drift content.
    let target = project.path().join(".agents").join("skills");
    assert!(target.join("docs-helper").join("SKILL.md").is_file());
    assert_ne!(
        std::fs::read_to_string(target.join("plain").join("SKILL.md")).unwrap(),
        "# changed upstream\n"
    );
}

#[test]
fn check_conflict_exits_2_and_writes_nothing() {
    let project = fixture_project("conflict");
    let assert = skills_cmd(project.path())
        .args(["update", "--check"])
        .assert()
        .failure()
        .code(2);
    assert!(stderr_of(&assert).contains("clashing"));
    assert!(!project.path().join(".agents").exists());
    assert!(!project.path().join("skills.lock").exists());
}

#[test]
fn check_conflicts_with_dry_run_flag_is_usage_error() {
    let project = fixture_project("basic");
    let assert = skills_cmd(project.path())
        .args(["update", "--check", "--dry-run"])
        .assert()
        .failure()
        .code(1);
    let err = stderr_of(&assert);
    assert!(
        err.contains("--check") && err.contains("--dry-run"),
        "{err}"
    );
    assert!(!project.path().join(".agents").exists());
}

#[test]
fn check_accepts_positional_package_filter() {
    let project = fixture_project("basic");
    skills_cmd(project.path()).arg("update").assert().success();
    // Scoped check: naming the donor keeps --check exit semantics.
    let assert = skills_cmd(project.path())
        .args(["update", "--check", "dir/skills-src"])
        .assert()
        .success();
    assert_eq!(stdout_of(&assert), "skills: up to date (3 skills)\n");
}
