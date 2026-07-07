//! Keep `resources/skills.schema.json` aligned with the serde model.

use skills_core::manifest::Manifest;

const SCHEMA: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../resources/skills.schema.json"
));

#[test]
fn schema_is_valid_json_draft_2020_12() {
    let schema: serde_json::Value = serde_json::from_str(SCHEMA).unwrap();
    assert_eq!(
        schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_eq!(schema["additionalProperties"], false);
}

/// The schema's `$id` must be the canonical published URL — the same one
/// `skills init` / `skills add` write into generated manifests.
#[test]
fn schema_id_is_the_published_url() {
    let schema: serde_json::Value = serde_json::from_str(SCHEMA).unwrap();
    assert_eq!(schema["$id"], skills_core::manifest::SCHEMA_URL);
}

#[test]
fn schema_top_level_properties_match_serde_model() {
    let schema: serde_json::Value = serde_json::from_str(SCHEMA).unwrap();
    let mut props: Vec<&str> = schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    props.sort_unstable();
    // Mirror of the fields on `Manifest` (kebab-case).
    let mut expected = vec![
        "$schema",
        "target",
        "aliases",
        "lock-file",
        "trusted",
        "trusted-replace",
        "discovery",
        "local",
        "sources",
        "remote",
        "audit",
        "path-from-root",
    ];
    expected.sort_unstable();
    assert_eq!(props, expected);

    // Every schema property must be accepted by the serde model (otherwise
    // deny_unknown_fields would reject documents the schema allows).
    let doc = r#"{
        "$schema": "x",
        "target": "t/skills",
        "aliases": ["a/skills"],
        "lock-file": "meta/skills.lock",
        "trusted": ["v/p"],
        "trusted-replace": true,
        "discovery": true,
        "local": { "composer": false, "npm": true, "go": true },
        "sources": [
            { "from": "github", "package": "a/b", "ref": "v1", "host": "h", "skills": [] },
            { "from": "zip", "url": "https://example.com/s.zip", "sha256": "abc" },
            { "from": "dir", "path": "./skills-src", "package": "acme/local-skills", "skills": ["x"] }
        ],
        "audit": { "mode": "warn", "pipeline": [
            { "use": "static", "on-fail": "warn" },
            { "use": "llm", "model": "m", "on-fail": "warn" },
            { "use": "http", "url": "https://a", "on-fail": "block" }
        ] },
        "path-from-root": "pkg/app"
    }"#;
    Manifest::parse(doc).unwrap();
}

/// The deprecated `remote` alias must still parse (back-compat manifests).
#[test]
fn legacy_remote_keyed_doc_still_parses() {
    let doc = r#"{
        "remote": [
            { "from": "github", "package": "a/b", "ref": "v1", "host": "h", "skills": [] },
            { "from": "zip", "url": "https://example.com/s.zip", "sha256": "abc" }
        ]
    }"#;
    let m = Manifest::parse(doc).unwrap();
    assert!(m.uses_deprecated_remote());
    assert_eq!(m.sources().len(), 2);
}

#[test]
fn schema_source_enums_match_model() {
    let schema: serde_json::Value = serde_json::from_str(SCHEMA).unwrap();
    let from_enum = |def: &str| -> Vec<String> {
        schema["$defs"][def]["properties"]["from"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(from_enum("sourceByPackage"), ["github", "gitlab"]);
    assert_eq!(from_enum("sourceByUrl"), ["http", "zip"]);
    assert_eq!(from_enum("sourceByDir"), ["dir"]);
}

/// `remote` is a deprecated alias of `sources`; the top-level `not` guard
/// forbids setting both (mirrors the serde-side validation).
#[test]
fn schema_marks_remote_deprecated_and_guards_both_keys() {
    let schema: serde_json::Value = serde_json::from_str(SCHEMA).unwrap();
    assert_eq!(schema["properties"]["remote"]["deprecated"], true);
    assert_eq!(
        schema["not"]["required"],
        serde_json::json!(["sources", "remote"])
    );
    // Both lists share the same entry shapes.
    assert_eq!(
        schema["properties"]["remote"]["items"],
        schema["properties"]["sources"]["items"]
    );
}

#[test]
fn schema_audit_pipeline_matches_model() {
    let schema: serde_json::Value = serde_json::from_str(SCHEMA).unwrap();
    let items = &schema["properties"]["audit"]["properties"]["pipeline"]["items"];

    // The `use` enum mirrors `manifest::AUDITOR_IDS`.
    let ids: Vec<&str> = items["properties"]["use"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(ids, skills_core::manifest::AUDITOR_IDS);

    // Unknown per-entry fields must be rejected by the schema too
    // (mirrors the per-variant deny_unknown_fields).
    assert_eq!(items["additionalProperties"], false);
    let mode = &schema["properties"]["audit"]["properties"]["mode"]["enum"];
    assert_eq!(
        mode.as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>(),
        ["off", "warn", "block"]
    );
}
