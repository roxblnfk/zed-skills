//! `skills.json` manifest (schema v2): serde model + validation.
//!
//! Strict at the top level: unknown keys are fatal (`deny_unknown_fields`).
//! Donor sources live in `sources[]` (by-package `github`/`gitlab`, by-url
//! `http`/`zip`, path-based `dir`); the legacy `remote` key is read as a
//! deprecated alias and may not be set together with `sources`. The
//! published JSON Schema (`resources/skills.schema.json`) mirrors this
//! model.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::ManifestError;
use crate::lockfile::LOCKFILE_NAME;
use crate::paths::normalize_rel;

pub const MANIFEST_NAME: &str = "skills.json";
pub const DEFAULT_TARGET: &str = ".agents/skills";

/// Canonical published URL of the manifest JSON Schema. Written as the
/// `$schema` value into manifests created by `skills init` / `skills add`
/// and pinned as the schema file's `$id` (see the schema-sync tests).
pub const SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/roxblnfk/zed-skills/master/resources/skills.schema.json";

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Manifest {
    #[serde(rename = "$schema")]
    pub schema: Option<String>,
    pub target: Option<String>,
    pub aliases: Option<Vec<String>>,
    /// Lockfile location, relative to the project root
    /// (default: `skills.lock`).
    pub lock_file: Option<String>,
    pub trusted: Option<Vec<String>>,
    pub trusted_replace: Option<bool>,
    pub discovery: Option<bool>,
    pub local: Option<LocalConfig>,
    /// Explicit donor sources. Each entry is one donor: a by-package
    /// (`github`/`gitlab`), by-url (`http`/`zip`), or path-based (`dir`)
    /// source.
    pub sources: Option<Vec<SourceEntry>>,
    /// Deprecated: renamed to [`Manifest::sources`]. Still read as an alias
    /// for back-compat, but may not be set together with `sources`.
    pub remote: Option<Vec<SourceEntry>>,
    pub audit: Option<AuditConfig>,
    /// Monorepo re-anchor. Validated for shape; semantics land in M5.
    pub path_from_root: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct LocalConfig {
    pub composer: Option<bool>,
    /// Reserved for future providers.
    pub npm: Option<bool>,
    /// Reserved for future providers.
    pub go: Option<bool>,
}

/// One `sources[]` entry. A tagged union over `from`:
/// by-package (`github`/`gitlab`, requires `package`), by-url
/// (`http`/`zip`, requires `url`), or path-based (`dir`, requires `path`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct SourceEntry {
    pub from: String,
    pub package: Option<String>,
    pub url: Option<String>,
    pub host: Option<String>,
    #[serde(rename = "ref")]
    pub r#ref: Option<String>,
    pub sha256: Option<String>,
    /// Filesystem path of a `dir` source donor, relative to the project
    /// root and confined to it (absolute paths and root-escaping `..` are
    /// rejected). Only valid with `from: "dir"`.
    pub path: Option<String>,
    /// Tri-state: absent/null = all skills; `[]` = donor registered, pulls
    /// nothing; non-empty = allowlist matched by canonical name.
    pub skills: Option<Vec<String>>,
}

const BY_PACKAGE_SOURCES: &[&str] = &["github", "gitlab"];
const BY_URL_SOURCES: &[&str] = &["http", "zip"];
const BY_PATH_SOURCES: &[&str] = &["dir"];

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

/// One segment of a JSON path into `skills.json` (e.g. `sources[1].skills[0]`
/// is `[Key("sources"), Index(1), Key("skills"), Index(0)]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSeg {
    Key(String),
    Index(usize),
}

impl PathSeg {
    pub fn key(k: &str) -> PathSeg {
        PathSeg::Key(k.to_string())
    }
}

/// A semantic validation problem, anchored to the offending field so
/// span-aware frontends (the LSP server) can point at it. `path` is empty
/// for document-level problems.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestIssue {
    pub path: Vec<PathSeg>,
    pub message: String,
}

impl ManifestIssue {
    fn new(path: Vec<PathSeg>, message: impl Into<String>) -> Self {
        ManifestIssue {
            path,
            message: message.into(),
        }
    }
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

    /// Effective lockfile path (normalized, `/`-separated, relative to the
    /// project root; default: `skills.lock`).
    pub fn effective_lock_file(&self) -> String {
        match &self.lock_file {
            Some(l) => normalize_rel(l).unwrap_or_else(|_| LOCKFILE_NAME.to_string()),
            None => LOCKFILE_NAME.to_string(),
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

    /// Effective donor sources: `sources` if set, else the deprecated
    /// `remote` alias, else an empty slice.
    pub fn sources(&self) -> &[SourceEntry] {
        self.sources
            .as_deref()
            .or(self.remote.as_deref())
            .unwrap_or(&[])
    }

    /// The JSON key the effective sources were read from: `"remote"` when
    /// only the deprecated alias is set, `"sources"` otherwise. Used to
    /// anchor diagnostics at the key the user actually wrote.
    pub fn sources_key(&self) -> &'static str {
        if self.sources.is_none() && self.remote.is_some() {
            "remote"
        } else {
            "sources"
        }
    }

    /// Whether the manifest relies on the deprecated `remote` alias
    /// (`remote` set and `sources` absent).
    pub fn uses_deprecated_remote(&self) -> bool {
        self.sources.is_none() && self.remote.is_some()
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
        match self.validate_issues().into_iter().next() {
            Some(issue) => Err(ManifestError::Invalid(issue.message)),
            None => Ok(()),
        }
    }

    /// All semantic validation problems, each anchored to the offending
    /// field. [`Manifest::parse`] fails on the first one; span-aware
    /// frontends surface them all.
    pub fn validate_issues(&self) -> Vec<ManifestIssue> {
        let mut issues = Vec::new();

        // target
        let target_norm = match &self.target {
            Some(t) => match normalize_rel(t) {
                Ok(norm) => norm,
                Err(e) => {
                    issues.push(ManifestIssue::new(
                        vec![PathSeg::key("target")],
                        format!("invalid target: {e}"),
                    ));
                    DEFAULT_TARGET.to_string()
                }
            },
            None => DEFAULT_TARGET.to_string(),
        };

        // aliases
        let mut alias_norms = HashSet::new();
        if let Some(aliases) = &self.aliases {
            for (idx, alias) in aliases.iter().enumerate() {
                let at = || vec![PathSeg::key("aliases"), PathSeg::Index(idx)];
                match normalize_rel(alias) {
                    Err(e) => issues.push(ManifestIssue::new(at(), format!("invalid alias: {e}"))),
                    Ok(norm) if norm == target_norm => issues.push(ManifestIssue::new(
                        at(),
                        format!("alias '{alias}' must not equal the target '{target_norm}'"),
                    )),
                    Ok(norm) => {
                        if !alias_norms.insert(norm.clone()) {
                            issues.push(ManifestIssue::new(
                                at(),
                                format!("duplicate alias '{norm}'"),
                            ));
                        }
                    }
                }
            }
        }

        // lock-file: relative, inside the project root, distinct from the
        // manifest itself, the target and every alias
        if let Some(lock) = &self.lock_file {
            let at = || vec![PathSeg::key("lock-file")];
            match normalize_rel(lock) {
                Err(e) => {
                    issues.push(ManifestIssue::new(at(), format!("invalid lock-file: {e}")));
                }
                Ok(norm) if norm == MANIFEST_NAME => issues.push(ManifestIssue::new(
                    at(),
                    format!("lock-file must not equal the manifest '{MANIFEST_NAME}'"),
                )),
                Ok(norm) if norm == target_norm => issues.push(ManifestIssue::new(
                    at(),
                    format!("lock-file '{norm}' must not equal the target '{target_norm}'"),
                )),
                Ok(norm) if alias_norms.contains(&norm) => issues.push(ManifestIssue::new(
                    at(),
                    format!("lock-file '{norm}' must not equal an alias"),
                )),
                Ok(_) => {}
            }
        }

        // trusted patterns: vendor/package or vendor/* (exactly one slash,
        // both sides non-empty)
        if let Some(trusted) = &self.trusted {
            for (idx, pattern) in trusted.iter().enumerate() {
                if crate::pattern::VendorPattern::parse(pattern).is_err() {
                    issues.push(ManifestIssue::new(
                        vec![PathSeg::key("trusted"), PathSeg::Index(idx)],
                        format!(
                            "invalid trusted pattern '{pattern}': expected 'vendor/package' or 'vendor/*'"
                        ),
                    ));
                }
            }
        }

        // sources / deprecated remote alias
        if self.sources.is_some() && self.remote.is_some() {
            issues.push(ManifestIssue::new(
                vec![PathSeg::key("remote")],
                "'remote' was renamed to 'sources' and must not be set together with it",
            ));
        }
        {
            // Anchor issues at the key the user actually wrote.
            let key = self.sources_key();
            let mut seen = HashSet::new();
            for (idx, entry) in self.sources().iter().enumerate() {
                let at = || vec![PathSeg::key(key), PathSeg::Index(idx)];
                match validate_source_entry(entry) {
                    Err(e) => issues.push(ManifestIssue::new(at(), format!("{key}[{idx}]: {e}"))),
                    Ok(uniq) => {
                        if !seen.insert(uniq.clone()) {
                            issues.push(ManifestIssue::new(
                                at(),
                                format!("{key}[{idx}]: duplicate entry '{uniq}'"),
                            ));
                        }
                    }
                }
            }
        }

        // path-from-root: relative, plain segments only
        if let Some(p) = &self.path_from_root {
            let at = || vec![PathSeg::key("path-from-root")];
            if p.trim().is_empty() {
                issues.push(ManifestIssue::new(at(), "path-from-root must not be empty"));
            } else {
                let plain = !crate::paths::is_absolute_like(p)
                    && p.split(['/', '\\'])
                        .all(|seg| !seg.is_empty() && seg != "." && seg != "..");
                if !plain {
                    issues.push(ManifestIssue::new(
                        at(),
                        format!("path-from-root must be relative with plain segments, got '{p}'"),
                    ));
                }
            }
        }

        issues
    }
}

/// Validate one source entry; returns its uniqueness key
/// (`from|host|identifier`; `host` is always `default` for `dir`).
fn validate_source_entry(entry: &SourceEntry) -> Result<String, String> {
    let from = entry.from.as_str();
    let by_package = BY_PACKAGE_SOURCES.contains(&from);
    let by_url = BY_URL_SOURCES.contains(&from);
    let by_path = BY_PATH_SOURCES.contains(&from);
    if !by_package && !by_url && !by_path {
        return Err(format!(
            "unknown source '{from}' (expected one of: github, gitlab, http, zip, dir)"
        ));
    }
    if by_path {
        let Some(path) = &entry.path else {
            return Err(format!("source '{from}' requires 'path'"));
        };
        if entry.url.is_some() {
            return Err(format!("'url' is not allowed with source '{from}'"));
        }
        if entry.host.is_some() {
            return Err(format!("'host' is not allowed with source '{from}'"));
        }
        if entry.r#ref.is_some() {
            return Err(format!("'ref' is not allowed with source '{from}'"));
        }
        if entry.sha256.is_some() {
            return Err(format!("'sha256' is not allowed with source '{from}'"));
        }
        // Optional vendor-name override.
        if let Some(package) = &entry.package
            && package.trim().is_empty()
        {
            return Err("'package' must not be empty".to_string());
        }
        // The path must stay inside the project root: absolute paths
        // (incl. Windows drive letters) and escaping `..` are rejected.
        // The normalized form is the uniqueness identifier.
        let identifier = normalize_rel(path).map_err(|e| format!("invalid path: {e}"))?;
        return Ok(format!("{from}|default|{identifier}"));
    }
    if entry.path.is_some() {
        return Err("'path' is only allowed with source 'dir'".to_string());
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
        assert!(m.sources().is_empty());
        assert!(!m.uses_deprecated_remote());
        assert_eq!(m.sources_key(), "sources");
    }

    #[test]
    fn fully_populated_accepted() {
        let m = Manifest::parse(
            r#"{
                "$schema": "https://example.com/skills.schema.json",
                "target": ".agents/skills",
                "aliases": [".claude/skills", ".cursor/skills"],
                "lock-file": ".agents/skills.lock",
                "trusted": ["acme/*", "acme/skills"],
                "trusted-replace": false,
                "discovery": false,
                "local": { "composer": true, "npm": false, "go": false },
                "sources": [
                    { "from": "github", "package": "acme/skills", "ref": "v1.2.0", "skills": ["code-review"] },
                    { "from": "gitlab", "package": "org/group/sub/project", "ref": "main", "host": "gitlab.example.com" },
                    { "from": "zip", "url": "https://example.com/skills.zip", "sha256": "abc" },
                    { "from": "dir", "path": "./skills-src", "package": "acme/local-skills" }
                ],
                "audit": { "mode": "off", "pipeline": [ { "use": "static", "on-fail": "warn" } ] },
                "path-from-root": "packages/app"
            }"#,
        )
        .unwrap();
        assert_eq!(m.sources().len(), 4);
        assert_eq!(m.sources()[3].path.as_deref(), Some("./skills-src"));
        assert!(!m.uses_deprecated_remote());
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
    fn lock_file_defaults_when_absent() {
        let m = Manifest::parse("{}").unwrap();
        assert_eq!(m.effective_lock_file(), "skills.lock");
    }

    #[test]
    fn custom_lock_file_normalized() {
        let m = Manifest::parse(r#"{ "lock-file": "./.agents\\skills.lock" }"#).unwrap();
        assert_eq!(m.effective_lock_file(), ".agents/skills.lock");
    }

    #[test]
    fn empty_lock_file_rejected() {
        assert!(err(r#"{ "lock-file": "" }"#).contains("invalid lock-file"));
        assert!(err(r#"{ "lock-file": "  " }"#).contains("invalid lock-file"));
    }

    #[test]
    fn absolute_lock_file_rejected() {
        assert!(err(r#"{ "lock-file": "/abs/skills.lock" }"#).contains("invalid lock-file"));
        assert!(err(r#"{ "lock-file": "C:\\skills.lock" }"#).contains("invalid lock-file"));
    }

    #[test]
    fn escaping_lock_file_rejected() {
        assert!(err(r#"{ "lock-file": "../skills.lock" }"#).contains("invalid lock-file"));
        assert!(err(r#"{ "lock-file": "a/../../skills.lock" }"#).contains("invalid lock-file"));
    }

    #[test]
    fn lock_file_equal_to_manifest_rejected() {
        let e = err(r#"{ "lock-file": "./skills.json" }"#);
        assert!(e.contains("must not equal the manifest"), "{e}");
    }

    #[test]
    fn lock_file_equal_to_target_rejected() {
        let e = err(r#"{ "target": ".agents/skills", "lock-file": "./.agents/skills" }"#);
        assert!(e.contains("must not equal the target"), "{e}");
        // Also against the default target when `target` is absent.
        let e = err(r#"{ "lock-file": ".agents/skills" }"#);
        assert!(e.contains("must not equal the target"), "{e}");
    }

    #[test]
    fn lock_file_equal_to_alias_rejected() {
        let e = err(r#"{ "aliases": [".claude/skills"], "lock-file": "./.claude/skills" }"#);
        assert!(e.contains("must not equal an alias"), "{e}");
    }

    #[test]
    fn valid_lock_file_accepted() {
        Manifest::parse(r#"{ "lock-file": ".agents/skills.lock" }"#).unwrap();
        Manifest::parse(r#"{ "lock-file": "skills.lock" }"#).unwrap();
    }

    #[test]
    fn validate_issues_anchors_lock_file_field() {
        let manifest: Manifest = serde_json::from_str(r#"{ "lock-file": "../out.lock" }"#).unwrap();
        let issues = manifest.validate_issues();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].path, vec![PathSeg::key("lock-file")]);
        assert!(
            issues[0].message.contains("invalid lock-file"),
            "{}",
            issues[0].message
        );
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
    fn legacy_local_dir_key_rejected() {
        // `local.dir` was replaced by `sources[]` dir entries; old manifests
        // must fail parsing (deny_unknown_fields).
        let e = err(r#"{ "local": { "dir": ["./a"] } }"#);
        assert!(e.starts_with("skills.json:"), "{e}");
        assert!(e.contains("unknown field"), "{e}");
    }

    #[test]
    fn unknown_local_key_rejected() {
        assert!(err(r#"{ "local": { "cargo": true } }"#).starts_with("skills.json:"));
    }

    #[test]
    fn sources_requires_exactly_one_of_package_url() {
        let both =
            err(r#"{ "sources": [ { "from": "github", "package": "a/b", "url": "https://x" } ] }"#);
        assert!(both.contains("got both"), "{both}");
        let neither = err(r#"{ "sources": [ { "from": "github" } ] }"#);
        assert!(neither.contains("got neither"), "{neither}");
    }

    #[test]
    fn sources_unknown_from_rejected() {
        let e = err(r#"{ "sources": [ { "from": "svn", "url": "x" } ] }"#);
        assert!(e.contains("unknown source"), "{e}");
        assert!(e.contains("github, gitlab, http, zip, dir"), "{e}");
    }

    #[test]
    fn sources_by_url_forbids_package_fields() {
        let e = err(r#"{ "sources": [ { "from": "zip", "url": "https://x", "host": "h" } ] }"#);
        assert!(e.contains("'host' is not allowed"), "{e}");
        let e = err(r#"{ "sources": [ { "from": "zip", "url": "https://x", "ref": "v1" } ] }"#);
        assert!(e.contains("'ref' is not allowed"), "{e}");
    }

    #[test]
    fn sources_by_package_forbids_sha256() {
        let e = err(r#"{ "sources": [ { "from": "github", "package": "a/b", "sha256": "ff" } ] }"#);
        assert!(e.contains("'sha256' is not allowed"), "{e}");
    }

    #[test]
    fn sources_by_url_requires_url_not_package() {
        let e = err(r#"{ "sources": [ { "from": "zip", "package": "a/b" } ] }"#);
        assert!(e.contains("requires 'url'"), "{e}");
    }

    #[test]
    fn sources_duplicate_rejected() {
        let e = err(r#"{ "sources": [
                { "from": "github", "package": "a/b" },
                { "from": "github", "package": "a/b", "ref": "v2" }
            ] }"#);
        assert!(e.contains("duplicate entry"), "{e}");
        assert!(e.contains("sources[1]"), "{e}");
    }

    #[test]
    fn sources_same_package_different_host_ok() {
        Manifest::parse(
            r#"{ "sources": [
                { "from": "gitlab", "package": "a/b" },
                { "from": "gitlab", "package": "a/b", "host": "gitlab.example.com" }
            ] }"#,
        )
        .unwrap();
    }

    #[test]
    fn sources_skills_tristate() {
        let m = Manifest::parse(
            r#"{ "sources": [
                { "from": "github", "package": "a/all" },
                { "from": "github", "package": "a/null", "skills": null },
                { "from": "github", "package": "a/none", "skills": [] },
                { "from": "github", "package": "a/some", "skills": ["x"] }
            ] }"#,
        )
        .unwrap();
        let sources = m.sources();
        assert_eq!(sources[0].skills, None);
        assert_eq!(sources[1].skills, None);
        assert_eq!(sources[2].skills, Some(vec![]));
        assert_eq!(sources[3].skills, Some(vec!["x".to_string()]));
    }

    #[test]
    fn sources_unknown_key_rejected() {
        let e = err(r#"{ "sources": [ { "from": "github", "package": "a/b", "branch": "x" } ] }"#);
        assert!(e.starts_with("skills.json:"), "{e}");
    }

    #[test]
    fn dir_source_accepted_with_package_override() {
        let m = Manifest::parse(
            r#"{ "sources": [ { "from": "dir", "path": "./skills-src", "package": "acme/local" } ] }"#,
        )
        .unwrap();
        assert_eq!(m.sources()[0].path.as_deref(), Some("./skills-src"));
        assert_eq!(m.sources()[0].package.as_deref(), Some("acme/local"));
    }

    #[test]
    fn dir_source_requires_path() {
        let e = err(r#"{ "sources": [ { "from": "dir" } ] }"#);
        assert!(e.contains("requires 'path'"), "{e}");
    }

    #[test]
    fn dir_source_empty_path_rejected() {
        let e = err(r#"{ "sources": [ { "from": "dir", "path": "" } ] }"#);
        assert!(e.contains("invalid path"), "{e}");
        let e = err(r#"{ "sources": [ { "from": "dir", "path": "  " } ] }"#);
        assert!(e.contains("invalid path"), "{e}");
    }

    #[test]
    fn dir_source_absolute_or_escaping_path_rejected() {
        // Root confinement: a dir source may not leave the project root.
        let e = err(r#"{ "sources": [ { "from": "dir", "path": "/abs" } ] }"#);
        assert!(e.contains("invalid path"), "{e}");
        let e = err(r#"{ "sources": [ { "from": "dir", "path": "C:\\abs" } ] }"#);
        assert!(e.contains("invalid path"), "{e}");
        let e = err(r#"{ "sources": [ { "from": "dir", "path": "../x" } ] }"#);
        assert!(e.contains("invalid path"), "{e}");
    }

    #[test]
    fn dir_source_duplicate_path_rejected() {
        // `./a`, `a\` and `a` normalize to the same donor.
        let e = err(r#"{ "sources": [
                { "from": "dir", "path": "./a" },
                { "from": "dir", "path": "a\\" }
            ] }"#);
        assert!(e.contains("duplicate entry"), "{e}");
        let e = err(r#"{ "sources": [
                { "from": "dir", "path": "./a" },
                { "from": "dir", "path": "a" }
            ] }"#);
        assert!(e.contains("duplicate entry"), "{e}");
    }

    #[test]
    fn dir_source_empty_package_override_rejected() {
        let e = err(r#"{ "sources": [ { "from": "dir", "path": "./a", "package": " " } ] }"#);
        assert!(e.contains("'package' must not be empty"), "{e}");
    }

    #[test]
    fn dir_source_forbids_url_fields() {
        let e = err(r#"{ "sources": [ { "from": "dir", "path": "./a", "url": "https://x" } ] }"#);
        assert!(e.contains("'url' is not allowed with source 'dir'"), "{e}");
        let e = err(r#"{ "sources": [ { "from": "dir", "path": "./a", "host": "h" } ] }"#);
        assert!(e.contains("'host' is not allowed"), "{e}");
        let e = err(r#"{ "sources": [ { "from": "dir", "path": "./a", "ref": "v1" } ] }"#);
        assert!(e.contains("'ref' is not allowed"), "{e}");
        let e = err(r#"{ "sources": [ { "from": "dir", "path": "./a", "sha256": "ff" } ] }"#);
        assert!(e.contains("'sha256' is not allowed"), "{e}");
    }

    #[test]
    fn path_forbidden_on_non_dir_sources() {
        let e = err(r#"{ "sources": [ { "from": "github", "package": "a/b", "path": "./a" } ] }"#);
        assert!(
            e.contains("'path' is only allowed with source 'dir'"),
            "{e}"
        );
    }

    #[test]
    fn deprecated_remote_alias_still_read() {
        let m =
            Manifest::parse(r#"{ "remote": [ { "from": "github", "package": "a/b" } ] }"#).unwrap();
        assert!(m.uses_deprecated_remote());
        assert_eq!(m.sources_key(), "remote");
        assert_eq!(m.sources().len(), 1);
        assert_eq!(m.sources()[0].package.as_deref(), Some("a/b"));
    }

    #[test]
    fn remote_alias_entries_validated_and_anchored_at_remote() {
        // Alias entries run through the same validation, anchored at the
        // key the user wrote.
        let e = err(r#"{ "remote": [ { "from": "svn", "url": "x" } ] }"#);
        assert!(e.contains("remote[0]"), "{e}");
        assert!(e.contains("unknown source"), "{e}");
        let manifest: Manifest =
            serde_json::from_str(r#"{ "remote": [ { "from": "github" } ] }"#).unwrap();
        let issues = manifest.validate_issues();
        assert_eq!(issues.len(), 1);
        assert_eq!(
            issues[0].path,
            vec![PathSeg::key("remote"), PathSeg::Index(0)]
        );
    }

    #[test]
    fn sources_and_remote_together_rejected() {
        let e = err(r#"{
                "sources": [ { "from": "github", "package": "a/b" } ],
                "remote": [ { "from": "github", "package": "c/d" } ]
            }"#);
        assert!(
            e.contains("'remote' was renamed to 'sources' and must not be set together with it"),
            "{e}"
        );
        // Anchored at the deprecated key; only the effective list is
        // validated (sources wins).
        let manifest: Manifest = serde_json::from_str(
            r#"{
                "sources": [ { "from": "github", "package": "a/b" } ],
                "remote": [ { "from": "svn", "url": "x" } ]
            }"#,
        )
        .unwrap();
        let issues = manifest.validate_issues();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].path, vec![PathSeg::key("remote")]);
        assert_eq!(manifest.sources_key(), "sources");
        assert!(!manifest.uses_deprecated_remote());
        assert_eq!(manifest.sources()[0].package.as_deref(), Some("a/b"));
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
    fn validate_issues_collects_all_with_paths() {
        let manifest: Manifest = serde_json::from_str(
            r#"{
                "target": "../out",
                "aliases": ["", ".claude/skills", "./.claude/skills"],
                "trusted": ["acme/*", "bare"],
                "sources": [
                    { "from": "github", "package": "a/b" },
                    { "from": "github", "package": "a/b" },
                    { "from": "svn", "url": "x" },
                    { "from": "dir", "path": "./a" },
                    { "from": "dir", "path": "a\\" }
                ],
                "path-from-root": "../x"
            }"#,
        )
        .unwrap();
        let issues = manifest.validate_issues();
        let paths: Vec<(String, &str)> = issues
            .iter()
            .map(|i| {
                let path = i
                    .path
                    .iter()
                    .map(|s| match s {
                        PathSeg::Key(k) => k.clone(),
                        PathSeg::Index(i) => i.to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(".");
                let kind = i.message.split([':', ' ']).next().unwrap_or("");
                (path, kind)
            })
            .collect();
        let got: Vec<(&str, &str)> = paths.iter().map(|(p, k)| (p.as_str(), *k)).collect();
        assert_eq!(
            got,
            [
                ("target", "invalid"),
                ("aliases.0", "invalid"),
                ("aliases.2", "duplicate"),
                ("trusted.1", "invalid"),
                ("sources.1", "sources[1]"),
                ("sources.2", "sources[2]"),
                ("sources.4", "sources[4]"),
                ("path-from-root", "path-from-root"),
            ]
        );
    }

    #[test]
    fn validate_issues_empty_for_valid_manifest() {
        let m = Manifest::parse(r#"{ "target": ".agents/skills" }"#).unwrap();
        assert!(m.validate_issues().is_empty());
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
