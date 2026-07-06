//! Wiremock-backed integration tests for the remote providers: real
//! `ReqwestClient` against a local mock server, fixture zips built in-test.

use std::sync::Arc;

use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

use skills_core::traits::{Cache, SkillLocator, Vendor};
use skills_providers::http::ReqwestClient;
use skills_providers::testkit::{ContractExpectations, build_zip, run_vendor_contract};
use skills_providers::{
    ComposerDeclaredLocator, GithubVendor, GitlabVendor, UrlVendor, WellKnownLocator,
};

fn http() -> Arc<ReqwestClient> {
    Arc::new(ReqwestClient::new().expect("reqwest client"))
}

fn locators() -> Vec<Arc<dyn SkillLocator>> {
    vec![
        Arc::new(ComposerDeclaredLocator),
        Arc::new(WellKnownLocator),
    ]
}

fn cache(tmp: &tempfile::TempDir) -> Cache {
    Cache::new(tmp.path().join(".skills-cache"))
}

/// Repo fixture: `.claude/skills` container with two skills.
fn repo_zip(top: &str) -> Vec<u8> {
    build_zip(&[
        (
            &format!("{top}/.claude/skills/code-review/SKILL.md"),
            Some("---\nname: code-review\ndescription: Reviews code\n---\nBody\n"),
        ),
        (
            &format!("{top}/.claude/skills/code-review/references/checklist.md"),
            Some("checklist"),
        ),
        (
            &format!("{top}/.claude/skills/greeting/SKILL.md"),
            Some("---\nname: greeting\n---\nHi\n"),
        ),
        (&format!("{top}/README.md"), Some("not a skill")),
    ])
}

/// Matches when the *raw* request path contains the given substring —
/// used to pin GitLab's %2F-encoded project id without any decoding
/// leniency from the stock `path` matcher.
struct RawPathContains(&'static str);

impl Match for RawPathContains {
    fn matches(&self, request: &Request) -> bool {
        request.url.path().contains(self.0)
    }
}

// --- GitHub -----------------------------------------------------------------

#[tokio::test]
async fn github_tags_zipball_flow_with_redirect_and_token() {
    let server = MockServer::start().await;

    // Tag listing: requires the GitHub headers + Bearer token.
    Mock::given(method("GET"))
        .and(path("/api/v3/repos/acme/skills/tags"))
        .and(query_param("per_page", "100"))
        .and(header("Accept", "application/vnd.github+json"))
        .and(header("User-Agent", "ai-skills"))
        .and(header("Authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"[{"name":"v1.1.0"},{"name":"v1.2.0"},{"name":"v2.0.0-rc.1"},{"name":"junk"}]"#,
            "application/json",
        ))
        .expect(1)
        .mount(&server)
        .await;

    // Zipball 302-redirects to a codeload-style URL (reqwest follows).
    Mock::given(method("GET"))
        .and(path("/api/v3/repos/acme/skills/zipball/v1.2.0"))
        .and(header("Authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(302).insert_header(
            "Location",
            format!("{}/codeload/acme-skills-v1.2.0.zip", server.uri()).as_str(),
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/codeload/acme-skills-v1.2.0.zip"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(repo_zip("acme-skills-1234567"), "application/zip"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let vendor = GithubVendor::new(
        http(),
        "acme/skills".to_string(),
        Some(server.uri()),
        None, // absent ref -> cascade picks the highest stable tag
        Some("test-token".to_string()),
    );
    let tmp = tempfile::tempdir().unwrap();
    let mv = vendor.materialize(&cache(&tmp)).await.unwrap();

    assert_eq!(mv.ref_resolved.as_deref(), Some("v1.2.0"));
    assert!(
        mv.root
            .join(".claude")
            .join("skills")
            .join("code-review")
            .join("SKILL.md")
            .is_file()
    );
    // Extracted into the specced cache layout (host segment keeps the port).
    let rel = mv.root.strip_prefix(tmp.path()).unwrap();
    let rel_str = rel.to_string_lossy().replace('\\', "/");
    assert!(rel_str.starts_with(".skills-cache/github/"), "{rel_str}");
    assert!(rel_str.ends_with("/acme__skills/v1.2.0"), "{rel_str}");
}

#[tokio::test]
async fn github_vendor_satisfies_the_provider_contract() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/repos/acme/skills/zipball/v1.0.0"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(repo_zip("acme-skills-aaa"), "application/zip"),
        )
        // The contract materializes twice; the second run must be a cache
        // hit with no additional download.
        .expect(1)
        .mount(&server)
        .await;

    let vendor = GithubVendor::new(
        http(),
        "acme/skills".to_string(),
        Some(server.uri()),
        Some("v1.0.0".to_string()), // verbatim ref: no tags roundtrip
        None,
    );
    let tmp = tempfile::tempdir().unwrap();
    let mv = run_vendor_contract(
        &vendor,
        locators(),
        &cache(&tmp),
        &ContractExpectations {
            skill_ids: vec!["code-review".to_string(), "greeting".to_string()],
        },
    )
    .await;
    assert_eq!(mv.ref_resolved.as_deref(), Some("v1.0.0"));
}

#[tokio::test]
async fn github_cache_hit_skips_http_and_refresh_forces_redownload() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/repos/acme/skills/zipball/v1.0.0"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(repo_zip("acme-skills-bbb"), "application/zip"),
        )
        .expect(2) // initial download + one forced by --refresh
        .mount(&server)
        .await;

    let vendor = GithubVendor::new(
        http(),
        "acme/skills".to_string(),
        Some(server.uri()),
        Some("v1.0.0".to_string()),
        None,
    );
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = cache(&tmp);

    let first = vendor.materialize(&cache).await.unwrap();
    // Cache hit: no HTTP at all.
    let second = vendor.materialize(&cache).await.unwrap();
    assert_eq!(first.root, second.root);

    // Refresh: cached entry is dropped and re-downloaded.
    cache.refresh = true;
    let third = vendor.materialize(&cache).await.unwrap();
    assert_eq!(first.root, third.root);
    // `.expect(2)` on the mock asserts exactly two downloads on drop.
}

#[tokio::test]
async fn github_stale_partial_cache_without_marker_is_rebuilt() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/repos/acme/skills/zipball/v1.0.0"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(repo_zip("acme-skills-ccc"), "application/zip"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let vendor = GithubVendor::new(
        http(),
        "acme/skills".to_string(),
        Some(server.uri()),
        Some("v1.0.0".to_string()),
        None,
    );
    let tmp = tempfile::tempdir().unwrap();
    let cache = cache(&tmp);

    // Simulate an interrupted extraction: dir exists, no marker.
    let stale = skills_providers::cachepath::entry_dir(
        &cache.root,
        "github",
        Some(&server.uri()),
        "acme/skills",
        "v1.0.0",
    );
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("leftover.txt"), "partial").unwrap();

    let mv = vendor.materialize(&cache).await.unwrap();
    assert!(
        !mv.root.join("leftover.txt").exists(),
        "stale content purged"
    );
    assert!(mv.root.join(".claude").is_dir());
}

// --- GitLab -----------------------------------------------------------------

#[tokio::test]
async fn gitlab_deep_subgroup_flow_uses_percent_encoded_project_id() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(RawPathContains(
            "/api/v4/projects/org%2Fgroup%2Fsub%2Fproject/repository/tags",
        ))
        .and(query_param("per_page", "100"))
        .and(header("PRIVATE-TOKEN", "glpat-test"))
        .and(header("User-Agent", "ai-skills"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"[{"name":"v0.9.0"},{"name":"v1.0.0"}]"#,
            "application/json",
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(RawPathContains(
            "/api/v4/projects/org%2Fgroup%2Fsub%2Fproject/repository/archive.zip",
        ))
        .and(query_param("sha", "v1.0.0"))
        .and(header("PRIVATE-TOKEN", "glpat-test"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(repo_zip("project-v1.0.0-deadbeef"), "application/zip"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let vendor = GitlabVendor::new(
        http(),
        "org/group/sub/project".to_string(),
        Some(server.uri()),
        None,
        Some("glpat-test".to_string()),
    );
    let tmp = tempfile::tempdir().unwrap();
    let mv = vendor.materialize(&cache(&tmp)).await.unwrap();
    assert_eq!(mv.ref_resolved.as_deref(), Some("v1.0.0"));
    assert!(
        mv.root
            .join(".claude")
            .join("skills")
            .join("greeting")
            .join("SKILL.md")
            .is_file()
    );
    // Cache id segment encodes the full subgroup path.
    let rel = mv
        .root
        .strip_prefix(tmp.path())
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/");
    assert!(rel.contains("/org__group__sub__project/"), "{rel}");
}

#[tokio::test]
async fn gitlab_not_found_error_includes_the_auth_hint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(RawPathContains(
            "/api/v4/projects/org%2Fprivate/repository/tags",
        ))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_raw(r#"{"message":"404 Project Not Found"}"#, "application/json"),
        )
        .mount(&server)
        .await;

    let vendor = GitlabVendor::new(
        http(),
        "org/private".to_string(),
        Some(server.uri()),
        None, // cascade needs the tag listing -> hits the 404
        None,
    );
    let tmp = tempfile::tempdir().unwrap();
    let err = vendor.materialize(&cache(&tmp)).await.unwrap_err();
    let message = err.to_string();
    assert!(message.contains("HTTP 404"), "{message}");
    assert!(message.contains("GITLAB_TOKEN"), "{message}");
    assert!(message.contains("private projects as 404"), "{message}");
}

// --- by-url ------------------------------------------------------------------

#[tokio::test]
async fn by_url_sha256_verification_pass_and_fail() {
    let server = MockServer::start().await;
    let bytes = repo_zip("bundle");
    let good_sha = sha256_hex(&bytes);
    Mock::given(method("GET"))
        .and(path("/skills.zip"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(bytes, "application/zip"))
        .mount(&server)
        .await;

    let url = format!("{}/skills.zip", server.uri());
    let tmp = tempfile::tempdir().unwrap();

    // Matching sha256: extracted fine.
    let vendor = UrlVendor::new(http(), url.clone(), Some(good_sha));
    let mv = vendor.materialize(&cache(&tmp)).await.unwrap();
    assert!(mv.root.join(".claude").is_dir());

    // Mismatching sha256: provider error, nothing cached for that label.
    let vendor = UrlVendor::new(http(), url, Some("ab".repeat(32)));
    let err = vendor.materialize(&cache(&tmp)).await.unwrap_err();
    assert!(err.to_string().contains("sha256 mismatch"), "{err}");
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().fold(String::new(), |mut acc, b| {
        use std::fmt::Write;
        let _ = write!(acc, "{b:02x}");
        acc
    })
}
