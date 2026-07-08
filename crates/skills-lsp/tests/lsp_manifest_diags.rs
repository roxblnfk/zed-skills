//! Integration tests: skills.json diagnostics over an in-memory LSP session.

mod common;

use common::{TestClient, simplify, uri_string, write_file};
use serde_json::Value;

fn code_of(diag: &Value) -> &str {
    diag.get("code").and_then(Value::as_str).unwrap_or("")
}

/// A project dir + an initialized client with the manifest open.
async fn open_manifest(project: &std::path::Path, manifest: &str) -> (TestClient, String) {
    write_file(&project.join("skills.json"), manifest);
    let mut client = TestClient::start();
    client.initialize(Some(project)).await;
    let uri = uri_string(&project.join("skills.json"));
    client.did_open(&uri, "json", manifest).await;
    (client, uri)
}

#[tokio::test(flavor = "multi_thread")]
async fn parse_error_is_anchored_at_the_reported_position() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = "{\n  \"target\": 5\n}";
    let (mut client, uri) = open_manifest(tmp.path(), manifest).await;
    let diags = client.wait_diagnostics(&uri).await;
    insta::assert_json_snapshot!("parse_error", simplify(&diags));
}

#[tokio::test(flavor = "multi_thread")]
async fn semantic_errors_are_anchored_to_their_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = concat!(
        "{\n",
        "  \"dependencies\": { \"composer\": { \"trusted\": [\"acme/*\", \"bare\"] } },\n",
        "  \"aliases\": [\".agents/skills\"]\n",
        "}",
    );
    let (mut client, uri) = open_manifest(tmp.path(), manifest).await;
    let diags = client.wait_diagnostics(&uri).await;
    insta::assert_json_snapshot!("semantic_errors", simplify(&diags));
}

#[tokio::test(flavor = "multi_thread")]
async fn conflict_between_two_dir_donors_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    for donor in ["a-skills", "b-skills"] {
        write_file(
            &tmp.path().join(donor).join("clash").join("SKILL.md"),
            "---\nname: clash\ndescription: d\n---\n",
        );
    }
    let manifest = concat!(
        "{\n",
        "  \"sources\": [\n",
        "    { \"from\": \"dir\", \"path\": \"./a-skills\" },\n",
        "    { \"from\": \"dir\", \"path\": \"./b-skills\" }\n",
        "  ]\n",
        "}",
    );
    let (mut client, uri) = open_manifest(tmp.path(), manifest).await;
    let diags = client.wait_diagnostics(&uri).await;
    insta::assert_json_snapshot!("conflict", simplify(&diags));
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_lockfile_is_an_info_on_the_file_top() {
    let tmp = tempfile::tempdir().unwrap();
    write_file(
        &tmp.path().join("skills-src").join("tidy").join("SKILL.md"),
        "---\nname: tidy\ndescription: d\n---\n",
    );
    let manifest = "{\n  \"sources\": [ { \"from\": \"dir\", \"path\": \"./skills-src\" } ]\n}";
    let (mut client, uri) = open_manifest(tmp.path(), manifest).await;
    let diags = client.wait_diagnostics(&uri).await;
    insta::assert_json_snapshot!("stale_lock", simplify(&diags));
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_allowlist_name_warns_on_the_exact_element() {
    let tmp = tempfile::tempdir().unwrap();
    // Simulate a previously fetched github donor: completed cache entry with
    // one skill in a well-known container.
    let entry = tmp
        .path()
        .join(".skills-cache")
        .join("github")
        .join("default")
        .join("acme__skills")
        .join("v1.0.0");
    write_file(
        &entry.join("skills").join("alpha").join("SKILL.md"),
        "---\nname: alpha\ndescription: d\n---\n",
    );
    write_file(&entry.join(".skills-cache-ok"), "v1.0.0");

    let manifest = concat!(
        "{\n",
        "  \"sources\": [\n",
        "    { \"from\": \"github\", \"package\": \"acme/skills\", \"ref\": \"v1.0.0\",\n",
        "      \"skills\": [\"alpha\", \"ghost\"] }\n",
        "  ]\n",
        "}",
    );
    let (mut client, uri) = open_manifest(tmp.path(), manifest).await;
    let diags = client.wait_diagnostics(&uri).await;
    insta::assert_json_snapshot!("unknown_allowlist", simplify(&diags));
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_not_in_cache_is_a_not_fetched_hint() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = concat!(
        "{\n",
        "  \"sources\": [\n",
        "    { \"from\": \"github\", \"package\": \"acme/skills\", \"ref\": \"v1.0.0\" }\n",
        "  ]\n",
        "}",
    );
    let (mut client, uri) = open_manifest(tmp.path(), manifest).await;
    let diags = client.wait_diagnostics(&uri).await;
    insta::assert_json_snapshot!("not_fetched", simplify(&diags));
}

#[tokio::test(flavor = "multi_thread")]
async fn deprecated_remote_alias_warns_and_still_analyzes() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = concat!(
        "{\n",
        "  \"remote\": [\n",
        "    { \"from\": \"github\", \"package\": \"acme/skills\", \"ref\": \"v1.0.0\" }\n",
        "  ]\n",
        "}",
    );
    let (mut client, uri) = open_manifest(tmp.path(), manifest).await;
    let diags = client.wait_diagnostics(&uri).await;
    insta::assert_json_snapshot!("deprecated_remote", simplify(&diags));
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_dir_source_error_is_anchored_at_its_path() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = concat!(
        "{\n",
        "  \"sources\": [\n",
        "    { \"from\": \"dir\", \"path\": \"./nope\" }\n",
        "  ]\n",
        "}",
    );
    let (mut client, uri) = open_manifest(tmp.path(), manifest).await;
    let diags = client.wait_diagnostics(&uri).await;
    insta::assert_json_snapshot!("missing_dir_source", simplify(&diags));
}

#[tokio::test(flavor = "multi_thread")]
async fn did_change_fixing_the_error_clears_diagnostics() {
    let tmp = tempfile::tempdir().unwrap();
    let broken = "{ \"dependencies\": { \"composer\": { \"trusted\": [\"bare\"] } } }";
    let (mut client, uri) = open_manifest(tmp.path(), broken).await;
    let diags = client.wait_diagnostics(&uri).await;
    assert_eq!(diags.len(), 1);
    assert_eq!(code_of(&diags[0]), "invalid");

    client.did_change_full(&uri, 2, "{}").await;
    let diags = client
        .wait_diagnostics_until(&uri, |diags| diags.is_empty())
        .await;
    assert!(diags.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn rapid_changes_publish_only_the_final_state() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut client, uri) = open_manifest(tmp.path(), "{}").await;
    let first = client.wait_diagnostics(&uri).await;
    assert!(first.is_empty());

    // A burst of edits: intermediate states are broken, the last is clean.
    client.did_change_full(&uri, 2, "{ not json").await;
    client
        .did_change_full(
            &uri,
            3,
            "{ \"dependencies\": { \"composer\": { \"trusted\": [\"bare\"] } } }",
        )
        .await;
    client.did_change_full(&uri, 4, "{ }").await;
    let diags = client
        .wait_diagnostics_until(&uri, |diags| diags.is_empty())
        .await;
    assert!(diags.is_empty());
}
