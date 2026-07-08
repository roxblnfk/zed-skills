//! End-to-end tests through the actual `skills` binary (assert_cmd).

mod common;

use assert_cmd::Command;
use common::fixture_project;
use skills_core::manifest::Manifest;

fn skills_cmd(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("skills").unwrap();
    cmd.current_dir(dir);
    cmd
}

fn stdout_of(assert: assert_cmd::assert::Assert) -> String {
    String::from_utf8(assert.get_output().stdout.clone()).unwrap()
}

// --- Standalone scenarios -------------------------------------------------

#[test]
fn init_works_in_empty_dir_without_composer_json() {
    let tmp = tempfile::tempdir().unwrap();
    skills_cmd(tmp.path()).arg("init").assert().success();
    let raw = std::fs::read_to_string(tmp.path().join("skills.json")).unwrap();
    // The stub must be a valid manifest and point at the published schema.
    let manifest = Manifest::parse(&raw).unwrap();
    assert_eq!(
        manifest.schema.as_deref(),
        Some(skills_core::manifest::SCHEMA_URL)
    );
    // The archive cache is gitignored from the start.
    let gitignore = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
    assert!(
        gitignore.lines().any(|l| l == ".skills-cache/"),
        "{gitignore}"
    );
}

#[test]
fn init_appends_cache_entry_to_existing_gitignore() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/target\n").unwrap();
    skills_cmd(tmp.path()).arg("init").assert().success();
    let gitignore = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
    assert!(gitignore.starts_with("/target\n"), "{gitignore}");
    assert!(gitignore.contains(".skills-cache/"), "{gitignore}");

    // Idempotent: a second init (--force) does not duplicate the entry.
    skills_cmd(tmp.path())
        .args(["init", "--force"])
        .assert()
        .success();
    let again = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
    assert_eq!(gitignore, again);
}

#[test]
fn init_refuses_overwrite_without_force() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("skills.json"), "{ \"target\": \"x/y\" }").unwrap();
    let assert = skills_cmd(tmp.path())
        .arg("init")
        .assert()
        .failure()
        .code(1);
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("--force"), "{stderr}");
    // Untouched: no manifest rewrite, no .gitignore side effects.
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("skills.json")).unwrap(),
        "{ \"target\": \"x/y\" }"
    );
    assert!(!tmp.path().join(".gitignore").exists());
}

#[test]
fn init_force_overwrites() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("skills.json"), "old").unwrap();
    skills_cmd(tmp.path())
        .args(["init", "--force"])
        .assert()
        .success();
    let raw = std::fs::read_to_string(tmp.path().join("skills.json")).unwrap();
    Manifest::parse(&raw).unwrap();
}

#[test]
fn update_without_manifest_is_config_error() {
    let tmp = tempfile::tempdir().unwrap();
    let assert = skills_cmd(tmp.path())
        .arg("update")
        .assert()
        .failure()
        .code(1);
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("skills init"), "{stderr}");
}

#[test]
fn unknown_subcommand_is_usage_error() {
    let tmp = tempfile::tempdir().unwrap();
    skills_cmd(tmp.path())
        .arg("bogus")
        .assert()
        .failure()
        .code(1);
}

#[test]
fn unknown_manifest_key_is_config_error() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("skills.json"), r#"{ "unknown-key": 1 }"#).unwrap();
    let assert = skills_cmd(tmp.path())
        .arg("update")
        .assert()
        .failure()
        .code(1);
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("skills.json"), "{stderr}");
}

// --- Update ----------------------------------------------------------------

#[test]
fn update_syncs_and_reports() {
    let project = fixture_project("basic");
    let out = stdout_of(skills_cmd(project.path()).arg("update").assert().success());
    insta::assert_snapshot!("bin_update_stdout", out);
    assert!(
        project
            .path()
            .join(".agents")
            .join("skills")
            .join("code-review")
            .join("SKILL.md")
            .is_file()
    );
    assert!(project.path().join("skills.lock").is_file());
}

#[test]
fn update_dry_run_advertises_and_writes_nothing() {
    let project = fixture_project("basic");
    let out = stdout_of(
        skills_cmd(project.path())
            .args(["update", "--dry-run"])
            .assert()
            .success(),
    );
    assert!(out.contains("would copy"), "{out}");
    assert!(!project.path().join(".agents").exists());
    assert!(!project.path().join("skills.lock").exists());
    insta::assert_snapshot!("bin_update_dry_run_stdout", out);
}

#[test]
fn update_conflict_exits_2_and_writes_nothing() {
    let project = fixture_project("conflict");
    let assert = skills_cmd(project.path())
        .arg("update")
        .assert()
        .failure()
        .code(2);
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("clashing"), "{stderr}");
    assert!(stderr.contains("dir/vendor-a"), "{stderr}");
    assert!(stderr.contains("dir/vendor-b"), "{stderr}");
    assert!(!project.path().join(".agents").exists());
    assert!(!project.path().join("skills.lock").exists());
}

#[test]
fn update_target_flag_beats_manifest() {
    let project = fixture_project("basic");
    skills_cmd(project.path())
        .args(["update", "--target", "custom/spot"])
        .assert()
        .success();
    assert!(
        project
            .path()
            .join("custom")
            .join("spot")
            .join("plain")
            .join("SKILL.md")
            .is_file()
    );
    assert!(!project.path().join(".agents").exists());
}

#[test]
fn missing_local_dir_is_provider_error_exit_4() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("skills.json"),
        r#"{ "sources": [ { "from": "dir", "path": "./missing" } ] }"#,
    )
    .unwrap();
    skills_cmd(tmp.path())
        .arg("update")
        .assert()
        .failure()
        .code(4);
}

// --- Aliases ----------------------------------------------------------------

/// Does `path` resolve on disk to the same canonical location as `target`?
/// Works across junctions (Windows) and symlinks (POSIX).
fn resolves_to(path: &std::path::Path, target: &std::path::Path) -> bool {
    match (std::fs::canonicalize(path), std::fs::canonicalize(target)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[test]
fn project_aliases_are_all_created_and_reachable() {
    let project = fixture_project("alias");
    let out = stdout_of(skills_cmd(project.path()).arg("update").assert().success());
    insta::assert_snapshot!("bin_update_alias_stdout", out);

    let target = project.path().join(".agents").join("skills");
    for alias_rel in [".claude", ".cursor"] {
        let alias = project.path().join(alias_rel).join("skills");
        assert!(
            resolves_to(&alias, &target),
            "{alias_rel} must link to target"
        );
        // Skill content is reachable through the alias.
        assert!(alias.join("code-review").join("SKILL.md").is_file());
    }
}

#[test]
fn alias_creation_is_idempotent() {
    let project = fixture_project("alias");
    skills_cmd(project.path()).arg("update").assert().success();
    let out = stdout_of(skills_cmd(project.path()).arg("update").assert().success());
    // Second run: aliases already correct, still exit 0.
    assert!(out.contains("already linked"), "{out}");
    let target = project.path().join(".agents").join("skills");
    assert!(resolves_to(
        &project.path().join(".claude").join("skills"),
        &target
    ));
}

#[test]
fn cli_alias_flag_replaces_project_config_entirely() {
    let project = fixture_project("alias");
    // Project config has .claude + .cursor; CLI passes only .zed → takeover.
    skills_cmd(project.path())
        .args(["update", "--alias", ".zed/skills"])
        .assert()
        .success();
    let target = project.path().join(".agents").join("skills");
    assert!(resolves_to(
        &project.path().join(".zed").join("skills"),
        &target
    ));
    assert!(
        !project.path().join(".claude").exists(),
        "project alias must not be created when --alias takes over"
    );
    assert!(!project.path().join(".cursor").exists());
}

#[test]
fn dry_run_announces_would_link_and_writes_nothing() {
    let project = fixture_project("alias");
    let out = stdout_of(
        skills_cmd(project.path())
            .args(["update", "--dry-run"])
            .assert()
            .success(),
    );
    assert!(out.contains("[would link]"), "{out}");
    assert!(out.contains(".claude/skills"), "{out}");
    assert!(!project.path().join(".claude").exists());
    assert!(!project.path().join(".agents").exists());
}

#[test]
fn pre_existing_real_directory_at_alias_fails_run_and_keeps_content() {
    let project = fixture_project("alias");
    // Occupy one alias path with a real dir holding user content.
    let claude = project.path().join(".claude").join("skills");
    std::fs::create_dir_all(&claude).unwrap();
    std::fs::write(claude.join("user.txt"), "precious").unwrap();

    let assert = skills_cmd(project.path())
        .args(["update", "--alias", ".claude/skills"])
        .assert()
        .failure()
        .code(1);
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("alias"), "{stderr}");

    // User content preserved; the target was still copied.
    assert_eq!(
        std::fs::read_to_string(claude.join("user.txt")).unwrap(),
        "precious"
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
fn alias_equal_to_target_is_config_error() {
    let project = fixture_project("basic");
    let assert = skills_cmd(project.path())
        .args(["update", "--alias", ".agents/skills"])
        .assert()
        .failure()
        .code(1);
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("must not equal the target"), "{stderr}");
    // Config error before any write.
    assert!(!project.path().join(".agents").exists());
}

#[test]
fn alias_escaping_project_root_is_config_error() {
    let project = fixture_project("basic");
    let assert = skills_cmd(project.path())
        .args(["update", "--alias", "../escape"])
        .assert()
        .failure()
        .code(1);
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("escapes the project root"), "{stderr}");
    assert!(!project.path().join("..").join("escape").exists());
}

// --- Show -------------------------------------------------------------------

#[test]
fn show_before_update_is_read_only_and_reports_not_synced() {
    let project = fixture_project("basic");
    let out = stdout_of(skills_cmd(project.path()).arg("show").assert().success());
    assert!(out.starts_with("Target: .agents/skills"), "{out}");
    assert!(out.contains("not-synced"), "{out}");
    // A `sources[]` donor is labeled as declared, not silently trusted.
    assert!(out.contains("[declared in skills.json]"), "{out}");
    // Read-only: no target dir, no lockfile.
    assert!(!project.path().join(".agents").exists());
    assert!(!project.path().join("skills.lock").exists());
    insta::assert_snapshot!("bin_show_before_update", out);
}

#[test]
fn show_after_update_reports_ok_and_descriptions() {
    let project = fixture_project("basic");
    skills_cmd(project.path()).arg("update").assert().success();
    let out = stdout_of(skills_cmd(project.path()).arg("show").assert().success());
    assert!(out.contains("[ok]"), "{out}");
    assert!(out.contains("Reviews code changes before commit"), "{out}");
    insta::assert_snapshot!("bin_show_after_update", out);
}

#[test]
fn show_detects_drift_but_ignores_user_files() {
    let project = fixture_project("basic");
    skills_cmd(project.path()).arg("update").assert().success();

    let target = project.path().join(".agents").join("skills");
    // User-added file: NOT drift.
    std::fs::write(target.join("docs-helper").join("user.md"), "mine").unwrap();
    // Edited lock-listed file: drift.
    std::fs::write(target.join("plain").join("SKILL.md"), "locally edited").unwrap();

    let out = stdout_of(skills_cmd(project.path()).arg("show").assert().success());
    let plain_line = out
        .lines()
        .find(|l| l.trim_start().starts_with("plain"))
        .unwrap();
    assert!(plain_line.contains("[mod]"), "{out}");
    let docs_line = out
        .lines()
        .find(|l| l.trim_start().starts_with("docs-helper"))
        .unwrap();
    assert!(docs_line.contains("[ok]"), "{out}");
}
