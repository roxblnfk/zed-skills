//! End-to-end dir-name safety tests through the actual `skills` binary:
//! case-collision conflicts and FS-dangerous skill directory names abort
//! with exit code 2 before any write.
//!
//! The offending projects are built programmatically in tempdirs — git on
//! Windows cannot even check out a fixture containing `Foo` next to `foo`
//! (case-insensitive) or a control character in a path, so committed
//! fixtures are not an option here.

mod common;

use std::path::Path;

use assert_cmd::Command;
use common::tree_fingerprint;

fn skills_cmd(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("skills").unwrap();
    cmd.current_dir(dir);
    cmd
}

fn stderr_of(assert: &assert_cmd::assert::Assert) -> String {
    String::from_utf8(assert.get_output().stderr.clone()).unwrap()
}

/// Project with one skill dir per donor, built at test runtime.
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

#[test]
fn case_variant_conflict_exits_2_listing_both_spellings() {
    let project = runtime_project(&[("donor-a", &["Foo"]), ("donor-b", &["foo"])]);
    let before = tree_fingerprint(project.path());

    let assert = skills_cmd(project.path())
        .arg("update")
        .assert()
        .failure()
        .code(2);
    insta::assert_snapshot!("case_conflict_stderr", stderr_of(&assert));

    assert_eq!(
        before,
        tree_fingerprint(project.path()),
        "conflict must leave the filesystem untouched"
    );
}

#[test]
fn dangerous_dir_name_exits_2_before_any_write() {
    // DEL (U+007F) is the only tier-1-dangerous character that can exist in
    // a donor directory on every OS including Windows; the full dangerous
    // matrix (reserved device names, trailing dot/space, illegal chars) is
    // covered FS-free by the resolve-stage unit tests.
    let project = runtime_project(&[("donor", &["bad\u{7f}name"])]);
    let before = tree_fingerprint(project.path());

    let assert = skills_cmd(project.path())
        .arg("update")
        .assert()
        .failure()
        .code(2);
    let stderr = stderr_of(&assert);
    assert!(
        stderr.contains("dangerous skill directory name"),
        "{stderr}"
    );
    assert!(stderr.contains("from dir/donor"), "{stderr}");
    assert!(stderr.contains("control character (U+007F)"), "{stderr}");

    assert_eq!(
        before,
        tree_fingerprint(project.path()),
        "dangerous name must leave the filesystem untouched"
    );
    assert!(!project.path().join(".agents").exists());
    assert!(!project.path().join("skills.lock").exists());
}

#[test]
fn dry_run_reports_dangerous_names_identically() {
    let project = runtime_project(&[("donor", &["nice", "bad\u{7f}name"])]);
    skills_cmd(project.path())
        .args(["update", "--dry-run"])
        .assert()
        .failure()
        .code(2);
    assert!(!project.path().join(".agents").exists());
}
