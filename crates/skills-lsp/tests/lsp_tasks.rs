//! Integration tests: the "skills: set up gutter tasks" source code action,
//! the `skills.setupTasks` command, and the startup `.zed/tasks.json`
//! reconciliation. (Binary name avoids "update" — Windows UAC, os error 740.)

mod common;

use common::{TestClient, uri_string, write_file};
use serde_json::{Value, json};

const SETUP_TITLE: &str = "skills: set up gutter tasks (.zed/tasks.json)";

/// The current test binary — what the in-process server resolves via
/// `std::env::current_exe()`.
fn current_exe_str() -> String {
    std::env::current_exe()
        .expect("current exe")
        .to_string_lossy()
        .into_owned()
}

async fn code_action_titles(client: &mut TestClient, uri: &str) -> Vec<String> {
    let response = client
        .request(
            "textDocument/codeAction",
            json!({
                "textDocument": { "uri": uri },
                "range": { "start": { "line": 0, "character": 0 },
                           "end": { "line": 0, "character": 0 } },
                "context": { "diagnostics": [] },
            }),
        )
        .await;
    response
        .as_array()
        .map(|actions| {
            actions
                .iter()
                .filter_map(|a| a.get("title").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn setup_action_offered_on_manifest_but_not_on_skill_md() {
    let tmp = tempfile::tempdir().unwrap();
    write_file(&tmp.path().join("skills.json"), "{}");
    let skill_md = "---\nname: tidy\ndescription: d\n---\n# Tidy\n";
    write_file(
        &tmp.path()
            .join(".agents")
            .join("skills")
            .join("tidy")
            .join("SKILL.md"),
        skill_md,
    );

    let mut client = TestClient::start();
    client.initialize(Some(tmp.path())).await;

    // Manifest buffer: the source action is offered with no diagnostics.
    let manifest_uri = uri_string(&tmp.path().join("skills.json"));
    client.did_open(&manifest_uri, "json", "{}").await;
    let titles = code_action_titles(&mut client, &manifest_uri).await;
    assert_eq!(titles, vec![SETUP_TITLE.to_string()]);
    let response = client
        .request(
            "textDocument/codeAction",
            json!({
                "textDocument": { "uri": manifest_uri },
                "range": { "start": { "line": 0, "character": 0 },
                           "end": { "line": 0, "character": 0 } },
                "context": { "diagnostics": [] },
            }),
        )
        .await;
    let action = &response.as_array().expect("actions")[0];
    assert_eq!(
        action.pointer("/kind").and_then(Value::as_str),
        Some("source")
    );
    assert_eq!(
        action.pointer("/command/command").and_then(Value::as_str),
        Some("skills.setupTasks")
    );

    // SKILL.md buffer: no setup action.
    let skill_uri = uri_string(
        &tmp.path()
            .join(".agents")
            .join("skills")
            .join("tidy")
            .join("SKILL.md"),
    );
    client.did_open(&skill_uri, "markdown", skill_md).await;
    let titles = code_action_titles(&mut client, &skill_uri).await;
    assert!(titles.is_empty(), "{titles:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn setup_command_creates_the_file_then_reports_up_to_date() {
    let tmp = tempfile::tempdir().unwrap();
    write_file(&tmp.path().join("skills.json"), "{}");

    let mut client = TestClient::start();
    client.initialize(Some(tmp.path())).await;
    let exec = client
        .request(
            "workspace/executeCommand",
            json!({
                "command": "skills.setupTasks",
                "arguments": [tmp.path().to_string_lossy()],
            }),
        )
        .await;
    assert_eq!(exec, Value::Null);
    let (msg_type, message) = client.wait_show_message().await;
    assert_eq!(msg_type, 3, "expected Info, got: {message}"); // INFO
    assert!(message.contains("created .zed/tasks.json"), "{message}");

    let tasks_path = tmp.path().join(".zed").join("tasks.json");
    let content = std::fs::read_to_string(&tasks_path).expect("tasks.json written");
    // Commands point at this very process (JSON-escaped absolute path).
    let exe_json = serde_json::to_string(&current_exe_str()).unwrap();
    assert_eq!(content.matches(exe_json.as_str()).count(), 4, "{content}");
    insta::assert_snapshot!(content.replace(exe_json.as_str(), "\"[SERVER_EXE]\""));

    // Second invocation: no change, "already up to date".
    let exec = client
        .request(
            "workspace/executeCommand",
            json!({
                "command": "skills.setupTasks",
                "arguments": [tmp.path().to_string_lossy()],
            }),
        )
        .await;
    assert_eq!(exec, Value::Null);
    let (msg_type, message) = client.wait_show_message().await;
    assert_eq!(msg_type, 3, "expected Info, got: {message}");
    assert!(message.contains("already up to date"), "{message}");
    assert_eq!(std::fs::read_to_string(&tasks_path).unwrap(), content);
}

#[tokio::test(flavor = "multi_thread")]
async fn startup_reconcile_rewrites_a_stale_command_path() {
    let tmp = tempfile::tempdir().unwrap();
    write_file(&tmp.path().join("skills.json"), "{}");
    // Pre-seed a tasks.json whose managed entry points at a pruned binary,
    // plus a foreign task that must stay untouched.
    let stale = tmp.path().join("pruned").join("skills.exe"); // never created
    let seeded = format!(
        "[\n  {{\n    \"label\": \"skills: update\",\n    \"command\": {},\n    \"args\": [\"update\"],\n    \"cwd\": \"$ZED_WORKTREE_ROOT\",\n    \"tags\": [\"skills-sync\"]\n  }},\n  {{ \"label\": \"build\", \"command\": \"cargo\", \"args\": [\"build\"] }}\n]\n",
        serde_json::to_string(&stale.to_string_lossy()).unwrap(),
    );
    write_file(&tmp.path().join(".zed").join("tasks.json"), &seeded);

    let mut client = TestClient::start();
    client.initialize(Some(tmp.path())).await;

    // Reconcile runs on `initialized` and logs what it healed.
    let (msg_type, message) = client.wait_log_message().await;
    assert_eq!(msg_type, 3, "expected Info, got: {message}"); // INFO
    assert!(
        message.contains("rewrote 1 stale task command path"),
        "{message}"
    );

    let content = std::fs::read_to_string(tmp.path().join(".zed").join("tasks.json")).unwrap();
    let parsed: Vec<Value> = serde_json::from_str(&content).expect("valid JSON after rewrite");
    assert_eq!(
        parsed[0].get("command").and_then(Value::as_str),
        Some(current_exe_str().as_str()),
        "{content}"
    );
    // Foreign entry byte-identical.
    assert!(
        content.contains("{ \"label\": \"build\", \"command\": \"cargo\", \"args\": [\"build\"] }"),
        "{content}"
    );
}
