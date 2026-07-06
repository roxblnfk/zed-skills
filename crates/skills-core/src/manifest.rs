//! `skills.json` manifest (schema v2): serde model + validation.
//!
//! Strict at the top level: unknown keys are fatal (`deny_unknown_fields`).
//! The published JSON Schema (`resources/skills.schema.json`) mirrors this
//! model.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::ManifestError;
use crate::paths::normalize_rel;

pub const MANIFEST_NAME: &str = "skills.json";
pub const DEFAULT_TARGET: &str = ".agents/skills";

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Manifest {
    #[serde(rename = "$schema")]
    pub schema: Option<String>,
    pub target: Option<String>,
    pub aliases: Option<Vec<String>>,
    pub trusted: Option<Vec<String>>,
    pub trusted_replace: Option<bool>,
    pub discovery: Option<bool>,
    pub local: Option<LocalConfig>,
    pub remote: Option<Vec<RemoteEntry>>,
    pub audit: Option<AuditConfig>,
    /// Monorepo re-anchor. Validated for shape; semantics land in M5.
    pub path_from_root: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct LocalConfig {
    pub composer: Option<bool>,
    pub dir: Option<Vec<String>>,
    /// Reserved for future providers.
    pub npm: Option<bool>,
    /// Reserved for future providers.
    pub go: Option<bool>,
}

/// One `remote[]` entry. A tagged union over `from`:
/// by-package (`github`/`gitlab`, requires `package`) or by-url
/// (`http`/`zip`, requires `url`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct RemoteEntry {
    pub from: String,
    pub package: Option<String>,
    pub url: Option<String>,
    pub host: Option<String>,
    #[serde(rename = "ref")]
    pub r#ref: Option<String>,
    pub sha256: Option<String>,
    /// Tri-state: absent/null = all skills; `[]` = donor registered, pulls
    /// nothing; non-empty = allowlist matched by canonical name.
    pub skills: Option<Vec<String>>,
}

const BY_PACKAGE_SOURCES: &[&str] = &["github", "gitlab"];
const BY_URL_SOURCES: &[&str] = &["http", "zip"];

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct AuditConfig {
    pub mode: Option<AuditMode>,
    /// Ordered auditor chain. Absent = the default chain (a single
    /// `static` entry); an explicit `[]` = no auditors.
    pub pipeline: Option<Vec<AuditStep>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuditMode {
    #[default]
    Off,
    Warn,
    Block,
}

/// One `audit.pipeline` entry, tagged by `use`. Unknown auditor ids and
/// unknown per-entry fields are config errors (exit 1).
#[derive(Debug, Clone, PartialEq)]
pub enum AuditStep {
    Static(StaticStep),
    Llm(LlmStep),
    Http(HttpStep),
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct StaticStep {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_fail: Option<OnFail>,
}

/// Config of the (not yet implemented) LLM auditor. Accepted by the schema
/// for forward compatibility; constructing the auditor is a config error.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct LlmStep {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_fail: Option<OnFail>,
}

/// Config of the (not yet implemented) HTTP auditor. Accepted by the schema
/// for forward compatibility; constructing the auditor is a config error.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct HttpStep {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_fail: Option<OnFail>,
}

pub const AUDITOR_IDS: &[&str] = &["static", "llm", "http"];

impl AuditStep {
    pub fn id(&self) -> &'static str {
        match self {
            AuditStep::Static(_) => "static",
            AuditStep::Llm(_) => "llm",
            AuditStep::Http(_) => "http",
        }
    }

    pub fn on_fail(&self) -> Option<OnFail> {
        match self {
            AuditStep::Static(c) => c.on_fail,
            AuditStep::Llm(c) => c.on_fail,
            AuditStep::Http(c) => c.on_fail,
        }
    }

    /// Canonical JSON value (options + `use`, keys sorted by serde_json's
    /// BTreeMap) — the input of the auditor-set hash.
    pub fn canonical(&self) -> serde_json::Value {
        let value = match self {
            AuditStep::Static(c) => serde_json::to_value(c),
            AuditStep::Llm(c) => serde_json::to_value(c),
            AuditStep::Http(c) => serde_json::to_value(c),
        };
        // Step configs are plain optional fields; serialization cannot fail.
        let mut obj = match value.expect("audit step serializes") {
            serde_json::Value::Object(obj) => obj,
            _ => serde_json::Map::new(),
        };
        obj.insert("use".to_string(), serde_json::Value::from(self.id()));
        serde_json::Value::Object(obj)
    }
}

// Manual impl: serde's internally-tagged representation does not support
// `deny_unknown_fields`, and we want a precise "unknown auditor" message.
impl<'de> Deserialize<'de> for AuditStep {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        let mut map = serde_json::Map::deserialize(deserializer)?;
        let tag = map
            .remove("use")
            .ok_or_else(|| D::Error::custom("audit.pipeline entry: missing field `use`"))?;
        let Some(id) = tag.as_str().map(str::to_string) else {
            return Err(D::Error::custom(
                "audit.pipeline entry: `use` must be a string",
            ));
        };
        let rest = serde_json::Value::Object(map);
        let step = match id.as_str() {
            "static" => serde_json::from_value(rest).map(AuditStep::Static),
            "llm" => serde_json::from_value(rest).map(AuditStep::Llm),
            "http" => serde_json::from_value(rest).map(AuditStep::Http),
            other => {
                return Err(D::Error::custom(format!(
                    "audit.pipeline entry: unknown auditor '{other}' (expected one of: {})",
                    AUDITOR_IDS.join(", ")
                )));
            }
        };
        step.map_err(|e| D::Error::custom(format!("audit.pipeline entry ({id}): {e}")))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OnFail {
    Warn,
    Block,
}

impl Manifest {
    /// Load and validate the manifest at `path`.
    pub fn load(path: &Path) -> Result<Manifest, ManifestError> {
        if !path.is_file() {
            return Err(ManifestError::NotFound {
                path: path.to_path_buf(),
            });
        }
        let raw = std::fs::read_to_string(path).map_err(|source| ManifestError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Manifest::parse(&raw)
    }

    /// Parse and validate manifest JSON.
    pub fn parse(raw: &str) -> Result<Manifest, ManifestError> {
        let manifest: Manifest = serde_json::from_str(raw).map_err(ManifestError::Parse)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Effective sync target (normalized, `/`-separated, relative).
    pub fn effective_target(&self) -> String {
        match &self.target {
            Some(t) => normalize_rel(t).unwrap_or_else(|_| DEFAULT_TARGET.to_string()),
            None => DEFAULT_TARGET.to_string(),
        }
    }

    /// Effective audit mode (default: off).
    pub fn audit_mode(&self) -> AuditMode {
        self.audit.as_ref().and_then(|a| a.mode).unwrap_or_default()
    }

    /// Effective audit pipeline: configured steps; an absent `pipeline`
    /// defaults to a single `static` entry, an explicit `[]` disables all
    /// auditors.
    pub fn audit_steps(&self) -> Vec<AuditStep> {
        match self.audit.as_ref().and_then(|a| a.pipeline.clone()) {
            Some(steps) => steps,
            None => vec![AuditStep::Static(StaticStep::default())],
        }
    }

    /// Declared `local.dir` entries (empty if absent).
    pub fn local_dirs(&self) -> &[String] {
        self.local
            .as_ref()
            .and_then(|l| l.dir.as_deref())
            .unwrap_or(&[])
    }

    /// `local.composer` toggle (default: enabled, SPEC §4).
    pub fn composer_enabled(&self) -> bool {
        self.local.as_ref().and_then(|l| l.composer).unwrap_or(true)
    }

    /// Parsed project `trusted` patterns. Validation already rejected
    /// malformed entries, so unparseable leftovers are ignored.
    pub fn trusted_patterns(&self) -> Vec<crate::pattern::VendorPattern> {
        self.trusted
            .iter()
            .flatten()
            .filter_map(|p| crate::pattern::VendorPattern::parse(p).ok())
            .collect()
    }

    fn validate(&self) -> Result<(), ManifestError> {
        let invalid = |msg: String| ManifestError::Invalid(msg);

        // target
        let target_norm = match &self.target {
            Some(t) => normalize_rel(t).map_err(|e| invalid(format!("invalid target: {e}")))?,
            None => DEFAULT_TARGET.to_string(),
        };

        // aliases
        if let Some(aliases) = &self.aliases {
            let mut seen = HashSet::new();
            for alias in aliases {
                let norm =
                    normalize_rel(alias).map_err(|e| invalid(format!("invalid alias: {e}")))?;
                if norm == target_norm {
                    return Err(invalid(format!(
                        "alias '{alias}' must not equal the target '{target_norm}'"
                    )));
                }
                if !seen.insert(norm.clone()) {
                    return Err(invalid(format!("duplicate alias '{norm}'")));
                }
            }
        }

        // trusted patterns: vendor/package or vendor/* (exactly one slash,
        // both sides non-empty)
        if let Some(trusted) = &self.trusted {
            for pattern in trusted {
                if crate::pattern::VendorPattern::parse(pattern).is_err() {
                    return Err(invalid(format!(
                        "invalid trusted pattern '{pattern}': expected 'vendor/package' or 'vendor/*'"
                    )));
                }
            }
        }

        // local.dir
        if let Some(dirs) = self.local.as_ref().and_then(|l| l.dir.as_ref()) {
            let mut seen = HashSet::new();
            for dir in dirs {
                if dir.trim().is_empty() {
                    return Err(invalid("local.dir entry must not be empty".to_string()));
                }
                let key = dir.trim().replace('\\', "/");
                if !seen.insert(key.clone()) {
                    return Err(invalid(format!("duplicate local.dir entry '{dir}'")));
                }
            }
        }

        // remote entries
        if let Some(remotes) = &self.remote {
            let mut seen = HashSet::new();
            for (idx, entry) in remotes.iter().enumerate() {
                let key = validate_remote_entry(entry)
                    .map_err(|e| invalid(format!("remote[{idx}]: {e}")))?;
                if !seen.insert(key.clone()) {
                    return Err(invalid(format!("remote[{idx}]: duplicate entry '{key}'")));
                }
            }
        }

        // path-from-root: relative, plain segments only
        if let Some(p) = &self.path_from_root {
            if p.trim().is_empty() {
                return Err(invalid("path-from-root must not be empty".to_string()));
            }
            let plain = !crate::paths::is_absolute_like(p)
                && p.split(['/', '\\'])
                    .all(|seg| !seg.is_empty() && seg != "." && seg != "..");
            if !plain {
                return Err(invalid(format!(
                    "path-from-root must be relative with plain segments, got '{p}'"
                )));
            }
        }

        Ok(())
    }
}

/// Validate one remote entry; returns its uniqueness key
/// (`from|host|identifier`).
fn validate_remote_entry(entry: &RemoteEntry) -> Result<String, String> {
    let from = entry.from.as_str();
    let by_package = BY_PACKAGE_SOURCES.contains(&from);
    let by_url = BY_URL_SOURCES.contains(&from);
    if !by_package && !by_url {
        return Err(format!(
            "unknown source '{from}' (expected one of: github, gitlab, http, zip)"
        ));
    }
    match (&entry.package, &entry.url) {
        (Some(_), Some(_)) => {
            return Err("exactly one of 'package'/'url' must be set, got both".to_string());
        }
        (None, None) => {
            return Err("exactly one of 'package'/'url' must be set, got neither".to_string());
        }
        _ => {}
    }
    let identifier = if by_package {
        let Some(package) = &entry.package else {
            return Err(format!("source '{from}' requires 'package'"));
        };
        if entry.sha256.is_some() {
            return Err(format!("'sha256' is not allowed with source '{from}'"));
        }
        if package.trim().is_empty() {
            return Err("'package' must not be empty".to_string());
        }
        package.clone()
    } else {
        let Some(url) = &entry.url else {
            return Err(format!("source '{from}' requires 'url'"));
        };
        if entry.host.is_some() {
            return Err(format!("'host' is not allowed with source '{from}'"));
        }
        if entry.r#ref.is_some() {
            return Err(format!("'ref' is not allowed with source '{from}'"));
        }
        if url.trim().is_empty() {
            return Err("'url' must not be empty".to_string());
        }
        url.clone()
    };
    let host = entry.host.clone().unwrap_or_else(|| "default".to_string());
    Ok(format!("{from}|{host}|{identifier}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(raw: &str) -> String {
        Manifest::parse(raw).unwrap_err().to_string()
    }

    #[test]
    fn empty_object_accepted() {
        let m = Manifest::parse("{}").unwrap();
        assert_eq!(m.effective_target(), DEFAULT_TARGET);
        assert_eq!(m.audit_mode(), AuditMode::Off);
        assert!(m.local_dirs().is_empty());
    }

    #[test]
    fn fully_populated_accepted() {
        let m = Manifest::parse(
            r#"{
                "$schema": "https://example.com/skills.schema.json",
                "target": ".agents/skills",
                "aliases": [".claude/skills", ".cursor/skills"],
                "trusted": ["acme/*", "acme/skills"],
                "trusted-replace": false,
                "discovery": false,
                "local": { "composer": true, "dir": ["./skills-src"], "npm": false, "go": false },
                "remote": [
                    { "from": "github", "package": "acme/skills", "ref": "v1.2.0", "skills": ["code-review"] },
                    { "from": "gitlab", "package": "org/group/sub/project", "ref": "main", "host": "gitlab.example.com" },
                    { "from": "zip", "url": "https://example.com/skills.zip", "sha256": "abc" }
                ],
                "audit": { "mode": "off", "pipeline": [ { "use": "static", "on-fail": "warn" } ] },
                "path-from-root": "packages/app"
            }"#,
        )
        .unwrap();
        assert_eq!(m.local_dirs(), ["./skills-src"]);
        assert_eq!(m.remote.as_ref().unwrap().len(), 3);
    }

    #[test]
    fn unknown_top_level_key_rejected() {
        let e = err(r#"{ "unknown-key": 1 }"#);
        assert!(e.starts_with("skills.json:"), "{e}");
        assert!(e.contains("unknown field"), "{e}");
    }

    #[test]
    fn empty_target_rejected() {
        assert!(err(r#"{ "target": "" }"#).contains("invalid target"));
        assert!(err(r#"{ "target": "  " }"#).contains("invalid target"));
    }

    #[test]
    fn non_string_target_rejected() {
        assert!(err(r#"{ "target": 5 }"#).starts_with("skills.json:"));
        assert!(err(r#"{ "target": ["x"] }"#).starts_with("skills.json:"));
    }

    #[test]
    fn absolute_or_escaping_target_rejected() {
        assert!(err(r#"{ "target": "/abs" }"#).contains("invalid target"));
        assert!(err(r#"{ "target": "C:\\abs" }"#).contains("invalid target"));
        assert!(err(r#"{ "target": "../out" }"#).contains("invalid target"));
    }

    #[test]
    fn non_bool_flags_rejected() {
        assert!(err(r#"{ "discovery": "yes" }"#).starts_with("skills.json:"));
        assert!(err(r#"{ "trusted-replace": 1 }"#).starts_with("skills.json:"));
    }

    #[test]
    fn scalar_aliases_rejected() {
        assert!(err(r#"{ "aliases": ".claude/skills" }"#).starts_with("skills.json:"));
    }

    #[test]
    fn empty_alias_entry_rejected() {
        assert!(err(r#"{ "aliases": [""] }"#).contains("invalid alias"));
    }

    #[test]
    fn alias_equal_to_target_rejected() {
        let e = err(r#"{ "target": ".agents/skills", "aliases": ["./.agents/skills"] }"#);
        assert!(e.contains("must not equal the target"), "{e}");
    }

    #[test]
    fn duplicate_aliases_rejected() {
        let e = err(r#"{ "aliases": [".claude/skills", "./.claude/skills"] }"#);
        assert!(e.contains("duplicate alias"), "{e}");
    }

    #[test]
    fn alias_escaping_root_rejected() {
        assert!(err(r#"{ "aliases": ["../outside"] }"#).contains("invalid alias"));
    }

    #[test]
    fn bare_vendor_trusted_pattern_rejected() {
        assert!(err(r#"{ "trusted": ["acme"] }"#).contains("invalid trusted pattern"));
    }

    #[test]
    fn multi_slash_trusted_pattern_rejected() {
        assert!(err(r#"{ "trusted": ["a/b/c"] }"#).contains("invalid trusted pattern"));
    }

    #[test]
    fn star_only_trusted_pattern_rejected() {
        assert!(err(r#"{ "trusted": ["*"] }"#).contains("invalid trusted pattern"));
    }

    #[test]
    fn valid_trusted_patterns_accepted() {
        Manifest::parse(r#"{ "trusted": ["acme/skills", "acme/*"] }"#).unwrap();
    }

    #[test]
    fn empty_local_dir_entry_rejected() {
        assert!(err(r#"{ "local": { "dir": [""] } }"#).contains("local.dir"));
    }

    #[test]
    fn duplicate_local_dir_rejected() {
        let e = err(r#"{ "local": { "dir": ["./a", ".\\a"] } }"#);
        assert!(e.contains("duplicate local.dir"), "{e}");
    }

    #[test]
    fn unknown_local_key_rejected() {
        assert!(err(r#"{ "local": { "cargo": true } }"#).starts_with("skills.json:"));
    }

    #[test]
    fn remote_requires_exactly_one_of_package_url() {
        let both =
            err(r#"{ "remote": [ { "from": "github", "package": "a/b", "url": "https://x" } ] }"#);
        assert!(both.contains("got both"), "{both}");
        let neither = err(r#"{ "remote": [ { "from": "github" } ] }"#);
        assert!(neither.contains("got neither"), "{neither}");
    }

    #[test]
    fn remote_unknown_from_rejected() {
        assert!(
            err(r#"{ "remote": [ { "from": "svn", "url": "x" } ] }"#).contains("unknown source")
        );
    }

    #[test]
    fn remote_by_url_forbids_package_fields() {
        let e = err(r#"{ "remote": [ { "from": "zip", "url": "https://x", "host": "h" } ] }"#);
        assert!(e.contains("'host' is not allowed"), "{e}");
        let e = err(r#"{ "remote": [ { "from": "zip", "url": "https://x", "ref": "v1" } ] }"#);
        assert!(e.contains("'ref' is not allowed"), "{e}");
    }

    #[test]
    fn remote_by_package_forbids_sha256() {
        let e = err(r#"{ "remote": [ { "from": "github", "package": "a/b", "sha256": "ff" } ] }"#);
        assert!(e.contains("'sha256' is not allowed"), "{e}");
    }

    #[test]
    fn remote_by_url_requires_url_not_package() {
        let e = err(r#"{ "remote": [ { "from": "zip", "package": "a/b" } ] }"#);
        assert!(e.contains("requires 'url'"), "{e}");
    }

    #[test]
    fn remote_duplicate_rejected() {
        let e = err(r#"{ "remote": [
                { "from": "github", "package": "a/b" },
                { "from": "github", "package": "a/b", "ref": "v2" }
            ] }"#);
        assert!(e.contains("duplicate entry"), "{e}");
    }

    #[test]
    fn remote_same_package_different_host_ok() {
        Manifest::parse(
            r#"{ "remote": [
                { "from": "gitlab", "package": "a/b" },
                { "from": "gitlab", "package": "a/b", "host": "gitlab.example.com" }
            ] }"#,
        )
        .unwrap();
    }

    #[test]
    fn remote_skills_tristate() {
        let m = Manifest::parse(
            r#"{ "remote": [
                { "from": "github", "package": "a/all" },
                { "from": "github", "package": "a/null", "skills": null },
                { "from": "github", "package": "a/none", "skills": [] },
                { "from": "github", "package": "a/some", "skills": ["x"] }
            ] }"#,
        )
        .unwrap();
        let remotes = m.remote.unwrap();
        assert_eq!(remotes[0].skills, None);
        assert_eq!(remotes[1].skills, None);
        assert_eq!(remotes[2].skills, Some(vec![]));
        assert_eq!(remotes[3].skills, Some(vec!["x".to_string()]));
    }

    #[test]
    fn remote_unknown_key_rejected() {
        let e = err(r#"{ "remote": [ { "from": "github", "package": "a/b", "branch": "x" } ] }"#);
        assert!(e.starts_with("skills.json:"), "{e}");
    }

    #[test]
    fn path_from_root_shape_validated() {
        Manifest::parse(r#"{ "path-from-root": "packages/app" }"#).unwrap();
        assert!(err(r#"{ "path-from-root": "../x" }"#).contains("path-from-root"));
        assert!(err(r#"{ "path-from-root": "./x" }"#).contains("path-from-root"));
        assert!(err(r#"{ "path-from-root": "/abs" }"#).contains("path-from-root"));
    }

    #[test]
    fn audit_config_parses() {
        let m = Manifest::parse(r#"{ "audit": { "mode": "block" } }"#).unwrap();
        assert_eq!(m.audit_mode(), AuditMode::Block);
        assert!(err(r#"{ "audit": { "mode": "loud" } }"#).starts_with("skills.json:"));
    }

    #[test]
    fn audit_pipeline_variants_parse() {
        let m = Manifest::parse(
            r#"{ "audit": { "mode": "warn", "pipeline": [
                { "use": "static", "on-fail": "warn" },
                { "use": "llm", "model": "gpt-x" },
                { "use": "http", "url": "https://audit.example", "on-fail": "block" }
            ] } }"#,
        )
        .unwrap();
        let steps = m.audit_steps();
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].id(), "static");
        assert_eq!(steps[0].on_fail(), Some(OnFail::Warn));
        assert_eq!(steps[1].id(), "llm");
        assert_eq!(steps[1].on_fail(), None);
        let AuditStep::Llm(llm) = &steps[1] else {
            panic!("expected llm step");
        };
        assert_eq!(llm.model.as_deref(), Some("gpt-x"));
        assert_eq!(steps[2].id(), "http");
        assert_eq!(steps[2].on_fail(), Some(OnFail::Block));
    }

    #[test]
    fn audit_pipeline_defaults_to_static() {
        // Absent pipeline (or absent audit section) = the default chain.
        let steps = Manifest::parse("{}").unwrap().audit_steps();
        assert_eq!(steps, vec![AuditStep::Static(StaticStep::default())]);
        let steps = Manifest::parse(r#"{ "audit": { "mode": "warn" } }"#)
            .unwrap()
            .audit_steps();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].id(), "static");

        // Explicit [] = no auditors.
        let steps = Manifest::parse(r#"{ "audit": { "pipeline": [] } }"#)
            .unwrap()
            .audit_steps();
        assert!(steps.is_empty());
    }

    #[test]
    fn audit_pipeline_unknown_auditor_rejected() {
        let e = err(r#"{ "audit": { "pipeline": [ { "use": "voodoo" } ] } }"#);
        assert!(e.contains("unknown auditor 'voodoo'"), "{e}");
        assert!(e.contains("static, llm, http"), "{e}");
    }

    #[test]
    fn audit_pipeline_entry_requires_use() {
        let e = err(r#"{ "audit": { "pipeline": [ { "on-fail": "warn" } ] } }"#);
        assert!(e.contains("missing field `use`"), "{e}");
        let e = err(r#"{ "audit": { "pipeline": [ { "use": 5 } ] } }"#);
        assert!(e.contains("`use` must be a string"), "{e}");
    }

    #[test]
    fn audit_pipeline_unknown_entry_field_rejected() {
        let e = err(r#"{ "audit": { "pipeline": [ { "use": "static", "level": 9 } ] } }"#);
        assert!(e.contains("unknown field"), "{e}");
        // Options are per-variant: `model` belongs to llm, not http.
        let e = err(r#"{ "audit": { "pipeline": [ { "use": "http", "model": "x" } ] } }"#);
        assert!(e.contains("unknown field"), "{e}");
    }

    #[test]
    fn audit_pipeline_bad_on_fail_rejected() {
        let e = err(r#"{ "audit": { "pipeline": [ { "use": "static", "on-fail": "off" } ] } }"#);
        assert!(e.starts_with("skills.json:"), "{e}");
    }

    #[test]
    fn malformed_json_fails_with_clear_prefix() {
        let e = err("{ not json");
        assert!(e.starts_with("skills.json:"), "{e}");
    }

    #[test]
    fn load_missing_file_mentions_init() {
        let e = Manifest::load(Path::new("Z:/nope/skills.json"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("skills init"), "{e}");
    }
}
