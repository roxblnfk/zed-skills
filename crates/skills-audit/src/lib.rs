//! Auditor implementations.
//!
//! M1 ships only the no-op chain: the pipeline's Audit stage is fully wired
//! but always passes. `StaticAuditor` (and later LLM/HTTP auditors) land in
//! M4 behind the same `Auditor` trait.

use std::sync::Arc;

use async_trait::async_trait;

use skills_core::audit::{AuditReport, AuditorId};
use skills_core::domain::ResolvedSkill;
use skills_core::error::AuditError;
use skills_core::traits::Auditor;

/// Auditor that finds nothing: every skill passes.
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

/// The default M1 audit chain: a single no-op auditor.
pub fn noop_chain() -> Vec<Arc<dyn Auditor>> {
    vec![Arc::new(NoopAuditor)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use skills_core::audit::Severity;
    use skills_core::domain::{Origin, SkillId, VendorName};

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
}
