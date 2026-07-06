//! Watcher smoke test: an external filesystem change refreshes diagnostics
//! for open documents without any didChange from the client.

mod common;

use common::{TestClient, uri_string, write_file};
use serde_json::Value;

#[tokio::test(flavor = "multi_thread")]
async fn external_lockfile_removal_refreshes_diagnostics() {
    let tmp = tempfile::tempdir().unwrap();
    write_file(
        &tmp.path().join("skills-src").join("tidy").join("SKILL.md"),
        "---\nname: tidy\ndescription: d\n---\n",
    );
    let manifest = "{\n  \"local\": { \"dir\": [\"./skills-src\"] }\n}";
    write_file(&tmp.path().join("skills.json"), manifest);

    // Bring the project in sync first (real pipeline, local donor only).
    skills_lsp::update::run_real_update(tmp.path())
        .await
        .expect("initial sync");
    assert!(tmp.path().join("skills.lock").is_file());

    let mut client = TestClient::start();
    client.initialize(Some(tmp.path())).await;
    let uri = uri_string(&tmp.path().join("skills.json"));
    client.did_open(&uri, "json", manifest).await;
    let diags = client.wait_diagnostics(&uri).await;
    assert!(diags.is_empty(), "synced project must be clean: {diags:?}");

    // External change (no LSP notification): drop the lockfile. The server's
    // own watcher must pick it up and republish a staleness diagnostic.
    std::fs::remove_file(tmp.path().join("skills.lock")).expect("remove lock");
    let diags = client
        .wait_diagnostics_until(&uri, |diags| {
            diags
                .iter()
                .any(|d| d.get("code").and_then(Value::as_str) == Some("stale"))
        })
        .await;
    assert_eq!(diags.len(), 1, "{diags:?}");
}
