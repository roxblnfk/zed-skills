//! Stage 7 — Audit: run the configured auditor chain over each resolved
//! skill and aggregate verdicts (worst severity wins).
//!
//! M1 ships only a no-op chain, but the stage is fully wired: with
//! `mode: block` a `Block` verdict aborts the pipeline before Sync.

use std::sync::Arc;

use crate::audit::{AuditedSkill, Severity};
use crate::domain::ResolvedSkill;
use crate::error::AuditError;
use crate::manifest::AuditMode;
use crate::traits::Auditor;

pub async fn audit_all(
    skills: Vec<ResolvedSkill>,
    auditors: &[Arc<dyn Auditor>],
    mode: AuditMode,
) -> Result<Vec<AuditedSkill>, AuditError> {
    let mut out = Vec::with_capacity(skills.len());
    for skill in skills {
        let audited = if mode == AuditMode::Off {
            AuditedSkill {
                skill,
                verdict: Severity::Pass,
                findings: Vec::new(),
            }
        } else {
            audit_one(skill, auditors).await?
        };
        out.push(audited);
    }

    if mode == AuditMode::Block
        && let Some(blocked) = out.iter().find(|a| a.verdict == Severity::Block)
    {
        let reason = blocked
            .findings
            .iter()
            .filter(|f| f.severity == Severity::Block)
            .map(|f| f.message.clone())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(AuditError::Blocked {
            skill: blocked.skill.id.clone(),
            reason,
        });
    }
    Ok(out)
}

async fn audit_one(
    skill: ResolvedSkill,
    auditors: &[Arc<dyn Auditor>],
) -> Result<AuditedSkill, AuditError> {
    let mut findings = Vec::new();
    for auditor in auditors {
        let report = auditor.audit(&skill).await?;
        findings.extend(report.findings);
    }
    let verdict = findings
        .iter()
        .map(|f| f.severity)
        .max()
        .unwrap_or(Severity::Pass);
    Ok(AuditedSkill {
        skill,
        verdict,
        findings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::{AuditReport, AuditorId, Finding};
    use crate::domain::{Origin, SkillId, VendorName};
    use async_trait::async_trait;
    use std::path::PathBuf;

    fn skill(id: &str) -> ResolvedSkill {
        ResolvedSkill {
            id: SkillId::new(id),
            canonical_name: id.to_string(),
            description: None,
            vendor: VendorName::new("a/x"),
            origin: Origin::Local { path: "./a".into() },
            ref_resolved: None,
            path: PathBuf::from(id),
            files: vec![],
            content_hash: "h".into(),
        }
    }

    struct Fixed(Severity);

    #[async_trait]
    impl Auditor for Fixed {
        fn id(&self) -> AuditorId {
            AuditorId("fixed")
        }
        async fn audit(&self, _skill: &ResolvedSkill) -> Result<AuditReport, AuditError> {
            Ok(AuditReport {
                findings: vec![Finding {
                    severity: self.0,
                    message: "finding".into(),
                    location: None,
                }],
            })
        }
    }

    #[tokio::test]
    async fn mode_off_skips_auditors() {
        let auditors: Vec<Arc<dyn Auditor>> = vec![Arc::new(Fixed(Severity::Block))];
        let out = audit_all(vec![skill("s")], &auditors, AuditMode::Off)
            .await
            .unwrap();
        assert_eq!(out[0].verdict, Severity::Pass);
    }

    #[tokio::test]
    async fn warn_mode_records_but_does_not_abort() {
        let auditors: Vec<Arc<dyn Auditor>> = vec![Arc::new(Fixed(Severity::Block))];
        let out = audit_all(vec![skill("s")], &auditors, AuditMode::Warn)
            .await
            .unwrap();
        assert_eq!(out[0].verdict, Severity::Block);
    }

    #[tokio::test]
    async fn block_mode_aborts_on_block_verdict() {
        let auditors: Vec<Arc<dyn Auditor>> = vec![Arc::new(Fixed(Severity::Block))];
        let err = audit_all(vec![skill("s")], &auditors, AuditMode::Block)
            .await
            .unwrap_err();
        assert!(matches!(err, AuditError::Blocked { .. }));
    }

    #[tokio::test]
    async fn worst_severity_wins_across_chain() {
        let auditors: Vec<Arc<dyn Auditor>> = vec![
            Arc::new(Fixed(Severity::Pass)),
            Arc::new(Fixed(Severity::Warn)),
        ];
        let out = audit_all(vec![skill("s")], &auditors, AuditMode::Block)
            .await
            .unwrap();
        assert_eq!(out[0].verdict, Severity::Warn);
        assert_eq!(out[0].findings.len(), 2);
    }
}
