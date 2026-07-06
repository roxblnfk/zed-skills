//! Audit result types, shared by all auditor implementations.

use std::fmt;

use sha2::{Digest, Sha256};

use crate::domain::ResolvedSkill;
use crate::lockfile::AuditCacheEntry;
use crate::manifest::AuditStep;

/// Identifier of an auditor implementation (e.g. `noop`, `static`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AuditorId(pub &'static str);

impl fmt::Display for AuditorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// Severity of a finding. Ordering matters: the worst severity wins when
/// aggregating a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Severity {
    #[default]
    Pass,
    Warn,
    Block,
}

impl Severity {
    /// Lockfile verdict-cache representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Pass => "pass",
            Severity::Warn => "warn",
            Severity::Block => "block",
        }
    }

    /// Parse a cached verdict; unknown strings are a cache miss (`None`).
    pub fn parse(s: &str) -> Option<Severity> {
        match s {
            "pass" => Some(Severity::Pass),
            "warn" => Some(Severity::Warn),
            "block" => Some(Severity::Block),
            _ => None,
        }
    }
}

/// A single auditor observation, as returned by an [`crate::traits::Auditor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub severity: Severity,
    pub message: String,
    pub location: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditReport {
    pub findings: Vec<Finding>,
}

impl AuditReport {
    /// Worst severity across findings; an empty report passes.
    pub fn worst(&self) -> Severity {
        self.findings
            .iter()
            .map(|f| f.severity)
            .max()
            .unwrap_or(Severity::Pass)
    }
}

/// A finding annotated by the Audit stage: which auditor produced it, with
/// the *effective* severity (after the per-entry `on-fail` cap).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditFinding {
    pub auditor: AuditorId,
    pub severity: Severity,
    pub message: String,
    pub location: Option<String>,
}

/// Output of the Audit stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditedSkill {
    pub skill: ResolvedSkill,
    /// Worst effective severity across the chain, independent of the audit
    /// mode (the mode decides what to do with it).
    pub verdict: Severity,
    pub findings: Vec<AuditFinding>,
    /// The verdict was reused from the lockfile cache (no auditor ran).
    pub cached: bool,
    /// Verdict-cache entry to store in the lockfile. `None` when the audit
    /// stage did not run (mode off) — existing cached verdicts then stay
    /// untouched.
    pub cache_entry: Option<AuditCacheEntry>,
}

impl AuditedSkill {
    /// An audited skill for runs where the audit stage is off.
    pub fn unaudited(skill: ResolvedSkill) -> Self {
        AuditedSkill {
            skill,
            verdict: Severity::Pass,
            findings: Vec::new(),
            cached: false,
            cache_entry: None,
        }
    }
}

/// SHA-256 over the canonical serialization of the audit pipeline config
/// (auditor ids + options + on-fail, order-sensitive). Together with the
/// skill content hash this keys the lockfile verdict cache.
pub fn auditor_set_hash(steps: &[AuditStep]) -> String {
    let canonical: Vec<serde_json::Value> = steps.iter().map(AuditStep::canonical).collect();
    let json = serde_json::Value::Array(canonical).to_string();
    let mut hasher = Sha256::new();
    hasher.update(json.as_bytes());
    crate::fsutil::hex(&hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{HttpStep, LlmStep, OnFail, StaticStep};

    #[test]
    fn empty_report_passes() {
        assert_eq!(AuditReport::default().worst(), Severity::Pass);
    }

    #[test]
    fn worst_severity_wins() {
        let report = AuditReport {
            findings: vec![
                Finding {
                    severity: Severity::Warn,
                    message: "w".into(),
                    location: None,
                },
                Finding {
                    severity: Severity::Block,
                    message: "b".into(),
                    location: None,
                },
                Finding {
                    severity: Severity::Pass,
                    message: "p".into(),
                    location: None,
                },
            ],
        };
        assert_eq!(report.worst(), Severity::Block);
    }

    #[test]
    fn severity_string_roundtrip() {
        for sev in [Severity::Pass, Severity::Warn, Severity::Block] {
            assert_eq!(Severity::parse(sev.as_str()), Some(sev));
        }
        assert_eq!(Severity::parse("loud"), None);
    }

    fn static_step(on_fail: Option<OnFail>) -> AuditStep {
        AuditStep::Static(StaticStep { on_fail })
    }

    #[test]
    fn auditor_set_hash_is_stable() {
        let a = auditor_set_hash(&[static_step(None)]);
        let b = auditor_set_hash(&[static_step(None)]);
        assert_eq!(a, b);
        // 64 hex chars.
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn auditor_set_hash_is_order_sensitive() {
        let s = static_step(None);
        let l = AuditStep::Llm(LlmStep::default());
        let forward = auditor_set_hash(&[s.clone(), l.clone()]);
        let backward = auditor_set_hash(&[l, s]);
        assert_ne!(forward, backward);
    }

    #[test]
    fn auditor_set_hash_covers_options_and_on_fail() {
        let base = auditor_set_hash(&[static_step(None)]);
        let capped = auditor_set_hash(&[static_step(Some(OnFail::Warn))]);
        assert_ne!(base, capped);

        let http = auditor_set_hash(&[AuditStep::Http(HttpStep::default())]);
        let http_url = auditor_set_hash(&[AuditStep::Http(HttpStep {
            url: Some("https://a".into()),
            on_fail: None,
        })]);
        assert_ne!(http, http_url);
    }

    #[test]
    fn absent_pipeline_hashes_like_explicit_default_static() {
        // `audit_steps()` already normalizes: both yield [Static(default)].
        let m1 = crate::manifest::Manifest::parse("{}").unwrap();
        let m2 = crate::manifest::Manifest::parse(
            r#"{ "audit": { "pipeline": [ { "use": "static" } ] } }"#,
        )
        .unwrap();
        assert_eq!(
            auditor_set_hash(&m1.audit_steps()),
            auditor_set_hash(&m2.audit_steps())
        );
    }
}
