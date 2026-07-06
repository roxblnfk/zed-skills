//! Stage 9 — Sync: the only stage that writes to the project.
//!
//! Transactional: every added/updated skill is first staged into a temporary
//! directory next to the target, then applied per skill. Non-destructive
//! merge: Sync owns exactly the files listed in the lockfile — donor files
//! overwrite, files the donor stopped shipping are removed only if
//! lock-listed, user-added files are always kept. The new lockfile is
//! written last.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use crate::audit::{AuditFinding, AuditedSkill, Severity};
use crate::domain::{Note, ResolvedSkill};
use crate::error::SyncError;
use crate::fsutil;
use crate::lockfile::{LOCKFILE_NAME, LockedSkill, Lockfile};
use crate::manifest::AuditMode;
use crate::paths::rel_to_path;
use crate::pipeline::ctx::Ctx;
use crate::pipeline::plan::SyncPlan;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncAction {
    Add,
    Update,
    Remove,
    Skip,
}

#[derive(Debug, Clone)]
pub struct SyncEntry {
    pub vendor: String,
    pub id: String,
    pub action: SyncAction,
    pub file_count: usize,
    /// Audit verdict of the skill; `None` for removals (nothing was audited).
    pub verdict: Option<Severity>,
    /// Effective findings from this run's audit chain (empty on cache hits).
    pub findings: Vec<AuditFinding>,
    /// The verdict was reused from the lockfile cache.
    pub audit_cached: bool,
}

#[derive(Debug, Clone)]
pub struct SyncReport {
    pub target_rel: String,
    pub dry_run: bool,
    pub audit_mode: AuditMode,
    /// Sorted by (vendor, id) so output groups cleanly per donor.
    pub entries: Vec<SyncEntry>,
    pub notes: Vec<Note>,
}

impl SyncReport {
    pub fn count(&self, action: SyncAction) -> usize {
        self.entries.iter().filter(|e| e.action == action).count()
    }
}

pub fn sync(ctx: &Ctx, plan: SyncPlan, notes: Vec<Note>) -> Result<SyncReport, SyncError> {
    let report = build_report(ctx, &plan, notes);
    if ctx.dry_run {
        return Ok(report);
    }

    apply(ctx, &plan)?;

    // New lockfile, written last: add/update from the fresh scan, skip keeps
    // its previous entry. Audit cache: `cache_entry` is stored for synced AND
    // skipped skills; `None` (mode off) leaves previous verdicts untouched.
    let mut lock = Lockfile::default();
    for audited in &plan.add {
        let mut locked = LockedSkill::from(&audited.skill);
        locked.audit = audited.cache_entry.clone();
        lock.skills.push(locked);
    }
    for (audited, old) in &plan.update {
        let mut locked = LockedSkill::from(&audited.skill);
        locked.audit = audited.cache_entry.clone().or_else(|| old.audit.clone());
        lock.skills.push(locked);
    }
    for (audited, old) in &plan.skip {
        let mut locked = old.clone();
        if let Some(entry) = &audited.cache_entry {
            locked.audit = Some(entry.clone());
        }
        lock.skills.push(locked);
    }
    // Out-of-scope entries of a partial run survive untouched.
    for locked in &plan.keep {
        lock.skills.push(locked.clone());
    }
    lock.save(&ctx.project_root.join(LOCKFILE_NAME))?;

    Ok(report)
}

fn build_report(ctx: &Ctx, plan: &SyncPlan, notes: Vec<Note>) -> SyncReport {
    let mut entries: Vec<SyncEntry> = Vec::new();
    let entry = |audited: &AuditedSkill, action: SyncAction| SyncEntry {
        vendor: audited.skill.vendor.as_str().to_string(),
        id: audited.skill.id.as_str().to_string(),
        action,
        file_count: audited.skill.files.len(),
        verdict: Some(audited.verdict),
        findings: audited.findings.clone(),
        audit_cached: audited.cached,
    };
    entries.extend(plan.add.iter().map(|a| entry(a, SyncAction::Add)));
    entries.extend(
        plan.update
            .iter()
            .map(|(a, _)| entry(a, SyncAction::Update)),
    );
    entries.extend(plan.skip.iter().map(|(a, _)| entry(a, SyncAction::Skip)));
    entries.extend(plan.remove.iter().map(|locked| SyncEntry {
        vendor: locked.vendor.clone(),
        id: locked.id.clone(),
        action: SyncAction::Remove,
        file_count: locked.files.len(),
        verdict: None,
        findings: Vec::new(),
        audit_cached: false,
    }));
    entries.sort_by(|a, b| (&a.vendor, &a.id).cmp(&(&b.vendor, &b.id)));
    SyncReport {
        target_rel: ctx.target_rel.clone(),
        dry_run: ctx.dry_run,
        audit_mode: ctx.manifest.audit_mode(),
        entries,
        notes,
    }
}

fn apply(ctx: &Ctx, plan: &SyncPlan) -> Result<(), SyncError> {
    let io_err = |path: &Path, source: io::Error| SyncError::Io {
        path: path.to_path_buf(),
        source,
    };
    let target = &ctx.target_abs;

    let staged: Vec<(&ResolvedSkill, Option<&LockedSkill>)> = plan
        .add
        .iter()
        .map(|a| (&a.skill, None))
        .chain(plan.update.iter().map(|(a, old)| (&a.skill, Some(old))))
        .collect();

    // Stage everything before touching the target.
    let staging = if staged.is_empty() {
        None
    } else {
        std::fs::create_dir_all(target).map_err(|e| io_err(target, e))?;
        let parent = target.parent().unwrap_or(target);
        let dir = tempfile::Builder::new()
            .prefix(".skills-staging-")
            .tempdir_in(parent)
            .map_err(|e| io_err(parent, e))?;
        for (skill, _) in &staged {
            let dst = dir.path().join(rel_to_path(skill.id.as_str()));
            fsutil::copy_files(&skill.path, &skill.files, &dst)
                .map_err(|e| io_err(&skill.path, e))?;
        }
        Some(dir)
    };

    // Apply staged skills.
    if let Some(staging) = &staging {
        for (skill, old_lock) in &staged {
            let staged_dir = staging.path().join(rel_to_path(skill.id.as_str()));
            let skill_target = target.join(rel_to_path(skill.id.as_str()));

            // Files the donor stopped shipping are removed only when
            // lock-listed (never touch user-added files).
            if let Some(old) = old_lock {
                let new_files: BTreeSet<&String> = skill.files.iter().collect();
                let stale: Vec<String> = old
                    .files
                    .iter()
                    .filter(|f| !new_files.contains(*f))
                    .cloned()
                    .collect();
                remove_files(&skill_target, &stale).map_err(|e| io_err(&skill_target, e))?;
            }

            if skill_target.exists() {
                fsutil::copy_files(&staged_dir, &skill.files, &skill_target)
                    .map_err(|e| io_err(&skill_target, e))?;
            } else {
                // Fresh skill: an atomic rename from the sibling staging dir.
                std::fs::rename(&staged_dir, &skill_target)
                    .or_else(|_| fsutil::copy_files(&staged_dir, &skill.files, &skill_target))
                    .map_err(|e| io_err(&skill_target, e))?;
            }
        }
    }

    // Prune skills whose donor disappeared: only lock-listed files; dirs are
    // kept if the user stored anything in them.
    for locked in &plan.remove {
        let skill_target = target.join(rel_to_path(&locked.id));
        if !skill_target.exists() {
            continue;
        }
        remove_files(&skill_target, &locked.files).map_err(|e| io_err(&skill_target, e))?;
        let _ = std::fs::remove_dir(&skill_target); // fails if user files remain
    }

    Ok(())
}

/// Delete the given relative files under `dir` (missing files are fine) and
/// clean up directories that became empty.
fn remove_files(dir: &Path, files: &[String]) -> io::Result<()> {
    let mut parents: BTreeSet<PathBuf> = BTreeSet::new();
    for rel in files {
        let path = dir.join(rel_to_path(rel));
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        }
        let mut cursor = path.parent();
        while let Some(p) = cursor {
            if p == dir {
                break;
            }
            parents.insert(p.to_path_buf());
            cursor = p.parent();
        }
    }
    // Deepest first: BTreeSet orders lexically, so reverse is depth-safe for
    // ancestor chains.
    for p in parents.iter().rev() {
        let _ = std::fs::remove_dir(p); // non-empty dirs (user files) survive
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Origin, SkillId, VendorName};
    use crate::manifest::MANIFEST_NAME;
    use crate::pipeline::ctx::{PrepareOptions, prepare};

    struct Fixture {
        _tmp: tempfile::TempDir,
        project: PathBuf,
        donor: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        let donor = tmp.path().join("donor");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&donor).unwrap();
        std::fs::write(project.join(MANIFEST_NAME), "{}").unwrap();
        Fixture {
            _tmp: tmp,
            project,
            donor,
        }
    }

    fn ctx(f: &Fixture, dry_run: bool) -> Ctx {
        prepare(
            &f.project,
            PrepareOptions {
                dry_run,
                ..Default::default()
            },
        )
        .unwrap()
    }

    fn donor_skill(f: &Fixture, id: &str, files: &[(&str, &str)]) -> ResolvedSkill {
        let dir = f.donor.join(id);
        for (rel, content) in files {
            let p = dir.join(rel_to_path(rel));
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        let listed = fsutil::list_files(&dir).unwrap();
        let hash = fsutil::content_hash(&dir, &listed).unwrap();
        ResolvedSkill {
            id: SkillId::new(id),
            canonical_name: id.to_string(),
            description: None,
            vendor: VendorName::new("dir/donor"),
            origin: Origin::Local {
                path: "./donor".into(),
            },
            ref_resolved: None,
            path: dir,
            files: listed,
            content_hash: hash,
        }
    }

    fn pass(skill: ResolvedSkill) -> AuditedSkill {
        AuditedSkill::unaudited(skill)
    }

    #[test]
    fn adds_new_skill_and_writes_lockfile() {
        let f = fixture();
        let skill = donor_skill(&f, "alpha", &[("SKILL.md", "hello"), ("refs/a.md", "ref")]);
        let ctx = ctx(&f, false);
        let plan = SyncPlan {
            add: vec![pass(skill)],
            ..Default::default()
        };
        let report = sync(&ctx, plan, vec![]).unwrap();
        assert_eq!(report.count(SyncAction::Add), 1);
        assert_eq!(
            std::fs::read_to_string(ctx.target_abs.join("alpha").join("SKILL.md")).unwrap(),
            "hello"
        );
        let lock = Lockfile::load(&f.project.join(LOCKFILE_NAME))
            .unwrap()
            .unwrap();
        assert_eq!(lock.skills.len(), 1);
        assert_eq!(lock.skills[0].files, ["SKILL.md", "refs/a.md"]);
        // No staging leftovers.
        let leftovers: Vec<_> = std::fs::read_dir(ctx.target_abs.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".skills-staging"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn update_overwrites_and_prunes_stale_lock_listed_files() {
        let f = fixture();
        let skill = donor_skill(&f, "alpha", &[("SKILL.md", "v2")]);
        let ctx = ctx(&f, false);
        // Target simulates a previous sync: stale donor file + a user file.
        let skill_dir = ctx.target_abs.join("alpha");
        std::fs::create_dir_all(skill_dir.join("old")).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "v1").unwrap();
        std::fs::write(skill_dir.join("old").join("gone.md"), "stale").unwrap();
        std::fs::write(skill_dir.join("user-notes.md"), "mine").unwrap();

        let old_lock = LockedSkill {
            id: "alpha".into(),
            vendor: "dir/donor".into(),
            origin: Origin::Local {
                path: "./donor".into(),
            },
            ref_resolved: None,
            content_hash: "old".into(),
            files: vec!["SKILL.md".into(), "old/gone.md".into()],
            audit: None,
        };
        let plan = SyncPlan {
            update: vec![(pass(skill), old_lock)],
            ..Default::default()
        };
        sync(&ctx, plan, vec![]).unwrap();

        assert_eq!(
            std::fs::read_to_string(skill_dir.join("SKILL.md")).unwrap(),
            "v2"
        );
        // Stale lock-listed file removed, its empty dir cleaned up.
        assert!(!skill_dir.join("old").exists());
        // User file untouched.
        assert_eq!(
            std::fs::read_to_string(skill_dir.join("user-notes.md")).unwrap(),
            "mine"
        );
    }

    #[test]
    fn remove_prunes_only_lock_listed_files_and_keeps_user_dirs() {
        let f = fixture();
        let ctx = ctx(&f, false);
        let skill_dir = ctx.target_abs.join("gone");
        std::fs::create_dir_all(skill_dir.join("scripts")).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "x").unwrap();
        std::fs::write(skill_dir.join("scripts").join("run.ps1"), "y").unwrap();
        std::fs::write(skill_dir.join("keep-me.md"), "user").unwrap();

        let locked = LockedSkill {
            id: "gone".into(),
            vendor: "dir/donor".into(),
            origin: Origin::Local {
                path: "./donor".into(),
            },
            ref_resolved: None,
            content_hash: "h".into(),
            files: vec!["SKILL.md".into(), "scripts/run.ps1".into()],
            audit: None,
        };
        let plan = SyncPlan {
            remove: vec![locked],
            ..Default::default()
        };
        sync(&ctx, plan, vec![]).unwrap();

        assert!(!skill_dir.join("SKILL.md").exists());
        assert!(!skill_dir.join("scripts").exists());
        // Skill dir survives because a user file lives in it.
        assert_eq!(
            std::fs::read_to_string(skill_dir.join("keep-me.md")).unwrap(),
            "user"
        );
    }

    #[test]
    fn remove_deletes_skill_dir_entirely_when_no_user_files() {
        let f = fixture();
        let ctx = ctx(&f, false);
        let skill_dir = ctx.target_abs.join("gone");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "x").unwrap();

        let locked = LockedSkill {
            id: "gone".into(),
            vendor: "dir/donor".into(),
            origin: Origin::Local {
                path: "./donor".into(),
            },
            ref_resolved: None,
            content_hash: "h".into(),
            files: vec!["SKILL.md".into()],
            audit: None,
        };
        let plan = SyncPlan {
            remove: vec![locked],
            ..Default::default()
        };
        sync(&ctx, plan, vec![]).unwrap();
        assert!(!skill_dir.exists());
    }

    #[test]
    fn dry_run_writes_nothing() {
        let f = fixture();
        let skill = donor_skill(&f, "alpha", &[("SKILL.md", "hello")]);
        let ctx = ctx(&f, true);
        let plan = SyncPlan {
            add: vec![pass(skill)],
            ..Default::default()
        };
        let report = sync(&ctx, plan, vec![]).unwrap();
        assert!(report.dry_run);
        assert_eq!(report.count(SyncAction::Add), 1);
        assert!(!ctx.target_abs.exists());
        assert!(!f.project.join(LOCKFILE_NAME).exists());
    }

    #[test]
    fn lockfile_reflects_add_update_skip_and_drops_removed() {
        let f = fixture();
        let added = donor_skill(&f, "added", &[("SKILL.md", "a")]);
        let updated = donor_skill(&f, "updated", &[("SKILL.md", "u2")]);
        let skipped = donor_skill(&f, "skipped", &[("SKILL.md", "s")]);
        let ctx = ctx(&f, false);
        std::fs::create_dir_all(ctx.target_abs.join("updated")).unwrap();
        std::fs::create_dir_all(ctx.target_abs.join("skipped")).unwrap();

        let old = |id: &str, hash: &str, files: Vec<String>| LockedSkill {
            id: id.into(),
            vendor: "dir/donor".into(),
            origin: Origin::Local {
                path: "./donor".into(),
            },
            ref_resolved: None,
            content_hash: hash.into(),
            files,
            audit: None,
        };
        let skipped_lock = old("skipped", &skipped.content_hash, skipped.files.clone());
        let plan = SyncPlan {
            add: vec![pass(added)],
            update: vec![(
                pass(updated),
                old("updated", "stale", vec!["SKILL.md".into()]),
            )],
            skip: vec![(pass(skipped), skipped_lock)],
            remove: vec![old("removed", "x", vec!["SKILL.md".into()])],
            keep: vec![old("kept", "k", vec!["SKILL.md".into()])],
        };
        sync(&ctx, plan, vec![]).unwrap();
        let lock = Lockfile::load(&f.project.join(LOCKFILE_NAME))
            .unwrap()
            .unwrap();
        let ids: Vec<&str> = lock.skills.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["added", "kept", "skipped", "updated"]);
    }

    #[test]
    fn lockfile_stores_and_preserves_audit_cache_entries() {
        use crate::lockfile::AuditCacheEntry;

        let f = fixture();
        let added = donor_skill(&f, "added", &[("SKILL.md", "a")]);
        let skipped = donor_skill(&f, "skipped", &[("SKILL.md", "s")]);
        let off_skipped = donor_skill(&f, "untouched", &[("SKILL.md", "u")]);
        let ctx = ctx(&f, false);

        let entry = |verdict: &str| AuditCacheEntry {
            verdict: verdict.into(),
            auditor_set_hash: "set-hash".into(),
        };
        let with_audit = |skill: ResolvedSkill, verdict: &str| {
            let mut a = AuditedSkill::unaudited(skill);
            a.cache_entry = Some(entry(verdict));
            a
        };
        let old = |skill: &ResolvedSkill, audit: Option<AuditCacheEntry>| LockedSkill {
            id: skill.id.as_str().into(),
            vendor: "dir/donor".into(),
            origin: Origin::Local {
                path: "./donor".into(),
            },
            ref_resolved: None,
            content_hash: skill.content_hash.clone(),
            files: skill.files.clone(),
            audit,
        };

        let skipped_lock = old(&skipped, Some(entry("pass")));
        let untouched_lock = old(&off_skipped, Some(entry("warn")));
        let plan = SyncPlan {
            // Synced skill: fresh verdict stored.
            add: vec![with_audit(added, "warn")],
            // Skipped skill, re-audited (e.g. pipeline changed): overwritten.
            skip: vec![
                (with_audit(skipped, "block"), skipped_lock),
                // Mode off (`cache_entry: None`): previous verdict untouched.
                (pass(off_skipped), untouched_lock),
            ],
            ..Default::default()
        };
        sync(&ctx, plan, vec![]).unwrap();

        let lock = Lockfile::load(&f.project.join(LOCKFILE_NAME))
            .unwrap()
            .unwrap();
        let audit_of = |id: &str| {
            lock.skills
                .iter()
                .find(|s| s.id == id)
                .and_then(|s| s.audit.clone())
        };
        assert_eq!(audit_of("added").unwrap().verdict, "warn");
        assert_eq!(audit_of("skipped").unwrap().verdict, "block");
        assert_eq!(audit_of("untouched").unwrap().verdict, "warn");
    }
}
