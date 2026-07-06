//! Stage 7 — Audit: run the configured auditor chain over each resolved
//! skill, apply per-entry `on-fail` caps, aggregate verdicts (worst effective
//! severity wins) and enforce the audit mode.
//!
//! Verdict cache: a lockfile entry with the same content hash and the same
//! auditor-set hash short-circuits the chain — the cached verdict is reused
//! (the mode is still enforced against it). `--re-audit` bypasses the cache;
//! `mode: off` skips the stage entirely and leaves cached verdicts untouched.

use std::sync::Arc;

use crate::audit::{AuditFinding, AuditedSkill, Severity, auditor_set_hash};
use crate::domain::ResolvedSkill;
use crate::error::AuditError;
use crate::lockfile::AuditCacheEntry;
use crate::manifest::{AuditMode, OnFail};
use crate::pipeline::ctx::Ctx;
use crate::traits::Auditor;

/// One link of the audit chain: an auditor plus its per-entry `on-fail` cap
/// from `skills.json`.
#[derive(Clone)]
pub struct ChainEntry {
    pub auditor: Arc<dyn Auditor>,
    pub on_fail: Option<OnFail>,
}

impl ChainEntry {
    pub fn new(auditor: Arc<dyn Auditor>) -> Self {
        ChainEntry {
            auditor,
            on_fail: None,
        }
    }
}

impl std::fmt::Debug for ChainEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChainEntry")
            .field("auditor", &self.auditor.id())
            .field("on_fail", &self.on_fail)
            .finish()
    }
}

/// Effective severity of a finding after the entry's `on-fail` cap:
/// `warn` downgrades Block to Warn, `block` upgrades Warn to Block; Pass is
/// never touched.
fn effective(severity: Severity, on_fail: Option<OnFail>) -> Severity {
    if severity == Severity::Pass {
        return Severity::Pass;
    }
    match on_fail {
        None => severity,
        Some(OnFail::Warn) => Severity::Warn,
        Some(OnFail::Block) => Severity::Block,
    }
}

pub async fn audit_all(
    ctx: &Ctx,
    skills: Vec<ResolvedSkill>,
    chain: &[ChainEntry],
) -> Result<Vec<AuditedSkill>, AuditError> {
    let mode = ctx.manifest.audit_mode();
    if mode == AuditMode::Off {
        return Ok(skills.into_iter().map(AuditedSkill::unaudited).collect());
    }

    let set_hash = auditor_set_hash(&ctx.manifest.audit_steps());
    let mut out = Vec::with_capacity(skills.len());
    for skill in skills {
        let audited = match cached_verdict(ctx, &skill, &set_hash) {
            Some(verdict) => AuditedSkill {
                skill,
                verdict,
                findings: Vec::new(),
                cached: true,
                cache_entry: Some(AuditCacheEntry {
                    verdict: verdict.as_str().to_string(),
                    auditor_set_hash: set_hash.clone(),
                }),
            },
            None => audit_one(skill, chain, &set_hash).await?,
        };
        out.push(audited);
    }

    if mode == AuditMode::Block
        && let Some(blocked) = out.iter().find(|a| a.verdict == Severity::Block)
    {
        let reason = if blocked.cached {
            "cached verdict 'block' (run `skills update --re-audit` to re-run the audit chain)"
                .to_string()
        } else {
            blocked
                .findings
                .iter()
                .filter(|f| f.severity == Severity::Block)
                .map(|f| f.message.clone())
                .collect::<Vec<_>>()
                .join("; ")
        };
        return Err(AuditError::Blocked {
            skill: blocked.skill.id.clone(),
            reason,
        });
    }
    Ok(out)
}

/// Cache hit = same content hash + same auditor-set hash, unless `--re-audit`
/// forces a re-run. Unknown verdict strings degrade to a miss.
fn cached_verdict(ctx: &Ctx, skill: &ResolvedSkill, set_hash: &str) -> Option<Severity> {
    if ctx.run.re_audit {
        return None;
    }
    let locked = ctx.lockfile.find(&skill.id)?;
    if locked.content_hash != skill.content_hash {
        return None;
    }
    let cached = locked.audit.as_ref()?;
    if cached.auditor_set_hash != set_hash {
        return None;
    }
    Severity::parse(&cached.verdict)
}

async fn audit_one(
    skill: ResolvedSkill,
    chain: &[ChainEntry],
    set_hash: &str,
) -> Result<AuditedSkill, AuditError> {
    let mut findings = Vec::new();
    for entry in chain {
        let report = entry.auditor.audit(&skill).await?;
        findings.extend(report.findings.into_iter().map(|f| AuditFinding {
            auditor: entry.auditor.id(),
            severity: effective(f.severity, entry.on_fail),
            message: f.message,
            location: f.location,
        }));
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
        cached: false,
        cache_entry: Some(AuditCacheEntry {
            verdict: verdict.as_str().to_string(),
            auditor_set_hash: set_hash.to_string(),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::{AuditReport, AuditorId, Finding};
    use crate::domain::{Origin, SkillId, VendorName};
    use crate::lockfile::{LockedSkill, Lockfile};
    use crate::manifest::{MANIFEST_NAME, Manifest};
    use crate::pipeline::ctx::{PrepareOptions, RunOptions, prepare};
    use async_trait::async_trait;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    /// Ctx with the given audit config and lockfile; the audit stage never
    /// touches the FS beyond Prepare.
    fn ctx(audit_json: &str, lockfile: Lockfile, re_audit: bool) -> (tempfile::TempDir, Ctx) {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(MANIFEST_NAME),
            format!(r#"{{ "audit": {audit_json} }}"#),
        )
        .unwrap();
        let mut ctx = prepare(
            tmp.path(),
            PrepareOptions {
                run: RunOptions {
                    re_audit,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
        ctx.lockfile = lockfile;
        (tmp, ctx)
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

    /// Counts invocations — for cache hit/miss assertions.
    struct Counting(Severity, AtomicUsize);

    #[async_trait]
    impl Auditor for Counting {
        fn id(&self) -> AuditorId {
            AuditorId("counting")
        }
        async fn audit(&self, _skill: &ResolvedSkill) -> Result<AuditReport, AuditError> {
            self.1.fetch_add(1, Ordering::SeqCst);
            Ok(AuditReport {
                findings: vec![Finding {
                    severity: self.0,
                    message: "counted".into(),
                    location: None,
                }],
            })
        }
    }

    fn chain(entries: Vec<(Severity, Option<OnFail>)>) -> Vec<ChainEntry> {
        entries
            .into_iter()
            .map(|(sev, on_fail)| ChainEntry {
                auditor: Arc::new(Fixed(sev)),
                on_fail,
            })
            .collect()
    }

    fn manifest_set_hash(audit_json: &str) -> String {
        let m = Manifest::parse(&format!(r#"{{ "audit": {audit_json} }}"#)).unwrap();
        auditor_set_hash(&m.audit_steps())
    }

    fn locked_with_audit(id: &str, hash: &str, verdict: &str, set_hash: &str) -> LockedSkill {
        LockedSkill {
            id: id.into(),
            vendor: "a/x".into(),
            origin: Origin::Local { path: "./a".into() },
            ref_resolved: None,
            content_hash: hash.into(),
            files: vec![],
            audit: Some(AuditCacheEntry {
                verdict: verdict.into(),
                auditor_set_hash: set_hash.into(),
            }),
        }
    }

    // --- mode / on-fail aggregation matrix ---------------------------------

    #[tokio::test]
    async fn mode_off_skips_auditors_and_stores_no_cache_entry() {
        let (_t, ctx) = ctx(r#"{ "mode": "off" }"#, Lockfile::default(), false);
        let out = audit_all(
            &ctx,
            vec![skill("s")],
            &chain(vec![(Severity::Block, None)]),
        )
        .await
        .unwrap();
        assert_eq!(out[0].verdict, Severity::Pass);
        assert!(out[0].findings.is_empty());
        assert!(out[0].cache_entry.is_none());
        assert!(!out[0].cached);
    }

    #[tokio::test]
    async fn warn_mode_records_but_does_not_abort() {
        let (_t, ctx) = ctx(r#"{ "mode": "warn" }"#, Lockfile::default(), false);
        let out = audit_all(
            &ctx,
            vec![skill("s")],
            &chain(vec![(Severity::Block, None)]),
        )
        .await
        .unwrap();
        assert_eq!(out[0].verdict, Severity::Block);
        assert_eq!(out[0].findings[0].auditor, AuditorId("fixed"));
    }

    #[tokio::test]
    async fn block_mode_aborts_on_block_verdict() {
        let (_t, ctx) = ctx(r#"{ "mode": "block" }"#, Lockfile::default(), false);
        let err = audit_all(
            &ctx,
            vec![skill("s")],
            &chain(vec![(Severity::Block, None)]),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AuditError::Blocked { .. }));
    }

    #[tokio::test]
    async fn aggregation_matrix_severity_and_on_fail() {
        // (auditor severity, on-fail) -> expected effective severity.
        let cases = [
            (Severity::Pass, None, Severity::Pass),
            (Severity::Pass, Some(OnFail::Warn), Severity::Pass),
            (Severity::Pass, Some(OnFail::Block), Severity::Pass),
            (Severity::Warn, None, Severity::Warn),
            (Severity::Warn, Some(OnFail::Warn), Severity::Warn),
            (Severity::Warn, Some(OnFail::Block), Severity::Block),
            (Severity::Block, None, Severity::Block),
            (Severity::Block, Some(OnFail::Warn), Severity::Warn),
            (Severity::Block, Some(OnFail::Block), Severity::Block),
        ];
        for (sev, on_fail, expected) in cases {
            let (_t, ctx) = ctx(r#"{ "mode": "warn" }"#, Lockfile::default(), false);
            let out = audit_all(&ctx, vec![skill("s")], &chain(vec![(sev, on_fail)]))
                .await
                .unwrap();
            assert_eq!(
                out[0].verdict, expected,
                "severity {sev:?} with on-fail {on_fail:?}"
            );
        }
    }

    #[tokio::test]
    async fn on_fail_warn_downgrade_prevents_block_mode_abort() {
        let (_t, ctx) = ctx(r#"{ "mode": "block" }"#, Lockfile::default(), false);
        let out = audit_all(
            &ctx,
            vec![skill("s")],
            &chain(vec![(Severity::Block, Some(OnFail::Warn))]),
        )
        .await
        .unwrap();
        assert_eq!(out[0].verdict, Severity::Warn);
    }

    #[tokio::test]
    async fn on_fail_block_upgrade_aborts_block_mode() {
        let (_t, ctx) = ctx(r#"{ "mode": "block" }"#, Lockfile::default(), false);
        let err = audit_all(
            &ctx,
            vec![skill("s")],
            &chain(vec![(Severity::Warn, Some(OnFail::Block))]),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AuditError::Blocked { .. }));
    }

    #[tokio::test]
    async fn worst_severity_wins_across_chain() {
        let (_t, ctx) = ctx(r#"{ "mode": "block" }"#, Lockfile::default(), false);
        let out = audit_all(
            &ctx,
            vec![skill("s")],
            &chain(vec![(Severity::Pass, None), (Severity::Warn, None)]),
        )
        .await
        .unwrap();
        assert_eq!(out[0].verdict, Severity::Warn);
        assert_eq!(out[0].findings.len(), 2);
    }

    // --- verdict cache ------------------------------------------------------

    #[tokio::test]
    async fn cache_hit_skips_the_chain_and_reuses_verdict() {
        let audit_json = r#"{ "mode": "warn" }"#;
        let set_hash = manifest_set_hash(audit_json);
        let lock = Lockfile {
            skills: vec![locked_with_audit("s", "h", "warn", &set_hash)],
            ..Default::default()
        };
        let (_t, ctx) = ctx(audit_json, lock, false);
        let counter = Arc::new(Counting(Severity::Block, AtomicUsize::new(0)));
        let entries = vec![ChainEntry::new(counter.clone() as Arc<dyn Auditor>)];
        let out = audit_all(&ctx, vec![skill("s")], &entries).await.unwrap();
        assert_eq!(counter.1.load(Ordering::SeqCst), 0, "chain must not run");
        assert_eq!(out[0].verdict, Severity::Warn);
        assert!(out[0].cached);
        // The cache entry is (re-)stored for the new lockfile.
        assert_eq!(out[0].cache_entry.as_ref().unwrap().verdict, "warn");
    }

    #[tokio::test]
    async fn cache_miss_on_content_change() {
        let audit_json = r#"{ "mode": "warn" }"#;
        let set_hash = manifest_set_hash(audit_json);
        let lock = Lockfile {
            skills: vec![locked_with_audit("s", "other-hash", "warn", &set_hash)],
            ..Default::default()
        };
        let (_t, ctx) = ctx(audit_json, lock, false);
        let counter = Arc::new(Counting(Severity::Pass, AtomicUsize::new(0)));
        let entries = vec![ChainEntry::new(counter.clone() as Arc<dyn Auditor>)];
        let out = audit_all(&ctx, vec![skill("s")], &entries).await.unwrap();
        assert_eq!(counter.1.load(Ordering::SeqCst), 1, "chain must re-run");
        assert!(!out[0].cached);
    }

    #[tokio::test]
    async fn cache_miss_on_pipeline_change() {
        // Cached under the default pipeline; run with an on-fail cap added.
        let cached_hash = manifest_set_hash(r#"{ "mode": "warn" }"#);
        let audit_json =
            r#"{ "mode": "warn", "pipeline": [ { "use": "static", "on-fail": "warn" } ] }"#;
        assert_ne!(cached_hash, manifest_set_hash(audit_json));
        let lock = Lockfile {
            skills: vec![locked_with_audit("s", "h", "warn", &cached_hash)],
            ..Default::default()
        };
        let (_t, ctx) = ctx(audit_json, lock, false);
        let counter = Arc::new(Counting(Severity::Pass, AtomicUsize::new(0)));
        let entries = vec![ChainEntry::new(counter.clone() as Arc<dyn Auditor>)];
        let out = audit_all(&ctx, vec![skill("s")], &entries).await.unwrap();
        assert_eq!(counter.1.load(Ordering::SeqCst), 1, "chain must re-run");
        assert!(!out[0].cached);
    }

    #[tokio::test]
    async fn re_audit_bypasses_the_cache() {
        let audit_json = r#"{ "mode": "warn" }"#;
        let set_hash = manifest_set_hash(audit_json);
        let lock = Lockfile {
            skills: vec![locked_with_audit("s", "h", "warn", &set_hash)],
            ..Default::default()
        };
        let (_t, ctx) = ctx(audit_json, lock, true);
        let counter = Arc::new(Counting(Severity::Pass, AtomicUsize::new(0)));
        let entries = vec![ChainEntry::new(counter.clone() as Arc<dyn Auditor>)];
        let out = audit_all(&ctx, vec![skill("s")], &entries).await.unwrap();
        assert_eq!(counter.1.load(Ordering::SeqCst), 1, "chain must re-run");
        assert!(!out[0].cached);
        assert_eq!(out[0].verdict, Severity::Pass);
    }

    #[tokio::test]
    async fn unknown_cached_verdict_is_a_miss() {
        let audit_json = r#"{ "mode": "warn" }"#;
        let set_hash = manifest_set_hash(audit_json);
        let lock = Lockfile {
            skills: vec![locked_with_audit("s", "h", "mystery", &set_hash)],
            ..Default::default()
        };
        let (_t, ctx) = ctx(audit_json, lock, false);
        let counter = Arc::new(Counting(Severity::Pass, AtomicUsize::new(0)));
        let entries = vec![ChainEntry::new(counter.clone() as Arc<dyn Auditor>)];
        audit_all(&ctx, vec![skill("s")], &entries).await.unwrap();
        assert_eq!(counter.1.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cached_block_verdict_is_enforced_under_block_mode() {
        let audit_json = r#"{ "mode": "block" }"#;
        let set_hash = manifest_set_hash(audit_json);
        let lock = Lockfile {
            skills: vec![locked_with_audit("s", "h", "block", &set_hash)],
            ..Default::default()
        };
        let (_t, ctx) = ctx(audit_json, lock, false);
        let err = audit_all(&ctx, vec![skill("s")], &[]).await.unwrap_err();
        let AuditError::Blocked { reason, .. } = err else {
            panic!("expected Blocked");
        };
        assert!(reason.contains("--re-audit"), "{reason}");
    }
}
