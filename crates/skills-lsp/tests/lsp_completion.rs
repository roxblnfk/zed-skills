//! Integration tests: SKILL.md frontmatter completion over an in-memory
//! LSP session.

mod common;

use common::{TestClient, uri_string, write_file};
use serde_json::{Value, json};
use std::path::Path;

/// Open a document and wait for its first diagnostics publish (ensures the
/// server has registered the buffer before the completion request).
async fn open_and_settle(client: &mut TestClient, uri: &str, language: &str, text: &str) {
    client.did_open(uri, language, text).await;
    client.wait_diagnostics(uri).await;
}

async fn completion_at(client: &mut TestClient, uri: &str, line: u32, character: u32) -> Value {
    client
        .request(
            "textDocument/completion",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
            }),
        )
        .await
}

fn labels(result: &Value) -> Vec<String> {
    result
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|i| i.get("label").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn skill_path(root: &Path, dir: &str) -> std::path::PathBuf {
    root.join(".agents")
        .join("skills")
        .join(dir)
        .join("SKILL.md")
}

#[tokio::test(flavor = "multi_thread")]
async fn key_position_lists_known_fields_excluding_present() {
    let tmp = tempfile::tempdir().unwrap();
    let path = skill_path(tmp.path(), "deploy");
    let text = "---\nname: deploy\ndescription: d\n\n---\n# Deploy\n";
    write_file(&path, text);

    let mut client = TestClient::start();
    client.initialize(Some(tmp.path())).await;
    let uri = uri_string(&path);
    open_and_settle(&mut client, &uri, "markdown", text).await;

    // Empty line inside the frontmatter block.
    let result = completion_at(&mut client, &uri, 3, 0).await;
    let mut labels = labels(&result);
    assert!(!labels.contains(&"name".to_string()), "{labels:?}");
    assert!(!labels.contains(&"description".to_string()), "{labels:?}");
    labels.sort();
    insta::assert_json_snapshot!("completion_key_labels", labels);
}

#[tokio::test(flavor = "multi_thread")]
async fn value_position_for_bool_field_yields_true_false() {
    let tmp = tempfile::tempdir().unwrap();
    let path = skill_path(tmp.path(), "deploy");
    let text = "---\nname: deploy\ndisable-model-invocation: \n---\n";
    write_file(&path, text);

    let mut client = TestClient::start();
    client.initialize(Some(tmp.path())).await;
    let uri = uri_string(&path);
    open_and_settle(&mut client, &uri, "markdown", text).await;

    let result = completion_at(&mut client, &uri, 2, 26).await;
    assert_eq!(labels(&result), ["true", "false"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn name_value_suggests_directory_name() {
    let tmp = tempfile::tempdir().unwrap();
    let path = skill_path(tmp.path(), "code-review");
    let text = "---\nname: \ndescription: d\n---\n";
    write_file(&path, text);

    let mut client = TestClient::start();
    client.initialize(Some(tmp.path())).await;
    let uri = uri_string(&path);
    open_and_settle(&mut client, &uri, "markdown", text).await;

    let result = completion_at(&mut client, &uri, 1, 6).await;
    assert_eq!(labels(&result), ["code-review"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn bootstrap_snippet_on_empty_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = skill_path(tmp.path(), "tidy");
    write_file(&path, "");

    let mut client = TestClient::start();
    client.initialize(Some(tmp.path())).await;
    let uri = uri_string(&path);
    open_and_settle(&mut client, &uri, "markdown", "").await;

    let result = completion_at(&mut client, &uri, 0, 0).await;
    let items = result.as_array().expect("completion array");
    assert_eq!(items.len(), 1, "{items:?}");
    let new_text = items[0]
        .pointer("/textEdit/newText")
        .and_then(Value::as_str)
        .expect("text edit");
    assert!(
        new_text.starts_with("---\nname: tidy\ndescription: "),
        "{new_text}"
    );
    assert!(new_text.contains("\n---\n"), "{new_text}");
    assert_eq!(
        items[0]
            .pointer("/insertTextFormat")
            .and_then(Value::as_i64),
        Some(2), // snippet
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn no_completions_outside_frontmatter() {
    let tmp = tempfile::tempdir().unwrap();
    let path = skill_path(tmp.path(), "tidy");
    let text = "---\nname: tidy\ndescription: d\n---\n# Body\n";
    write_file(&path, text);

    let mut client = TestClient::start();
    client.initialize(Some(tmp.path())).await;
    let uri = uri_string(&path);
    open_and_settle(&mut client, &uri, "markdown", text).await;

    // In the body.
    let result = completion_at(&mut client, &uri, 4, 0).await;
    assert_eq!(result, Value::Null, "{result:?}");
    // On the closing delimiter.
    let result = completion_at(&mut client, &uri, 3, 0).await;
    assert_eq!(result, Value::Null, "{result:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn no_completions_in_manifest_documents() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("skills.json");
    let text = "{}";
    write_file(&path, text);

    let mut client = TestClient::start();
    client.initialize(Some(tmp.path())).await;
    let uri = uri_string(&path);
    open_and_settle(&mut client, &uri, "json", text).await;

    let result = completion_at(&mut client, &uri, 0, 0).await;
    assert_eq!(result, Value::Null, "{result:?}");
}
