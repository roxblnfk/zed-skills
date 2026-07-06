//! End-to-end tests for remote donors against a wiremock server: the full
//! pipeline through the library API plus the `skills add` binary flow.

mod common;

use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;

use assert_cmd::Command;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

use common::tree_listing;
use skills_core::error::PipelineError;
use skills_core::pipeline::ctx::{PrepareOptions, prepare};
use skills_core::pipeline::{SyncAction, SyncReport, run_update};
use skills_core::traits::VendorProvider;
use skills_providers::http::ReqwestClient;
use skills_providers::{DirProvider, GithubProvider, GitlabProvider, UrlProvider};

struct RawPathContains(&'static str);

impl Match for RawPathContains {
    fn matches(&self, request: &Request) -> bool {
        request.url.path().contains(self.0)
    }
}

/// Full provider set backed by the real HTTP client (pointed at wiremock
/// via the manifest `host` field).
fn providers() -> Vec<Arc<dyn VendorProvider>> {
    let http: Arc<dyn skills_providers::HttpClient> = Arc::new(ReqwestClient::new().unwrap());
    vec![
        Arc::new(DirProvider),
        Arc::new(GithubProvider::new(Arc::clone(&http), None)),
        Arc::new(GitlabProvider::new(Arc::clone(&http), None)),
        Arc::new(UrlProvider::new(http)),
    ]
}

fn locators() -> Vec<Arc<dyn skills_core::traits::SkillLocator>> {
    vec![
        Arc::new(skills_providers::ComposerDeclaredLocator),
        Arc::new(skills_providers::WellKnownLocator),
        Arc::new(skills_providers::DeclaredLocator),
    ]
}

async fn run(project: &Path, refresh: bool) -> Result<SyncReport, PipelineError> {
    let ctx = prepare(
        project,
        PrepareOptions {
            refresh,
            ..Default::default()
        },
    )?;
    run_update(&ctx, &providers(), &locators(), &skills_audit::noop_chain()).await
}

fn zip_fixture() -> Vec<u8> {
    skills_providers::testkit::build_zip(&[
        (
            "acme-skills-1234/.claude/skills/code-review/SKILL.md",
            Some("---\nname: code-review\ndescription: Reviews code changes\n---\nBody\n"),
        ),
        (
            "acme-skills-1234/.claude/skills/code-review/scripts/run.ps1",
            Some("Write-Host review\n"),
        ),
        (
            "acme-skills-1234/skills/catalog/deep-skill/SKILL.md",
            Some("---\nname: deep-skill\n---\nCatalog layout\n"),
        ),
        ("acme-skills-1234/README.md", Some("not a skill")),
    ])
}

async fn mount_github_repo(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/v3/repos/acme/skills/zipball/v1.0.0"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(zip_fixture(), "application/zip"))
        .expect(1) // the cache must absorb every later materialization
        .mount(server)
        .await;
}

fn write_manifest(project: &Path, host: &str) {
    std::fs::write(
        project.join("skills.json"),
        format!(
            r#"{{ "remote": [ {{ "from": "github", "package": "acme/skills", "ref": "v1.0.0", "host": "{host}" }} ] }}"#
        ),
    )
    .unwrap();
}

/// Redact the wiremock host (random port) and content hashes.
fn redact(text: &str, host: &str) -> String {
    common::redact_lock(&text.replace(host, "[host]"))
}

#[tokio::test]
async fn remote_entry_syncs_tree_and_lockfile() {
    let server = MockServer::start().await;
    mount_github_repo(&server).await;
    let project = tempfile::tempdir().unwrap();
    write_manifest(project.path(), &server.uri());

    let report = run(project.path(), false).await.unwrap();
    assert_eq!(report.count(SyncAction::Add), 2);

    let target = project.path().join(".agents").join("skills");
    insta::assert_snapshot!("remote_target_tree", tree_listing(&target).join("\n"));

    let lock_raw = std::fs::read_to_string(project.path().join("skills.lock")).unwrap();
    // Machine-independent: no temp paths inside.
    let temp_path = project.path().to_string_lossy().replace('\\', "/");
    assert!(!lock_raw.replace('\\', "/").contains(&temp_path));
    insta::assert_snapshot!("remote_lockfile", redact(&lock_raw, &server.uri()));
}

#[tokio::test]
async fn second_sync_hits_the_cache_and_skips() {
    let server = MockServer::start().await;
    mount_github_repo(&server).await; // expect(1): one download total
    let project = tempfile::tempdir().unwrap();
    write_manifest(project.path(), &server.uri());

    run(project.path(), false).await.unwrap();
    let report = run(project.path(), false).await.unwrap();
    assert_eq!(report.count(SyncAction::Skip), 2);
    assert_eq!(report.count(SyncAction::Add), 0);
}

#[tokio::test]
async fn refresh_flag_redownloads_the_archive() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/repos/acme/skills/zipball/v1.0.0"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(zip_fixture(), "application/zip"))
        .expect(2) // initial + refresh
        .mount(&server)
        .await;
    let project = tempfile::tempdir().unwrap();
    write_manifest(project.path(), &server.uri());

    run(project.path(), false).await.unwrap();
    let report = run(project.path(), true).await.unwrap();
    assert_eq!(report.count(SyncAction::Skip), 2, "content did not change");
}

#[tokio::test]
async fn remote_allowlist_filters_by_canonical_name() {
    let server = MockServer::start().await;
    mount_github_repo(&server).await;
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("skills.json"),
        format!(
            r#"{{ "remote": [ {{ "from": "github", "package": "acme/skills", "ref": "v1.0.0",
                 "host": "{}", "skills": ["code-review"] }} ] }}"#,
            server.uri()
        ),
    )
    .unwrap();

    let report = run(project.path(), false).await.unwrap();
    assert_eq!(report.count(SyncAction::Add), 1);
    let target = project.path().join(".agents").join("skills");
    assert!(target.join("code-review").is_dir());
    assert!(!target.join("deep-skill").exists());
}

// --- `skills add` through the binary ---------------------------------------

fn skills_cmd(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("skills").unwrap();
    cmd.current_dir(dir);
    cmd
}

#[tokio::test]
async fn add_gitlab_subgroup_registers_and_syncs() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(RawPathContains(
            "/api/v4/projects/org%2Fgroup%2Fproject/repository/tags",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"[{"name":"v1.2.3"},{"name":"v1.0.0"}]"#,
            "application/json",
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(RawPathContains(
            "/api/v4/projects/org%2Fgroup%2Fproject/repository/archive.zip",
        ))
        .and(query_param("sha", "v1.2.3"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(zip_fixture(), "application/zip"))
        .expect(1) // add validates once; the follow-up sync hits the cache
        .mount(&server)
        .await;

    let project = tempfile::tempdir().unwrap();
    let assert = skills_cmd(project.path())
        .args(["add", "gitlab:org/group/project", "--host", &server.uri()])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    // No --ref: the cascade picked v1.2.3 and stored its caret form.
    assert!(
        stdout.contains("Registered gitlab:org/group/project @ ^1.2.3"),
        "{stdout}"
    );

    // Manifest entry written (2-space indent, caret ref, host preserved).
    let manifest = std::fs::read_to_string(project.path().join("skills.json")).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    let entry = &doc["remote"][0];
    assert_eq!(entry["from"], "gitlab");
    assert_eq!(entry["package"], "org/group/project");
    assert_eq!(entry["ref"], "^1.2.3");
    assert_eq!(entry["host"], server.uri());
    assert!(entry.get("skills").is_none());

    // Sync ran: skills on disk + lockfile.
    let target = project.path().join(".agents").join("skills");
    assert!(target.join("code-review").join("SKILL.md").is_file());
    assert!(target.join("deep-skill").join("SKILL.md").is_file());
    assert!(project.path().join("skills.lock").is_file());
}

#[tokio::test]
async fn add_with_explicit_ref_and_skill_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/repos/acme/skills/zipball/main"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(zip_fixture(), "application/zip"))
        .mount(&server)
        .await;

    let project = tempfile::tempdir().unwrap();
    // Existing manifest must be preserved, not clobbered.
    std::fs::write(
        project.path().join("skills.json"),
        "{ \"target\": \".agents/skills\" }\n",
    )
    .unwrap();

    skills_cmd(project.path())
        .args([
            "add",
            "github:acme/skills",
            "--ref",
            "main",
            "--skill",
            "code-review",
            "--host",
            &server.uri(),
        ])
        .assert()
        .success();

    let manifest = std::fs::read_to_string(project.path().join("skills.json")).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    assert_eq!(doc["target"], ".agents/skills");
    let entry = &doc["remote"][0];
    // Explicit user ref stored verbatim (no caret derivation).
    assert_eq!(entry["ref"], "main");
    assert_eq!(entry["skills"], serde_json::json!(["code-review"]));

    let target = project.path().join(".agents").join("skills");
    assert!(target.join("code-review").is_dir());
    assert!(!target.join("deep-skill").exists(), "allowlist filtered");
}

#[tokio::test]
async fn add_refuses_repos_without_skills() {
    let server = MockServer::start().await;
    let empty_zip =
        skills_providers::testkit::build_zip(&[("repo-main/README.md", Some("no skills here"))]);
    Mock::given(method("GET"))
        .and(path("/api/v3/repos/acme/empty/zipball/main"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(empty_zip, "application/zip"))
        .mount(&server)
        .await;

    let project = tempfile::tempdir().unwrap();
    let assert = skills_cmd(project.path())
        .args([
            "add",
            "github:acme/empty",
            "--ref",
            "main",
            "--host",
            &server.uri(),
        ])
        .assert()
        .failure()
        .code(4);
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("acme/empty"), "{stderr}");
    // Nothing registered, nothing synced.
    assert!(!project.path().join("skills.json").exists());
    assert!(!project.path().join(".agents").exists());
}

#[tokio::test]
async fn add_bad_input_is_a_usage_error() {
    let project = tempfile::tempdir().unwrap();
    let assert = skills_cmd(project.path())
        .args(["add", "not a repo"])
        .assert()
        .failure()
        .code(1);
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("github:owner/repo"), "{stderr}");
}

// --- `--from` filter ---------------------------------------------------------

#[tokio::test]
async fn from_filter_hides_other_providers() {
    let server = MockServer::start().await;
    mount_github_repo(&server).await;
    let project = tempfile::tempdir().unwrap();
    // A dir donor AND a github donor.
    let donor = project.path().join("local-src");
    std::fs::create_dir_all(donor.join("local-skill")).unwrap();
    let mut f = std::fs::File::create(donor.join("local-skill").join("SKILL.md")).unwrap();
    writeln!(f, "---\nname: local-skill\n---").unwrap();
    std::fs::write(
        project.path().join("skills.json"),
        format!(
            r#"{{ "local": {{ "dir": ["./local-src"] }},
                 "remote": [ {{ "from": "github", "package": "acme/skills", "ref": "v1.0.0", "host": "{}" }} ] }}"#,
            server.uri()
        ),
    )
    .unwrap();

    // --from=github hides the dir donor.
    let out = skills_cmd(project.path())
        .args(["update", "--from", "github", "--dry-run"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("code-review"), "{stdout}");
    assert!(!stdout.contains("local-skill"), "{stdout}");

    // --from=dir hides the remote donor (and needs no network at all).
    let out = skills_cmd(project.path())
        .args(["update", "--from", "dir", "--dry-run"])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("local-skill"), "{stdout}");
    assert!(!stdout.contains("code-review"), "{stdout}");

    // Unknown provider id is a usage error.
    skills_cmd(project.path())
        .args(["update", "--from", "svn"])
        .assert()
        .failure()
        .code(1);
}
