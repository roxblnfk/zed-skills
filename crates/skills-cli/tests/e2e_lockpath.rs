//! End-to-end tests for the `lock-file` manifest option through the actual
//! binary: the lockfile is read from and written to the configured path.
//!
//! Note: this test binary must not contain "update" in its file name
//! (Windows UAC installer detection, os error 740).

mod common;

use assert_cmd::Command;
use common::fixture_project;

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
fn sync_writes_lock_at_configured_path_not_root() {
    let project = fixture_project("lockpath");
    skills_cmd(project.path()).arg("update").assert().success();

    let lock = project.path().join(".agents").join("skills.lock");
    assert!(lock.is_file(), "lock must land at the configured path");
    assert!(
        !project.path().join("skills.lock").exists(),
        "no lockfile at the project root"
    );
    assert!(
        project
            .path()
            .join(".agents")
            .join("skills")
            .join("code-review")
            .join("SKILL.md")
            .is_file()
    );
}

#[test]
fn second_run_is_idempotent_with_custom_lock_path() {
    let project = fixture_project("lockpath");
    skills_cmd(project.path()).arg("update").assert().success();
    let lock_path = project.path().join(".agents").join("skills.lock");
    let first = std::fs::read(&lock_path).unwrap();

    let out = stdout_of(&skills_cmd(project.path()).arg("update").assert().success());
    assert!(out.contains("2 up to date"), "{out}");
    assert_eq!(
        first,
        std::fs::read(&lock_path).unwrap(),
        "second run must not change the lock"
    );
    assert!(!project.path().join("skills.lock").exists());
}

#[test]
fn check_reads_the_configured_lock() {
    let project = fixture_project("lockpath");
    skills_cmd(project.path()).arg("update").assert().success();

    // In sync: --check must find the lock at the configured path (exit 0).
    let assert = skills_cmd(project.path())
        .args(["update", "--check"])
        .assert()
        .success();
    assert_eq!(stdout_of(&assert), "skills: up to date (2 skills)\n");
}

#[test]
fn show_reads_the_configured_lock() {
    let project = fixture_project("lockpath");
    skills_cmd(project.path()).arg("update").assert().success();
    let out = stdout_of(&skills_cmd(project.path()).arg("show").assert().success());
    assert!(out.contains("[ok]"), "{out}");
    assert!(!out.contains("not-synced"), "{out}");
}

#[test]
fn moving_the_option_ignores_the_old_root_lock() {
    // First sync with the default location → skills.lock at the root.
    let project = fixture_project("lockpath");
    std::fs::write(
        project.path().join("skills.json"),
        r#"{ "target": ".agents/skills", "local": { "dir": ["./skills-src"] } }"#,
    )
    .unwrap();
    skills_cmd(project.path()).arg("update").assert().success();
    let old_lock = project.path().join("skills.lock");
    let old_bytes = std::fs::read(&old_lock).unwrap();

    // The manifest now points elsewhere: the old lock is IGNORED (treated as
    // absent — no auto-migration), so everything plans as add again.
    std::fs::write(
        project.path().join("skills.json"),
        r#"{
    "target": ".agents/skills",
    "lock-file": ".agents/skills.lock",
    "local": {
        "dir": ["./skills-src"]
    }
}
"#,
    )
    .unwrap();
    let out = stdout_of(&skills_cmd(project.path()).arg("update").assert().success());
    insta::assert_snapshot!("bin_lock_moved_stdout", out);

    // New lock written at the configured path; the old root lock is left
    // behind untouched (the user deletes or commits it).
    assert!(project.path().join(".agents").join("skills.lock").is_file());
    assert_eq!(
        old_bytes,
        std::fs::read(&old_lock).unwrap(),
        "old root lock must survive byte-identical"
    );
}

#[test]
fn lock_file_equal_to_target_is_config_error() {
    let project = fixture_project("lockpath");
    std::fs::write(
        project.path().join("skills.json"),
        r#"{ "target": ".agents/skills", "lock-file": ".agents/skills", "local": { "dir": ["./skills-src"] } }"#,
    )
    .unwrap();
    let assert = skills_cmd(project.path())
        .arg("update")
        .assert()
        .failure()
        .code(1);
    assert!(
        stderr_of(&assert).contains("must not equal the target"),
        "{}",
        stderr_of(&assert)
    );
    // Config error before any write.
    assert!(!project.path().join(".agents").exists());
}

#[test]
fn lock_file_escaping_root_is_config_error() {
    let project = fixture_project("lockpath");
    std::fs::write(
        project.path().join("skills.json"),
        r#"{ "lock-file": "../skills.lock", "local": { "dir": ["./skills-src"] } }"#,
    )
    .unwrap();
    let assert = skills_cmd(project.path())
        .arg("update")
        .assert()
        .failure()
        .code(1);
    assert!(
        stderr_of(&assert).contains("invalid lock-file"),
        "{}",
        stderr_of(&assert)
    );
    assert!(!project.path().join("..").join("skills.lock").exists());
    assert!(!project.path().join(".agents").exists());
}
