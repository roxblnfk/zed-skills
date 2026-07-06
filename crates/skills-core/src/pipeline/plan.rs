//! Stage 8 — Plan: diff the audited skills against the lockfile.

use crate::audit::AuditedSkill;
use crate::lockfile::{LockedSkill, Lockfile};

#[derive(Debug, Clone, Default)]
pub struct SyncPlan {
    /// Skills not present in the lockfile.
    pub add: Vec<AuditedSkill>,
    /// Skills whose content hash differs from the lock (with the old entry —
    /// its file list drives removal of files the donor stopped shipping).
    pub update: Vec<(AuditedSkill, LockedSkill)>,
    /// Skills identical to the lock.
    pub skip: Vec<(AuditedSkill, LockedSkill)>,
    /// Lock entries whose donor (or skill) disappeared — to be pruned.
    pub remove: Vec<LockedSkill>,
    /// Lock entries out of scope of a partial run (positional filters,
    /// `--from`): carried into the new lockfile untouched, never pruned.
    pub keep: Vec<LockedSkill>,
}

impl SyncPlan {
    pub fn has_changes(&self) -> bool {
        !self.add.is_empty() || !self.update.is_empty() || !self.remove.is_empty()
    }
}

/// Diff the audited skills against the lockfile. `partial` marks a scoped
/// run (positional filters / `--from`): lock entries whose skill is absent
/// from the run are then retained instead of pruned.
pub fn plan(lockfile: &Lockfile, audited: &[AuditedSkill], partial: bool) -> SyncPlan {
    let mut plan = SyncPlan::default();
    for entry in audited {
        match lockfile.find(&entry.skill.id) {
            None => plan.add.push(entry.clone()),
            Some(locked) if locked.content_hash == entry.skill.content_hash => {
                plan.skip.push((entry.clone(), locked.clone()));
            }
            Some(locked) => plan.update.push((entry.clone(), locked.clone())),
        }
    }
    for locked in &lockfile.skills {
        let still_present = audited.iter().any(|a| a.skill.id.as_str() == locked.id);
        if !still_present {
            if partial {
                plan.keep.push(locked.clone());
            } else {
                plan.remove.push(locked.clone());
            }
        }
    }
    plan.add.sort_by(|a, b| a.skill.id.cmp(&b.skill.id));
    plan.update.sort_by(|a, b| a.0.skill.id.cmp(&b.0.skill.id));
    plan.skip.sort_by(|a, b| a.0.skill.id.cmp(&b.0.skill.id));
    plan.remove.sort_by(|a, b| a.id.cmp(&b.id));
    plan.keep.sort_by(|a, b| a.id.cmp(&b.id));
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Origin, ResolvedSkill, SkillId, VendorName};
    use std::path::PathBuf;

    fn audited(id: &str, hash: &str) -> AuditedSkill {
        AuditedSkill::unaudited(ResolvedSkill {
            id: SkillId::new(id),
            canonical_name: id.to_string(),
            description: None,
            vendor: VendorName::new("a/x"),
            origin: Origin::Local { path: "./a".into() },
            ref_resolved: None,
            path: PathBuf::from(id),
            files: vec!["SKILL.md".into()],
            content_hash: hash.to_string(),
        })
    }

    fn locked(id: &str, hash: &str) -> LockedSkill {
        LockedSkill {
            id: id.to_string(),
            vendor: "a/x".into(),
            origin: Origin::Local { path: "./a".into() },
            ref_resolved: None,
            content_hash: hash.to_string(),
            files: vec!["SKILL.md".into()],
            audit: None,
        }
    }

    #[test]
    fn empty_lock_all_adds() {
        let lock = Lockfile::default();
        let p = plan(&lock, &[audited("a", "1"), audited("b", "2")], false);
        assert_eq!(p.add.len(), 2);
        assert!(p.update.is_empty() && p.skip.is_empty() && p.remove.is_empty());
        assert!(p.has_changes());
    }

    #[test]
    fn diff_classifies_all_four_cases() {
        let lock = Lockfile {
            skills: vec![
                locked("same", "hash-1"),
                locked("changed", "old-hash"),
                locked("gone", "hash-x"),
            ],
            ..Default::default()
        };
        let p = plan(
            &lock,
            &[
                audited("same", "hash-1"),
                audited("changed", "new-hash"),
                audited("brand-new", "hash-n"),
            ],
            false,
        );
        assert_eq!(
            p.add
                .iter()
                .map(|a| a.skill.id.as_str())
                .collect::<Vec<_>>(),
            ["brand-new"]
        );
        assert_eq!(p.update.len(), 1);
        assert_eq!(p.update[0].0.skill.id.as_str(), "changed");
        assert_eq!(p.update[0].1.content_hash, "old-hash");
        assert_eq!(p.skip.len(), 1);
        assert_eq!(p.skip[0].0.skill.id.as_str(), "same");
        assert_eq!(
            p.remove.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
            ["gone"]
        );
    }

    #[test]
    fn unchanged_everything_has_no_changes() {
        let lock = Lockfile {
            skills: vec![locked("a", "h")],
            ..Default::default()
        };
        let p = plan(&lock, &[audited("a", "h")], false);
        assert!(!p.has_changes());
        assert_eq!(p.skip.len(), 1);
    }

    #[test]
    fn partial_run_keeps_out_of_scope_entries_instead_of_pruning() {
        let lock = Lockfile {
            skills: vec![locked("in-scope", "h"), locked("out-of-scope", "h2")],
            ..Default::default()
        };
        let p = plan(&lock, &[audited("in-scope", "h")], true);
        assert!(p.remove.is_empty());
        assert_eq!(
            p.keep.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
            ["out-of-scope"]
        );
        assert!(!p.has_changes());

        // The same diff on a full run prunes.
        let p = plan(&lock, &[audited("in-scope", "h")], false);
        assert_eq!(
            p.remove.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
            ["out-of-scope"]
        );
        assert!(p.keep.is_empty());
    }
}
