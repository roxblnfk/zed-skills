//! Integration tests: `skills.json` document links over an in-memory LSP
//! session.

mod common;

use common::{TestClient, uri_string, write_file};
use serde_json::{Value, json};

async fn document_links(client: &mut TestClient, uri: &str) -> Value {
    client
        .request(
            "textDocument/documentLink",
            json!({ "textDocument": { "uri": uri } }),
        )
        .await
}

/// Rangeless, path-redacted summary: `{target, tooltip}` per link. Ranges
/// are pinned by the unit tests; the temp project root varies per run.
fn summarize(result: &Value, root_uri: &str) -> Vec<Value> {
    result
        .as_array()
        .expect("array of links")
        .iter()
        .map(|link| {
            let target = link
                .get("target")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .replace(root_uri, "<TMP>");
            json!({
                "target": target,
                "tooltip": link.get("tooltip").cloned().unwrap_or(Value::Null),
            })
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn manifest_links_cover_all_entry_kinds() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("skills-src")).unwrap();
    let path = tmp.path().join("skills.json");
    let text = r#"{
  "local": { "dir": ["./skills-src"] },
  "remote": [
    { "from": "github", "package": "acme/skills" },
    { "from": "gitlab", "package": "org/group/sub/project" },
    { "from": "zip", "url": "https://example.com/skills.zip" }
  ]
}"#;
    write_file(&path, text);

    let mut client = TestClient::start();
    let init = client.initialize(Some(tmp.path())).await;
    // No-resolve capability declared alongside the links themselves.
    assert_eq!(
        init.pointer("/capabilities/documentLinkProvider/resolveProvider"),
        Some(&json!(false))
    );
    let uri = uri_string(&path);
    client.did_open(&uri, "json", text).await;
    client.wait_diagnostics(&uri).await;

    let result = document_links(&mut client, &uri).await;
    let summary = summarize(&result, &uri_string(tmp.path()));
    insta::assert_json_snapshot!("document_links_summary", summary);
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_manifest_yields_empty_links() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("skills.json");
    let text = "{ not json at all";
    write_file(&path, text);

    let mut client = TestClient::start();
    client.initialize(Some(tmp.path())).await;
    let uri = uri_string(&path);
    client.did_open(&uri, "json", text).await;
    client.wait_diagnostics(&uri).await;

    let result = document_links(&mut client, &uri).await;
    assert_eq!(result, json!([]));
}

#[tokio::test(flavor = "multi_thread")]
async fn skill_md_gets_no_links() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp
        .path()
        .join(".agents")
        .join("skills")
        .join("s")
        .join("SKILL.md");
    let text = "---\nname: s\ndescription: d\n---\n";
    write_file(&path, text);

    let mut client = TestClient::start();
    client.initialize(Some(tmp.path())).await;
    let uri = uri_string(&path);
    client.did_open(&uri, "markdown", text).await;
    client.wait_diagnostics(&uri).await;

    let result = document_links(&mut client, &uri).await;
    assert_eq!(result, Value::Null);
}
