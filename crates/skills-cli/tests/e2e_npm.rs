//! End-to-end npm-provider scenarios (per-manager trust, discovery gate,
//! `--from` scoping, show annotations) through the actual `skills` binary.
//!
//! Fixtures are built at runtime in tempdirs: a real `node_modules/` tree is
//! not committed (git cannot reliably carry one, and it would balloon the
//! repo). The file name deliberately avoids "update" (Windows os error 740).

mod common;

use std::path::Path;

use assert_cmd::Command;

fn skills_cmd(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("skills").unwrap();
    cmd.current_dir(dir);
    cmd
}

fn stdout_of(assert: assert_cmd::assert::Assert) -> String {
    String::from_utf8(assert.get_output().stdout.clone()).unwrap()
}

/// Whether a skill landed in the default target.
fn has_skill(dir: &Path, id: &str) -> bool {
    dir.join(".agents")
        .join("skills")
        .join(id)
        .join("SKILL.md")
        .is_file()
}

/// Sorted skill dir names inside the default target.
fn target_entries(dir: &Path) -> Vec<String> {
    let target = dir.join(".agents").join("skills");
    if !target.is_dir() {
        return Vec::new();
    }
    let mut out: Vec<String> = std::fs::read_dir(&target)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = rel.split('/').fold(root.to_path_buf(), |d, s| d.join(s));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// `node_modules/<rel_dir>/package.json` naming the package `name`.
fn npm_pkg(root: &Path, rel_dir: &str, name: &str) {
    write(
        root,
        &format!("node_modules/{rel_dir}/package.json"),
        &format!("{{ \"name\": \"{name}\" }}"),
    );
}

/// A discoverable skill at `node_modules/<rel_dir>/skills/<id>/SKILL.md`.
fn npm_skill(root: &Path, rel_dir: &str, id: &str) {
    write(
        root,
        &format!("node_modules/{rel_dir}/skills/{id}/SKILL.md"),
        &format!("---\ndescription: {id}\n---\n"),
    );
}

// ── trust: donor is synced when trusted ───────────────────────────────────────

#[test]
fn direct_dependency_npm_donor_is_synced() {
    // @myorg/pack is a root direct dependency ⇒ trusted implicitly. npm is
    // opted in with the bare `dependencies.npm: true` toggle.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        root,
        "skills.json",
        r#"{ "dependencies": { "npm": true } }"#,
    );
    write(
        root,
        "package.json",
        r#"{ "dependencies": { "@myorg/pack": "^1" } }"#,
    );
    npm_pkg(root, "@myorg/pack", "@myorg/pack");
    npm_skill(root, "@myorg/pack", "npm-direct");

    skills_cmd(root).arg("update").assert().success();
    assert!(has_skill(root, "npm-direct"));
}

#[test]
fn transitive_npm_donor_synced_via_project_trusted_list() {
    // @myorg/pack is not a direct dep (no package.json), so it clears trust
    // only through `dependencies.npm.trusted`.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        root,
        "skills.json",
        r#"{ "dependencies": { "npm": { "enabled": true, "trusted": ["@myorg/*"] } } }"#,
    );
    npm_pkg(root, "@myorg/pack", "@myorg/pack");
    npm_skill(root, "@myorg/pack", "npm-trusted");

    skills_cmd(root).arg("update").assert().success();
    assert!(has_skill(root, "npm-trusted"));
}

#[test]
fn transitive_npm_donor_synced_via_builtin_list() {
    // @anthropic-ai/* is on the npm builtin list; the donor is transitive and
    // not project-listed, proving the builtin list is consumed.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        root,
        "skills.json",
        r#"{ "dependencies": { "npm": true } }"#,
    );
    npm_pkg(root, "@anthropic-ai/pack", "@anthropic-ai/pack");
    npm_skill(root, "@anthropic-ai/pack", "npm-builtin");

    skills_cmd(root).arg("update").assert().success();
    assert!(has_skill(root, "npm-builtin"));
}

// ── trust: untrusted transitive donor is skipped ──────────────────────────────

#[test]
fn untrusted_transitive_npm_donor_is_skipped_and_mentioned() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        root,
        "skills.json",
        r#"{ "dependencies": { "npm": true } }"#,
    );
    npm_pkg(root, "@random/pack", "@random/pack");
    npm_skill(root, "@random/pack", "npm-untrusted");

    let out = stdout_of(skills_cmd(root).arg("update").assert().success());
    assert!(!has_skill(root, "npm-untrusted"), "{out}");
    assert!(out.contains("[skip]"), "{out}");
    assert!(out.contains("@random/pack"), "{out}");
    assert!(out.contains("untrusted"), "{out}");
}

// ── discovery gate: npm is off by default ─────────────────────────────────────

#[test]
fn npm_is_disabled_by_default() {
    // No `dependencies.npm` entry ⇒ the npm tree is never walked, even for a
    // direct dependency that ships skills.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, "skills.json", "{}");
    write(
        root,
        "package.json",
        r#"{ "dependencies": { "@myorg/pack": "^1" } }"#,
    );
    npm_pkg(root, "@myorg/pack", "@myorg/pack");
    npm_skill(root, "@myorg/pack", "npm-offby");

    let out = stdout_of(skills_cmd(root).arg("update").assert().success());
    assert!(!has_skill(root, "npm-offby"), "{out}");
    assert_eq!(target_entries(root), Vec::<String>::new());
    assert!(out.contains("No skills found."), "{out}");
}

// ── show annotations ──────────────────────────────────────────────────────────

#[test]
fn show_lists_npm_donor_with_discovered_and_trust_chips() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        root,
        "skills.json",
        r#"{ "dependencies": { "npm": true } }"#,
    );
    write(
        root,
        "package.json",
        r#"{ "dependencies": { "@myorg/pack": "^1" } }"#,
    );
    // Direct dependency ⇒ [direct-dep]; npm donors are always discovery-routed
    // ⇒ [discovered].
    npm_pkg(root, "@myorg/pack", "@myorg/pack");
    npm_skill(root, "@myorg/pack", "npm-shown");
    // Builtin-trusted transitive donor ⇒ [builtin] + [discovered].
    npm_pkg(root, "@anthropic-ai/pack", "@anthropic-ai/pack");
    npm_skill(root, "@anthropic-ai/pack", "npm-anthropic");

    let out = stdout_of(skills_cmd(root).arg("show").assert().success());

    let direct = out
        .lines()
        .find(|l| l.contains("@myorg/pack"))
        .unwrap_or_default();
    assert!(direct.contains("[direct-dep]"), "{out}");
    assert!(direct.contains("[discovered]"), "{out}");

    let builtin = out
        .lines()
        .find(|l| l.contains("@anthropic-ai/pack"))
        .unwrap_or_default();
    assert!(builtin.contains("[builtin]"), "{out}");
    assert!(builtin.contains("[discovered]"), "{out}");

    // show is read-only.
    assert!(!root.join(".agents").exists());
    assert!(!root.join("skills.lock").exists());
}

// ── --from scoping ────────────────────────────────────────────────────────────

#[test]
fn from_npm_scopes_to_npm_donors_only() {
    // A project with BOTH a trusted composer donor and a trusted npm donor.
    // `--from=npm` must run npm alone, leaving the composer donor untouched.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        root,
        "skills.json",
        r#"{ "dependencies": { "npm": true } }"#,
    );

    // Composer direct-dep donor (trusted implicitly).
    write(
        root,
        "composer.json",
        r#"{ "name": "acme/project", "require": { "acme/comp-pack": "@dev" } }"#,
    );
    write(
        root,
        "vendor/composer/installed.json",
        r#"{ "packages": [ { "name": "acme/comp-pack", "version": "dev-main", "type": "library", "extra": { "skills": { "source": "skills" } }, "install-path": "../acme/comp-pack" } ], "dev": true, "dev-package-names": [] }"#,
    );
    write(
        root,
        "vendor/acme/comp-pack/composer.json",
        r#"{ "name": "acme/comp-pack", "extra": { "skills": { "source": "skills" } } }"#,
    );
    write(
        root,
        "vendor/acme/comp-pack/skills/comp-only/SKILL.md",
        "---\ndescription: comp-only\n---\n",
    );

    // npm direct-dep donor (trusted implicitly).
    write(
        root,
        "package.json",
        r#"{ "dependencies": { "@myorg/pack": "^1" } }"#,
    );
    npm_pkg(root, "@myorg/pack", "@myorg/pack");
    npm_skill(root, "@myorg/pack", "npm-only");

    // Scoped to npm from the start: the composer donor is never discovered, so
    // only the npm skill is synced. (A scoped run does not prune out-of-scope
    // entries, so it must not run after a full sync for this assertion.)
    let out = stdout_of(
        skills_cmd(root)
            .args(["update", "--from=npm"])
            .assert()
            .success(),
    );
    assert!(has_skill(root, "npm-only"), "{out}");
    assert!(!has_skill(root, "comp-only"), "{out}");
    assert!(!out.contains("acme/comp-pack"), "{out}");

    // Sanity: an unscoped run does sync the composer donor too, proving it is
    // a valid trusted donor that was excluded purely by `--from`.
    skills_cmd(root).arg("update").assert().success();
    assert!(has_skill(root, "comp-only"));
}

#[test]
fn unknown_from_value_lists_npm_in_the_expected_set() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, "skills.json", "{}");
    let assert = skills_cmd(root)
        .args(["update", "--from=bogus"])
        .assert()
        .failure()
        .code(1);
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("dir, composer, npm, github, gitlab, url"),
        "{stderr}"
    );
}
