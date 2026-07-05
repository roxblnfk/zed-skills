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
        "trusted",
        "trusted-replace",
        "discovery",
        "local",
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
        "trusted": ["v/p"],
        "trusted-replace": true,
        "discovery": true,
        "local": { "composer": false, "dir": ["./d"], "npm": true, "go": true },
        "remote": [ { "from": "github", "package": "a/b", "ref": "v1", "host": "h", "skills": [] } ],
        "audit": { "mode": "warn", "pipeline": [ { "use": "static", "on-fail": "warn" } ] },
        "path-from-root": "pkg/app"
    }"#;
    Manifest::parse(doc).unwrap();
}

#[test]
fn schema_remote_enum_matches_model() {
    let schema: serde_json::Value = serde_json::from_str(SCHEMA).unwrap();
    let from = &schema["properties"]["remote"]["items"]["properties"]["from"]["enum"];
    assert_eq!(
        from.as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>(),
        ["github", "gitlab", "http", "zip"]
    );
}
