//! Integration tests: SKILL.md frontmatter hover over an in-memory LSP
//! session.

mod common;

use common::{TestClient, uri_string, write_file};
use serde_json::{Value, json};
use std::path::Path;

/// Open a document and wait for its first diagnostics publish (ensures the
/// server has registered the buffer before the hover request).
async fn open_and_settle(client: &mut TestClient, uri: &str, language: &str, text: &str) {
    client.did_open(uri, language, text).await;
    client.wait_diagnostics(uri).await;
}

async fn hover_at(client: &mut TestClient, uri: &str, line: u32, character: u32) -> Value {
    client
        .request(
            "textDocument/hover",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
            }),
        )
        .await
}

fn skill_path(root: &Path, dir: &str) -> std::path::PathBuf {
    root.join(".agents")
        .join("skills")
        .join(dir)
        .join("SKILL.md")
}

#[tokio::test(flavor = "multi_thread")]
async fn hover_on_known_key_returns_markdown_docs() {
    let tmp = tempfile::tempdir().unwrap();
    let path = skill_path(tmp.path(), "deploy");
    let text = "---\nname: deploy\ndescription: Deploys things.\neffort: high\n---\n# Deploy\n";
    write_file(&path, text);

    let mut client = TestClient::start();
    client.initialize(Some(tmp.path())).await;
    let uri = uri_string(&path);
    open_and_settle(&mut client, &uri, "markdown", text).await;

    // The initialize result declared the capability.
    // (Checked here to keep the capability pinned by an integration test.)
    let result = hover_at(&mut client, &uri, 3, 2).await; // on `effort`
    insta::assert_json_snapshot!("hover_effort_key", result);

    // Value position → null (pinned policy: value hovers miss).
    let result = hover_at(&mut client, &uri, 3, 9).await;
    assert_eq!(result, Value::Null);

    // Body position → null.
    let result = hover_at(&mut client, &uri, 5, 2).await;
    assert_eq!(result, Value::Null);
}

#[tokio::test(flavor = "multi_thread")]
async fn hover_capability_is_declared() {
    let tmp = tempfile::tempdir().unwrap();
    let mut client = TestClient::start();
    let init = client.initialize(Some(tmp.path())).await;
    assert_eq!(
        init.pointer("/capabilities/hoverProvider"),
        Some(&json!(true))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn hover_in_manifest_returns_null() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("skills.json");
    let text = r#"{ "target": ".agents/skills" }"#;
    write_file(&path, text);

    let mut client = TestClient::start();
    client.initialize(Some(tmp.path())).await;
    let uri = uri_string(&path);
    open_and_settle(&mut client, &uri, "json", text).await;

    let result = hover_at(&mut client, &uri, 0, 3).await;
    assert_eq!(result, Value::Null);
}
