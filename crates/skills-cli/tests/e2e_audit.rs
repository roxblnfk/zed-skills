//! End-to-end audit-pipeline tests through the `skills` binary.
//!
//! Fixture `audit`: `clean-src/tidy` passes every static check;
//! `danger-src/payload` ships a `curl | bash` install script (one blocking
//! `static` finding).

mod common;

use assert_cmd::Command;
use common::{fixture_project, tree_fingerprint};
use skills_core::lockfile::Lockfile;

fn skills_cmd(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("skills").unwrap();
    cmd.current_dir(dir);
    cmd
}

fn stdout_of(assert: assert_cmd::assert::Assert) -> String {
    String::from_utf8(assert.get_output().stdout.clone()).unwrap()
}

fn stderr_of(assert: assert_cmd::assert::Assert) -> String {
    String::from_utf8(assert.get_output().stderr.clone()).unwrap()
}

/// Rewrite the fixture manifest with the given donor dirs and audit section.
fn write_manifest(project: &std::path::Path, dirs: &[&str], audit: &str) {
    let dirs = dirs
        .iter()
        .map(|d| format!("\"{d}\""))
        .collect::<Vec<_>>()
        .join(", ");
    std::fs::write(
        project.join("skills.json"),
        format!(r#"{{ "local": {{ "dir": [{dirs}] }}, "audit": {audit} }}"#),
    )
    .unwrap();
}

const FINDING_MARKER: &str = "curl-pipe-shell";

// --- mode matrix -------------------------------------------------------------

#[test]
fn warn_mode_syncs_with_audit_warnings() {
    let project = fixture_project("audit");
    let out = stdout_of(skills_cmd(project.path()).arg("update").assert().success());
    assert!(
        out.contains("[audit] warn static: curl-pipe-shell"),
        "{out}"
    );
    assert!(out.contains("scripts/install.sh:2"), "{out}");
    // The dangerous skill still syncs under mode warn.
    let target = project.path().join(".agents").join("skills");
    assert!(target.join("payload").join("SKILL.md").is_file());
    assert!(target.join("tidy").join("SKILL.md").is_file());
    insta::assert_snapshot!("audit_warn_stdout", out);
}

#[test]
fn block_mode_aborts_exit_3_with_fs_untouched() {
    let project = fixture_project("audit");
    write_manifest(
        project.path(),
        &["./clean-src", "./danger-src"],
        r#"{ "mode": "block" }"#,
    );
    let before = tree_fingerprint(project.path());

    let assert = skills_cmd(project.path())
        .arg("update")
        .assert()
        .failure()
        .code(3);
    let stderr = stderr_of(assert);
    assert!(stderr.contains("audit blocked skill 'payload'"), "{stderr}");
    assert!(stderr.contains(FINDING_MARKER), "{stderr}");

    // Target and lockfile untouched — nothing (not even the clean skill)
    // was written.
    assert_eq!(before, tree_fingerprint(project.path()));
    assert!(!project.path().join(".agents").exists());
    assert!(!project.path().join("skills.lock").exists());
}

#[test]
fn off_mode_is_silent_and_stores_no_verdicts() {
    let project = fixture_project("audit");
    write_manifest(
        project.path(),
        &["./clean-src", "./danger-src"],
        r#"{ "mode": "off" }"#,
    );
    let out = stdout_of(skills_cmd(project.path()).arg("update").assert().success());
    assert!(!out.contains("[audit]"), "{out}");

    let lock = Lockfile::load(&project.path().join("skills.lock"))
        .unwrap()
        .unwrap();
    assert!(lock.skills.iter().all(|s| s.audit.is_none()));
}

#[test]
fn clean_skill_under_block_mode_syncs_fine() {
    let project = fixture_project("audit");
    write_manifest(project.path(), &["./clean-src"], r#"{ "mode": "block" }"#);
    let out = stdout_of(skills_cmd(project.path()).arg("update").assert().success());
    assert!(!out.contains("[audit]"), "{out}");
    assert!(
        project
            .path()
            .join(".agents")
            .join("skills")
            .join("tidy")
            .join("SKILL.md")
            .is_file()
    );
    // A passing verdict is cached too.
    let lock = Lockfile::load(&project.path().join("skills.lock"))
        .unwrap()
        .unwrap();
    assert_eq!(lock.skills[0].audit.as_ref().unwrap().verdict, "pass");
}

// --- verdict cache -----------------------------------------------------------

#[test]
fn verdict_is_cached_across_runs_and_re_audit_forces() {
    let project = fixture_project("audit");

    // Run 1: full audit, findings rendered, verdict cached in the lock.
    let out1 = stdout_of(skills_cmd(project.path()).arg("update").assert().success());
    assert!(out1.contains(FINDING_MARKER), "{out1}");
    assert!(!out1.contains("cached verdict"), "{out1}");
    let lock = Lockfile::load(&project.path().join("skills.lock"))
        .unwrap()
        .unwrap();
    let payload = lock.skills.iter().find(|s| s.id == "payload").unwrap();
    let cached = payload.audit.as_ref().unwrap();
    // The true aggregate verdict is stored, independent of the mode.
    assert_eq!(cached.verdict, "block");
    assert_eq!(cached.auditor_set_hash.len(), 64);

    // Run 2: cache hit — no re-audit, findings not re-rendered.
    let out2 = stdout_of(skills_cmd(project.path()).arg("update").assert().success());
    assert!(!out2.contains(FINDING_MARKER), "{out2}");
    assert!(
        out2.contains("[audit] cached verdict: warn (--re-audit to recheck)"),
        "{out2}"
    );

    // Run 3: --re-audit bypasses the cache, findings come back.
    let out3 = stdout_of(
        skills_cmd(project.path())
            .args(["update", "--re-audit"])
            .assert()
            .success(),
    );
    assert!(out3.contains(FINDING_MARKER), "{out3}");
    assert!(!out3.contains("cached verdict"), "{out3}");
}

#[test]
fn pipeline_change_invalidates_the_cache() {
    let project = fixture_project("audit");
    skills_cmd(project.path()).arg("update").assert().success();

    // Same auditors, different on-fail: the auditor-set hash changes.
    write_manifest(
        project.path(),
        &["./clean-src", "./danger-src"],
        r#"{ "mode": "warn", "pipeline": [ { "use": "static", "on-fail": "warn" } ] }"#,
    );
    let out = stdout_of(skills_cmd(project.path()).arg("update").assert().success());
    assert!(out.contains(FINDING_MARKER), "{out}");
    assert!(!out.contains("cached verdict"), "{out}");
    // Downgraded by on-fail=warn: the cached verdict is now warn.
    let lock = Lockfile::load(&project.path().join("skills.lock"))
        .unwrap()
        .unwrap();
    let payload = lock.skills.iter().find(|s| s.id == "payload").unwrap();
    assert_eq!(payload.audit.as_ref().unwrap().verdict, "warn");
}

#[test]
fn cached_block_verdict_enforced_when_mode_switches_to_block() {
    let project = fixture_project("audit");
    // Warn run caches the 'block' verdict and syncs.
    skills_cmd(project.path()).arg("update").assert().success();

    write_manifest(
        project.path(),
        &["./clean-src", "./danger-src"],
        r#"{ "mode": "block" }"#,
    );
    let before = tree_fingerprint(project.path());
    let assert = skills_cmd(project.path())
        .arg("update")
        .assert()
        .failure()
        .code(3);
    let stderr = stderr_of(assert);
    assert!(stderr.contains("cached verdict 'block'"), "{stderr}");
    assert!(stderr.contains("--re-audit"), "{stderr}");
    // Abort leaves the previously synced tree and lockfile untouched.
    assert_eq!(before, tree_fingerprint(project.path()));
}

#[test]
fn on_fail_warn_downgrade_lets_block_mode_sync() {
    let project = fixture_project("audit");
    write_manifest(
        project.path(),
        &["./clean-src", "./danger-src"],
        r#"{ "mode": "block", "pipeline": [ { "use": "static", "on-fail": "warn" } ] }"#,
    );
    let out = stdout_of(skills_cmd(project.path()).arg("update").assert().success());
    assert!(
        out.contains("[audit] warn static: curl-pipe-shell"),
        "{out}"
    );
    assert!(
        project
            .path()
            .join(".agents")
            .join("skills")
            .join("payload")
            .join("SKILL.md")
            .is_file()
    );
}

// --- show ---------------------------------------------------------------------

#[test]
fn show_surfaces_cached_verdicts() {
    let project = fixture_project("audit");
    skills_cmd(project.path()).arg("update").assert().success();
    let out = stdout_of(skills_cmd(project.path()).arg("show").assert().success());
    let payload_line = out
        .lines()
        .find(|l| l.trim_start().starts_with("payload"))
        .unwrap();
    assert!(payload_line.contains("[audit: block]"), "{out}");
    let tidy_line = out
        .lines()
        .find(|l| l.trim_start().starts_with("tidy"))
        .unwrap();
    assert!(!tidy_line.contains("[audit:"), "{out}");
}

// --- config errors -------------------------------------------------------------

#[test]
fn unknown_auditor_in_config_is_exit_1() {
    let project = fixture_project("audit");
    write_manifest(
        project.path(),
        &["./clean-src"],
        r#"{ "mode": "warn", "pipeline": [ { "use": "voodoo" } ] }"#,
    );
    let assert = skills_cmd(project.path())
        .arg("update")
        .assert()
        .failure()
        .code(1);
    let stderr = stderr_of(assert);
    assert!(stderr.contains("unknown auditor 'voodoo'"), "{stderr}");
}

#[test]
fn llm_and_http_stubs_are_exit_1_when_audit_is_on() {
    let project = fixture_project("audit");
    for auditor in ["llm", "http"] {
        write_manifest(
            project.path(),
            &["./clean-src"],
            &format!(r#"{{ "mode": "warn", "pipeline": [ {{ "use": "{auditor}" }} ] }}"#),
        );
        let assert = skills_cmd(project.path())
            .arg("update")
            .assert()
            .failure()
            .code(1);
        let stderr = stderr_of(assert);
        assert!(
            stderr.contains("not implemented yet, coming in a future release"),
            "{stderr}"
        );
    }

    // With mode off the same entries are tolerated (pre-staged config).
    write_manifest(
        project.path(),
        &["./clean-src"],
        r#"{ "mode": "off", "pipeline": [ { "use": "llm" } ] }"#,
    );
    skills_cmd(project.path()).arg("update").assert().success();
}

// --- idempotency under audit ----------------------------------------------------

#[test]
fn second_run_is_idempotent_including_lock_bytes() {
    let project = fixture_project("audit");
    skills_cmd(project.path()).arg("update").assert().success();
    let lock1 = std::fs::read(project.path().join("skills.lock")).unwrap();
    skills_cmd(project.path()).arg("update").assert().success();
    let lock2 = std::fs::read(project.path().join("skills.lock")).unwrap();
    assert_eq!(
        lock1, lock2,
        "cached verdict must round-trip byte-identically"
    );
}
