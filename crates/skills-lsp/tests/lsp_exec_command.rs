//! Integration tests: the "Run skills update" code action and the
//! `skills.update` command (real pipeline, local donors only — no network).

mod common;

use common::{TestClient, uri_string, write_file};
use serde_json::{Value, json};

fn stale_project(tmp: &std::path::Path) -> String {
    write_file(
        &tmp.join("skills-src").join("tidy").join("SKILL.md"),
        "---\nname: tidy\ndescription: d\n---\n# Tidy\n",
    );
    let manifest = "{\n  \"sources\": [ { \"from\": \"dir\", \"path\": \"./skills-src\" } ]\n}";
    write_file(&tmp.join("skills.json"), manifest);
    manifest.to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_diagnostic_offers_the_code_action_and_the_command_syncs() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = stale_project(tmp.path());

    let mut client = TestClient::start();
    client.initialize(Some(tmp.path())).await;
    let uri = uri_string(&tmp.path().join("skills.json"));
    client.did_open(&uri, "json", &manifest).await;

    // The project has one unsynced skill: expect the stale info.
    let diags = client.wait_diagnostics(&uri).await;
    assert_eq!(diags.len(), 1, "{diags:?}");
    let stale = &diags[0];
    assert_eq!(stale.get("code").and_then(Value::as_str), Some("stale"));

    // The stale diagnostic carries the "Run skills update" code action.
    let response = client
        .request(
            "textDocument/codeAction",
            json!({
                "textDocument": { "uri": uri },
                "range": stale.get("range").cloned().unwrap(),
                "context": { "diagnostics": [stale] },
            }),
        )
        .await;
    let actions = response.as_array().expect("code action list");
    // The quickfix plus the always-offered "set up gutter tasks" source action.
    assert_eq!(actions.len(), 2);
    let action = actions
        .iter()
        .find(|a| a.get("title").and_then(Value::as_str) == Some("Run skills update"))
        .expect("quickfix listed");
    let command = action.get("command").expect("command attached");
    assert_eq!(
        command.get("command").and_then(Value::as_str),
        Some("skills.update")
    );

    // Execute it: the real pipeline runs in-process and writes the target.
    let exec = client
        .request(
            "workspace/executeCommand",
            json!({
                "command": "skills.update",
                "arguments": command.get("arguments").cloned().unwrap(),
            }),
        )
        .await;
    assert_eq!(exec, Value::Null);

    let (msg_type, message) = client.wait_show_message().await;
    assert_eq!(msg_type, 3, "expected Info, got: {message}"); // MessageType::INFO
    assert!(message.contains("1 added"), "{message}");

    // On disk: target synced + lockfile written.
    assert!(
        tmp.path()
            .join(".agents")
            .join("skills")
            .join("tidy")
            .join("SKILL.md")
            .is_file()
    );
    assert!(tmp.path().join("skills.lock").is_file());

    // Republished diagnostics are clean.
    let diags = client
        .wait_diagnostics_until(&uri, |diags| diags.is_empty())
        .await;
    assert!(diags.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn command_failure_is_reported_via_show_message() {
    let tmp = tempfile::tempdir().unwrap();
    // Manifest referencing a missing sources dir path: prepare succeeds,
    // discover fails — the command must surface an error message, not crash.
    write_file(
        &tmp.path().join("skills.json"),
        "{ \"sources\": [ { \"from\": \"dir\", \"path\": \"./nope\" } ] }",
    );

    let mut client = TestClient::start();
    client.initialize(Some(tmp.path())).await;
    let exec = client
        .request(
            "workspace/executeCommand",
            json!({
                "command": "skills.update",
                "arguments": [tmp.path().to_string_lossy()],
            }),
        )
        .await;
    assert_eq!(exec, Value::Null);
    let (msg_type, message) = client.wait_show_message().await;
    assert_eq!(msg_type, 1, "expected Error, got: {message}"); // MessageType::ERROR
    assert!(message.contains("./nope"), "{message}");
}
