//! End-to-end composer-provider scenarios (trust, filters, discovery,
//! robustness, show annotations) against the sandbox fixture — ports of the
//! PHP acceptance suite (SPEC §11).

mod common;

use std::path::Path;

use assert_cmd::Command;
use common::{fixture_project, tree_fingerprint};

fn skills_cmd(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("skills").unwrap();
    cmd.current_dir(dir);
    cmd
}

fn set_manifest(dir: &Path, json: &str) {
    std::fs::write(dir.join("skills.json"), json).unwrap();
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

fn has_skill(dir: &Path, id: &str) -> bool {
    dir.join(".agents")
        .join("skills")
        .join(id)
        .join("SKILL.md")
        .is_file()
}

fn stdout_of(assert: assert_cmd::assert::Assert) -> String {
    String::from_utf8(assert.get_output().stdout.clone()).unwrap()
}

fn stderr_of(assert: assert_cmd::assert::Assert) -> String {
    String::from_utf8(assert.get_output().stderr.clone()).unwrap()
}

// ── trust policy ────────────────────────────────────────────────────────────

#[test]
fn default_sync_covers_direct_dep_and_builtin_trust() {
    // acme/* via direct-dep trust, spiral/skills-demo via the builtin list;
    // transitive evil/clash skipped; undeclared packages left as candidates.
    let project = fixture_project("sandbox");
    let out = stdout_of(skills_cmd(project.path()).arg("update").assert().success());
    assert_eq!(
        target_entries(project.path()),
        ["code-review", "demo", "greeting", "migrate", "refactor"]
    );
    insta::assert_snapshot!("composer_default_update_stdout", out);
}

#[test]
fn untrusted_vendor_is_skipped_by_default_and_mentioned_in_skip_block() {
    let project = fixture_project("sandbox");
    let out = stdout_of(skills_cmd(project.path()).arg("update").assert().success());
    assert!(!has_skill(project.path(), "tutorial"));
    assert!(out.contains("[skip]"), "{out}");
    assert!(out.contains("evil/payload"), "{out}");
}

#[test]
fn trust_flag_allows_untrusted_vendor() {
    let project = fixture_project("sandbox");
    skills_cmd(project.path())
        .args(["update", "--trust=evil/payload"])
        .assert()
        .success();
    assert!(has_skill(project.path(), "tutorial"));
}

#[test]
fn trust_flag_vendor_wildcard_allows_all_packages_under_it() {
    let project = fixture_project("sandbox");
    skills_cmd(project.path())
        .args(["update", "--trust=evil/*"])
        .assert()
        .success();
    assert!(has_skill(project.path(), "tutorial"));
}

#[test]
fn invalid_trust_pattern_is_a_usage_error() {
    let project = fixture_project("sandbox");
    let assert = skills_cmd(project.path())
        .args(["update", "--trust=evil"])
        .assert()
        .failure()
        .code(1);
    assert!(stderr_of(assert).contains("invalid vendor pattern"));
}

#[test]
fn naming_untrusted_vendor_positionally_trusts_it_silently() {
    let project = fixture_project("sandbox");
    let assert = skills_cmd(project.path())
        .args(["update", "evil/payload"])
        .assert()
        .success();
    assert!(has_skill(project.path(), "tutorial"));
    let combined = stdout_of(assert);
    assert!(!combined.contains("untrusted"), "{combined}");
}

#[test]
fn project_trusted_wildcard_unlocks_transitive_vendor() {
    let project = fixture_project("sandbox");
    set_manifest(project.path(), r#"{ "trusted": ["evil/*"] }"#);
    let out = stdout_of(skills_cmd(project.path()).arg("update").assert().success());
    assert_eq!(
        target_entries(project.path()),
        [
            "code-review",
            "demo",
            "greeting",
            "migrate",
            "refactor",
            "tutorial"
        ]
    );
    // clash/skills-conflict stays skipped: same transitive position, no
    // pattern coverage.
    assert!(out.contains("clash/skills-conflict"), "{out}");
}

#[test]
fn direct_dependency_is_implicitly_trusted_without_any_pattern() {
    let project = fixture_project("sandbox");
    set_manifest(project.path(), r#"{ "trusted": [] }"#);
    skills_cmd(project.path()).arg("update").assert().success();
    assert!(has_skill(project.path(), "code-review"));
    assert!(has_skill(project.path(), "refactor"));
}

#[test]
fn transitive_dependency_stays_skipped_without_pattern() {
    let project = fixture_project("sandbox");
    let out = stdout_of(skills_cmd(project.path()).arg("update").assert().success());
    assert!(!has_skill(project.path(), "tutorial"));
    assert!(out.contains("evil/payload"), "{out}");
}

#[test]
fn builtin_list_is_active_when_replace_is_false() {
    let project = fixture_project("sandbox");
    set_manifest(project.path(), r#"{ "trusted": [] }"#);
    skills_cmd(project.path()).arg("update").assert().success();
    // spiral/* is on the builtin list; the donor is transitive.
    assert!(has_skill(project.path(), "demo"));
}

#[test]
fn trusted_replace_disables_builtin_and_direct_dep_trust() {
    let project = fixture_project("sandbox");
    set_manifest(
        project.path(),
        r#"{ "trusted": [], "trusted-replace": true }"#,
    );
    skills_cmd(project.path()).arg("update").assert().success();
    assert_eq!(target_entries(project.path()), Vec::<String>::new());
}

#[test]
fn trusted_replace_limits_trust_to_project_list_exactly() {
    let project = fixture_project("sandbox");
    set_manifest(
        project.path(),
        r#"{ "trusted": ["acme/skills-basic"], "trusted-replace": true }"#,
    );
    skills_cmd(project.path()).arg("update").assert().success();
    assert_eq!(target_entries(project.path()), ["code-review", "greeting"]);
}

// ── positional package filters ──────────────────────────────────────────────

#[test]
fn positional_arg_restricts_sync_to_named_package() {
    let project = fixture_project("sandbox");
    skills_cmd(project.path())
        .args(["update", "acme/skills-basic"])
        .assert()
        .success();
    assert_eq!(target_entries(project.path()), ["code-review", "greeting"]);
}

#[test]
fn multiple_positional_args_include_all_named_packages() {
    let project = fixture_project("sandbox");
    skills_cmd(project.path())
        .args(["update", "acme/skills-basic", "acme/skills-pro"])
        .assert()
        .success();
    assert_eq!(
        target_entries(project.path()),
        ["code-review", "greeting", "migrate", "refactor"]
    );
}

#[test]
fn wildcard_positional_matches_vendor_and_auto_discovers_undeclared() {
    let project = fixture_project("sandbox");
    skills_cmd(project.path())
        .args(["update", "acme/*"])
        .assert()
        .success();
    // basic + pro (declared) + undeclared (auto-discovered because the user
    // named it via the wildcard); broken is malformed (warned, skipped);
    // clash/evil/spiral/nested/oddball are other vendors.
    assert_eq!(
        target_entries(project.path()),
        [
            "auto-skill",
            "code-review",
            "greeting",
            "migrate",
            "refactor"
        ]
    );
}

#[test]
fn positional_matching_no_installed_package_is_usage_error_and_writes_nothing() {
    let project = fixture_project("sandbox");
    let assert = skills_cmd(project.path())
        .args(["update", "ghost/package"])
        .assert()
        .failure()
        .code(1);
    assert!(stderr_of(assert).contains("ghost/package"));
    assert!(!project.path().join(".agents").exists());
    assert!(!project.path().join("skills.lock").exists());
}

#[test]
fn partial_run_never_prunes_out_of_scope_lock_entries() {
    let project = fixture_project("sandbox");
    skills_cmd(project.path()).arg("update").assert().success();
    assert_eq!(target_entries(project.path()).len(), 5);

    // A scoped re-run must not remove the other donors' skills.
    skills_cmd(project.path())
        .args(["update", "acme/skills-basic"])
        .assert()
        .success();
    assert_eq!(target_entries(project.path()).len(), 5);
    let lock = std::fs::read_to_string(project.path().join("skills.lock")).unwrap();
    assert!(lock.contains("\"demo\""), "{lock}");

    // And a following full run is still a no-op.
    let out = stdout_of(skills_cmd(project.path()).arg("update").assert().success());
    assert!(out.contains("0 added, 0 updated, 0 removed"), "{out}");
}

// ── conflicts ───────────────────────────────────────────────────────────────

#[test]
fn conflict_between_trusted_donors_aborts_and_writes_nothing() {
    let project = fixture_project("sandbox");
    set_manifest(project.path(), r#"{ "trusted": ["clash/*"] }"#);
    let assert = skills_cmd(project.path())
        .arg("update")
        .assert()
        .failure()
        .code(2);
    let stderr = stderr_of(assert);
    assert!(stderr.contains("greeting"), "{stderr}");
    assert!(stderr.contains("acme/skills-basic"), "{stderr}");
    assert!(stderr.contains("clash/skills-conflict"), "{stderr}");
    assert!(!project.path().join(".agents").exists());
    assert!(!project.path().join("skills.lock").exists());
}

#[test]
fn dry_run_reports_conflicts_identically() {
    let project = fixture_project("sandbox");
    set_manifest(project.path(), r#"{ "trusted": ["clash/*"] }"#);
    skills_cmd(project.path())
        .args(["update", "--dry-run"])
        .assert()
        .failure()
        .code(2);
    assert!(!project.path().join(".agents").exists());
}

// ── robustness ──────────────────────────────────────────────────────────────

#[test]
fn malformed_vendor_extra_warns_but_never_blocks_other_donors() {
    let project = fixture_project("sandbox");
    let out = stdout_of(skills_cmd(project.path()).arg("update").assert().success());
    assert!(has_skill(project.path(), "greeting"));
    assert!(has_skill(project.path(), "refactor"));
    assert!(out.contains("[warn]"), "{out}");
    assert!(out.contains("acme/skills-broken"), "{out}");
    assert!(
        out.contains("extra.skills.source must not escape the package root"),
        "{out}"
    );
}

#[test]
fn vendor_with_rootlike_extra_but_no_source_is_skipped_silently() {
    let project = fixture_project("sandbox");
    let update = skills_cmd(project.path()).arg("update").assert().success();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&update.get_output().stdout),
        String::from_utf8_lossy(&update.get_output().stderr)
    );
    assert!(!combined.contains("skills-rootlike"), "{combined}");

    let show = skills_cmd(project.path()).arg("show").assert().success();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&show.get_output().stdout),
        String::from_utf8_lossy(&show.get_output().stderr)
    );
    assert!(!combined.contains("skills-rootlike"), "{combined}");
}

#[test]
fn composer_sync_is_idempotent() {
    let project = fixture_project("sandbox");
    skills_cmd(project.path()).arg("update").assert().success();
    let first = tree_fingerprint(&project.path().join(".agents").join("skills"));
    let lock_first = std::fs::read_to_string(project.path().join("skills.lock")).unwrap();

    let out = stdout_of(skills_cmd(project.path()).arg("update").assert().success());
    let second = tree_fingerprint(&project.path().join(".agents").join("skills"));
    let lock_second = std::fs::read_to_string(project.path().join("skills.lock")).unwrap();
    assert_eq!(first, second);
    assert_eq!(lock_first, lock_second);
    assert!(out.contains("0 added, 0 updated, 0 removed"), "{out}");
}

#[test]
fn local_composer_toggle_disables_the_provider() {
    let project = fixture_project("sandbox");
    set_manifest(project.path(), r#"{ "local": { "composer": false } }"#);
    let out = stdout_of(skills_cmd(project.path()).arg("update").assert().success());
    assert!(out.contains("No skills found."), "{out}");
    assert!(!out.contains("acme/"), "{out}");
}

// ── discovery ───────────────────────────────────────────────────────────────

#[test]
fn discovery_is_off_by_default_and_hints_about_candidates() {
    let project = fixture_project("sandbox");
    let out = stdout_of(skills_cmd(project.path()).arg("update").assert().success());
    assert!(!has_skill(project.path(), "auto-skill"));
    assert!(!has_skill(project.path(), "hidden-claude"));
    assert!(out.contains("[hint]"), "{out}");
    assert!(out.contains("--discovery"), "{out}");
}

#[test]
fn discovery_flag_includes_undeclared_skills_from_trusted_vendor_without_hint() {
    let project = fixture_project("sandbox");
    let out = stdout_of(
        skills_cmd(project.path())
            .args(["update", "--discovery", "--trust=acme/skills-undeclared"])
            .assert()
            .success(),
    );
    assert!(has_skill(project.path(), "auto-skill"));
    assert!(!out.contains("[hint]"), "{out}");
}

#[test]
fn discovery_finds_dot_claude_and_catalog_layout_of_undeclared_package() {
    let project = fixture_project("sandbox");
    skills_cmd(project.path())
        .args(["update", "--discovery", "--trust=nested/*"])
        .assert()
        .success();
    assert!(has_skill(project.path(), "hidden-claude"));
    assert!(has_skill(project.path(), "hidden-catalog"));
}

#[test]
fn recursive_fallback_skills_gated_behind_discovery() {
    let project = fixture_project("sandbox");
    skills_cmd(project.path()).arg("update").assert().success();
    assert!(!has_skill(project.path(), "weird-place"));

    skills_cmd(project.path())
        .args(["update", "--discovery", "--trust=oddball/*"])
        .assert()
        .success();
    assert!(has_skill(project.path(), "weird-place"));
}

#[test]
fn naming_undeclared_package_auto_enables_discovery_for_it_only() {
    let project = fixture_project("sandbox");
    let out = stdout_of(
        skills_cmd(project.path())
            .args(["update", "acme/skills-undeclared"])
            .assert()
            .success(),
    );
    assert!(has_skill(project.path(), "auto-skill"));
    // Other undeclared packages stay out and still drive the hint.
    assert!(!has_skill(project.path(), "hidden-claude"));
    assert!(out.contains("[hint]"), "{out}");
}

#[test]
fn manifest_discovery_flag_enables_the_chain() {
    let project = fixture_project("sandbox");
    set_manifest(
        project.path(),
        r#"{ "discovery": true, "trusted": ["acme/*", "nested/*", "oddball/*"] }"#,
    );
    skills_cmd(project.path()).arg("update").assert().success();
    assert_eq!(
        target_entries(project.path()),
        [
            "auto-skill",
            "code-review",
            "demo",
            "greeting",
            "hidden-catalog",
            "hidden-claude",
            "migrate",
            "refactor",
            "weird-place",
        ]
    );
}

// ── show ────────────────────────────────────────────────────────────────────

#[test]
fn show_is_read_only_and_annotates_builtin_trust() {
    let project = fixture_project("sandbox");
    let out = stdout_of(skills_cmd(project.path()).arg("show").assert().success());
    let spiral = out
        .lines()
        .find(|l| l.contains("spiral/skills-demo"))
        .unwrap_or_default();
    assert!(spiral.contains("[builtin]"), "{out}");
    assert!(!project.path().join(".agents").exists());
    assert!(!project.path().join("skills.lock").exists());
    insta::assert_snapshot!("composer_default_show_stdout", out);
}

#[test]
fn show_annotates_direct_dependency_trust() {
    let project = fixture_project("sandbox");
    set_manifest(project.path(), r#"{ "trusted": [] }"#);
    let out = stdout_of(skills_cmd(project.path()).arg("show").assert().success());
    let basic = out
        .lines()
        .find(|l| l.contains("acme/skills-basic"))
        .unwrap_or_default();
    assert!(basic.contains("[direct-dep]"), "{out}");
}

#[test]
fn show_does_not_annotate_project_trusted_donors() {
    let project = fixture_project("sandbox");
    set_manifest(
        project.path(),
        r#"{ "trusted": ["acme/skills-basic", "acme/skills-pro"] }"#,
    );
    let out = stdout_of(skills_cmd(project.path()).arg("show").assert().success());
    for line in out.lines() {
        if line.contains("acme/skills-basic") || line.contains("acme/skills-pro") {
            assert!(!line.contains("[builtin]"), "{line}");
            assert!(!line.contains("[direct-dep]"), "{line}");
        }
    }
}

#[test]
fn show_lists_untrusted_donors_in_skipped_section() {
    let project = fixture_project("sandbox");
    let out = stdout_of(skills_cmd(project.path()).arg("show").assert().success());
    assert!(out.contains("Skipped:"), "{out}");
    let evil = out
        .lines()
        .find(|l| l.contains("evil/payload"))
        .unwrap_or_default();
    assert!(evil.contains("untrusted"), "{out}");
}

#[test]
fn show_lists_malformed_donor_with_reason_detail() {
    let project = fixture_project("sandbox");
    let out = stdout_of(skills_cmd(project.path()).arg("show").assert().success());
    let broken = out
        .lines()
        .find(|l| l.contains("acme/skills-broken"))
        .unwrap_or_default();
    assert!(broken.contains("malformed"), "{out}");
    assert!(
        broken.contains("extra.skills.source must not escape the package root"),
        "{out}"
    );
}

#[test]
fn show_lists_undeclared_donor_as_not_declared_and_hints() {
    let project = fixture_project("sandbox");
    let out = stdout_of(skills_cmd(project.path()).arg("show").assert().success());
    assert!(!out.contains("auto-skill"), "{out}");
    let undeclared = out
        .lines()
        .find(|l| l.contains("acme/skills-undeclared"))
        .unwrap_or_default();
    assert!(undeclared.contains("not-declared"), "{out}");
    assert!(out.contains("--discovery"), "{out}");
}

#[test]
fn show_discovery_flag_promotes_undeclared_donor_with_discovered_mark() {
    let project = fixture_project("sandbox");
    let out = stdout_of(
        skills_cmd(project.path())
            .args(["show", "--discovery", "--trust=acme/skills-undeclared"])
            .assert()
            .success(),
    );
    let undeclared = out
        .lines()
        .find(|l| l.contains("acme/skills-undeclared"))
        .unwrap_or_default();
    assert!(undeclared.contains("[discovered]"), "{out}");
    assert!(out.contains("auto-skill"), "{out}");
    assert!(!out.contains("[hint]"), "{out}");
}

#[test]
fn show_lists_filtered_out_donors_under_positional_filter() {
    let project = fixture_project("sandbox");
    let out = stdout_of(
        skills_cmd(project.path())
            .args(["show", "acme/skills-basic"])
            .assert()
            .success(),
    );
    assert!(out.contains("acme/skills-basic"), "{out}");
    let pro = out
        .lines()
        .find(|l| l.contains("acme/skills-pro"))
        .unwrap_or_default();
    assert!(pro.contains("filtered-out"), "{out}");
}

#[test]
fn show_reports_ok_after_update() {
    let project = fixture_project("sandbox");
    skills_cmd(project.path()).arg("update").assert().success();
    let out = stdout_of(skills_cmd(project.path()).arg("show").assert().success());
    let demo = out
        .lines()
        .find(|l| l.trim_start().starts_with("demo"))
        .unwrap_or_default();
    assert!(demo.contains("[ok]"), "{out}");
}
