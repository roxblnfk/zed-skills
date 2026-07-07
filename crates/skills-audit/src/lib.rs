//! Auditor implementations and the audit-chain builder.
//!
//! `StaticAuditor` is the only real auditor (M4); `LlmAuditor` / `HttpAuditor`
//! are stubs behind the same `Auditor` trait — the manifest schema accepts
//! their config entries, but constructing them is a config error until they
//! ship.

pub mod staticaudit;
pub mod textcheck;

use std::sync::Arc;

use async_trait::async_trait;
use thiserror::Error;

use skills_core::audit::{AuditReport, AuditorId};
use skills_core::domain::ResolvedSkill;
use skills_core::error::AuditError;
use skills_core::manifest::{AuditMode, AuditStep, HttpStep, LlmStep, Manifest};
use skills_core::pipeline::ChainEntry;
use skills_core::traits::Auditor;

pub use staticaudit::StaticAuditor;
pub use textcheck::{
    TextCheck, danger_checks, dir_name_spec_error, name_format_error, skill_md_checks,
};

/// Building the audit chain from `skills.json` failed (a config error,
/// exit 1).
#[derive(Debug, Error)]
pub enum ChainError {
    #[error(
        "audit.pipeline: the '{id}' auditor is not implemented yet, coming in a future release"
    )]
    NotImplemented { id: &'static str },
}

/// Build the audit chain from the manifest's `audit` section, in configured
/// order. With `mode: off` the chain is not constructed at all — pre-staged
/// llm/http entries are then allowed to sit in the config.
pub fn build_chain(manifest: &Manifest) -> Result<Vec<ChainEntry>, ChainError> {
    if manifest.audit_mode() == AuditMode::Off {
        return Ok(Vec::new());
    }
    manifest
        .audit_steps()
        .iter()
        .map(|step| {
            let auditor: Arc<dyn Auditor> = match step {
                AuditStep::Static(_) => Arc::new(StaticAuditor),
                AuditStep::Llm(config) => Arc::new(LlmAuditor::new(config)?),
                AuditStep::Http(config) => Arc::new(HttpAuditor::new(config)?),
            };
            Ok(ChainEntry {
                auditor,
                on_fail: step.on_fail(),
            })
        })
        .collect()
}

/// Auditor that finds nothing: every skill passes. Useful as a test chain.
pub struct NoopAuditor;

#[async_trait]
impl Auditor for NoopAuditor {
    fn id(&self) -> AuditorId {
        AuditorId("noop")
    }

    async fn audit(&self, _skill: &ResolvedSkill) -> Result<AuditReport, AuditError> {
        Ok(AuditReport::default())
    }
}

/// A chain with a single no-op auditor (every skill passes).
pub fn noop_chain() -> Vec<ChainEntry> {
    vec![ChainEntry::new(Arc::new(NoopAuditor))]
}

/// LLM-backed auditor — not implemented yet. Construction always fails so a
/// config referencing it aborts with exit 1.
pub struct LlmAuditor {
    _private: (),
}

impl LlmAuditor {
    pub fn new(_config: &LlmStep) -> Result<Self, ChainError> {
        Err(ChainError::NotImplemented { id: "llm" })
    }
}

#[async_trait]
impl Auditor for LlmAuditor {
    fn id(&self) -> AuditorId {
        AuditorId("llm")
    }

    // Unreachable: `new()` never yields an instance.
    async fn audit(&self, _skill: &ResolvedSkill) -> Result<AuditReport, AuditError> {
        Err(AuditError::Auditor {
            auditor: "llm".to_string(),
            message: "not implemented".to_string(),
        })
    }
}

/// HTTP-service auditor — not implemented yet. Construction always fails so
/// a config referencing it aborts with exit 1.
pub struct HttpAuditor {
    _private: (),
}

impl HttpAuditor {
    pub fn new(_config: &HttpStep) -> Result<Self, ChainError> {
        Err(ChainError::NotImplemented { id: "http" })
    }
}

#[async_trait]
impl Auditor for HttpAuditor {
    fn id(&self) -> AuditorId {
        AuditorId("http")
    }

    // Unreachable: `new()` never yields an instance.
    async fn audit(&self, _skill: &ResolvedSkill) -> Result<AuditReport, AuditError> {
        Err(AuditError::Auditor {
            auditor: "http".to_string(),
            message: "not implemented".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skills_core::audit::Severity;
    use skills_core::domain::{Origin, SkillId, VendorName};
    use skills_core::manifest::OnFail;

    #[tokio::test]
    async fn noop_auditor_passes_everything() {
        let skill = ResolvedSkill {
            id: SkillId::new("s"),
            canonical_name: "s".into(),
            description: None,
            vendor: VendorName::new("a/x"),
            origin: Origin::Local { path: "./a".into() },
            ref_resolved: None,
            path: std::path::PathBuf::from("s"),
            files: vec![],
            content_hash: "h".into(),
        };
        let report = NoopAuditor.audit(&skill).await.unwrap();
        assert_eq!(report.worst(), Severity::Pass);
        assert_eq!(NoopAuditor.id(), AuditorId("noop"));
        assert_eq!(noop_chain().len(), 1);
    }

    fn manifest(audit_json: &str) -> Manifest {
        Manifest::parse(&format!(r#"{{ "audit": {audit_json} }}"#)).unwrap()
    }

    #[test]
    fn build_chain_default_static() {
        let chain = build_chain(&manifest(r#"{ "mode": "warn" }"#)).unwrap();
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].auditor.id(), AuditorId("static"));
        assert_eq!(chain[0].on_fail, None);
    }

    #[test]
    fn build_chain_carries_on_fail() {
        let chain = build_chain(&manifest(
            r#"{ "mode": "block", "pipeline": [ { "use": "static", "on-fail": "warn" } ] }"#,
        ))
        .unwrap();
        assert_eq!(chain[0].on_fail, Some(OnFail::Warn));
    }

    #[test]
    fn build_chain_mode_off_skips_construction() {
        // llm entries are tolerated while the audit stage is off.
        let chain = build_chain(&manifest(
            r#"{ "mode": "off", "pipeline": [ { "use": "llm" } ] }"#,
        ))
        .unwrap();
        assert!(chain.is_empty());
    }

    #[test]
    fn llm_and_http_stubs_fail_construction_with_clear_message() {
        let err = build_chain(&manifest(
            r#"{ "mode": "warn", "pipeline": [ { "use": "llm", "model": "m" } ] }"#,
        ))
        .unwrap_err();
        assert!(err.to_string().contains("'llm'"), "{err}");
        assert!(
            err.to_string()
                .contains("not implemented yet, coming in a future release"),
            "{err}"
        );

        let err = build_chain(&manifest(
            r#"{ "mode": "warn", "pipeline": [ { "use": "static" }, { "use": "http" } ] }"#,
        ))
        .unwrap_err();
        assert!(err.to_string().contains("'http'"), "{err}");
    }

    #[test]
    fn empty_pipeline_builds_empty_chain() {
        let chain = build_chain(&manifest(r#"{ "mode": "block", "pipeline": [] }"#)).unwrap();
        assert!(chain.is_empty());
    }
}
