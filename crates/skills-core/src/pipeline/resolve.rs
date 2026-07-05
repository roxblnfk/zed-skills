//! Stage 6 — Resolve (barrier): allowlist filtering + conflict detection.
//!
//! Allowlists (`skills: [...]`) are applied first — a skill the user opted
//! out of cannot cause a conflict. Conflicts are detected on the *directory
//! name*: the same `dir_name` owned by more than one donor aborts the run
//! (before any filesystem write) listing all offenders. Allowlists match the
//! *canonical* name; unknown allowlist names produce a hint, never an abort.

use std::collections::BTreeMap;

use crate::domain::{
    MaterializedVendor, Note, ResolvedSkill, ScannedSkill, SkillsFilter, VendorName,
};
use crate::error::{Conflict, ResolveError};

#[derive(Debug, Clone)]
pub struct Resolution {
    pub skills: Vec<ResolvedSkill>,
    pub notes: Vec<Note>,
}

pub fn resolve(
    scanned: Vec<ScannedSkill>,
    vendors: &[MaterializedVendor],
) -> Result<Resolution, ResolveError> {
    let mut notes = Vec::new();

    // 1. Apply per-vendor allowlists (matched by canonical name).
    let filters: BTreeMap<&VendorName, &SkillsFilter> =
        vendors.iter().map(|v| (&v.name, &v.filter)).collect();

    let mut kept: Vec<ScannedSkill> = Vec::new();
    for skill in scanned {
        let filter = filters
            .get(&skill.vendor)
            .copied()
            .unwrap_or(&SkillsFilter::All);
        if filter.allows(&skill.canonical_name) {
            kept.push(skill);
        } else {
            notes.push(Note::skip(format!(
                "{}: skill '{}' excluded by skills[] allowlist",
                skill.vendor, skill.canonical_name
            )));
        }
    }

    // Unknown allowlist names: warn, do not abort.
    for vendor in vendors {
        if let SkillsFilter::Only(names) = &vendor.filter {
            if names.is_empty() {
                notes.push(Note::skip(format!(
                    "{}: skills [] — donor registered, pulls nothing",
                    vendor.name
                )));
            }
            for name in names {
                let known = kept
                    .iter()
                    .any(|s| s.vendor == vendor.name && &s.canonical_name == name);
                if !known {
                    notes.push(Note::hint(format!(
                        "{}: skills[] entry '{name}' matches no skill of this donor",
                        vendor.name
                    )));
                }
            }
        }
    }

    // 2. Conflict detection on dir_name across donors.
    let mut by_dir: BTreeMap<&str, Vec<&ScannedSkill>> = BTreeMap::new();
    for skill in &kept {
        by_dir.entry(skill.id.as_str()).or_default().push(skill);
    }
    let mut conflicts = Vec::new();
    for (_, owners) in by_dir {
        let mut vendors_of: Vec<VendorName> = owners.iter().map(|s| s.vendor.clone()).collect();
        vendors_of.sort();
        vendors_of.dedup();
        if vendors_of.len() > 1 {
            conflicts.push(Conflict {
                id: owners[0].id.clone(),
                vendors: vendors_of,
            });
        }
    }
    if !conflicts.is_empty() {
        return Err(ResolveError::Conflict(conflicts));
    }

    let mut skills: Vec<ResolvedSkill> = kept.into_iter().map(ResolvedSkill::from).collect();
    skills.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(Resolution { skills, notes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{NoteKind, Origin, SkillId};
    use std::path::PathBuf;

    fn skill(vendor: &str, dir_name: &str, canonical: &str) -> ScannedSkill {
        ScannedSkill {
            id: SkillId::new(dir_name),
            canonical_name: canonical.to_string(),
            description: None,
            vendor: VendorName::new(vendor),
            origin: Origin::Local {
                path: format!("./{vendor}"),
            },
            ref_resolved: None,
            path: PathBuf::from(format!("{vendor}/{dir_name}")),
            files: vec!["SKILL.md".to_string()],
            content_hash: "h".to_string(),
        }
    }

    fn vendor(name: &str, filter: SkillsFilter) -> MaterializedVendor {
        MaterializedVendor {
            name: VendorName::new(name),
            origin: Origin::Local {
                path: format!("./{name}"),
            },
            root: PathBuf::from(name),
            ref_resolved: None,
            filter,
        }
    }

    #[test]
    fn no_conflict_passes_through_sorted() {
        let scanned = vec![skill("a/x", "zeta", "zeta"), skill("a/x", "alpha", "alpha")];
        let vendors = vec![vendor("a/x", SkillsFilter::All)];
        let res = resolve(scanned, &vendors).unwrap();
        let ids: Vec<&str> = res.skills.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["alpha", "zeta"]);
        assert!(res.notes.is_empty());
    }

    #[test]
    fn same_dir_name_from_two_donors_aborts_with_offenders() {
        let scanned = vec![
            skill("a/x", "clash", "clash"),
            skill("b/y", "clash", "clash-b"),
            skill("a/x", "fine", "fine"),
        ];
        let vendors = vec![
            vendor("a/x", SkillsFilter::All),
            vendor("b/y", SkillsFilter::All),
        ];
        let err = resolve(scanned, &vendors).unwrap_err();
        let ResolveError::Conflict(conflicts) = err;
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].id.as_str(), "clash");
        assert_eq!(
            conflicts[0].vendors,
            vec![VendorName::new("a/x"), VendorName::new("b/y")]
        );
    }

    #[test]
    fn multiple_conflicts_all_reported() {
        let scanned = vec![
            skill("a/x", "one", "one"),
            skill("b/y", "one", "one"),
            skill("a/x", "two", "two"),
            skill("b/y", "two", "two"),
        ];
        let vendors = vec![
            vendor("a/x", SkillsFilter::All),
            vendor("b/y", SkillsFilter::All),
        ];
        let ResolveError::Conflict(conflicts) = resolve(scanned, &vendors).unwrap_err();
        assert_eq!(conflicts.len(), 2);
    }

    #[test]
    fn allowlist_matches_canonical_name_not_dir_name() {
        // dir_name "review-dir", canonical "code-review": allowlist must use
        // the canonical name.
        let scanned = vec![skill("a/x", "review-dir", "code-review")];
        let vendors = vec![vendor(
            "a/x",
            SkillsFilter::Only(vec!["code-review".to_string()]),
        )];
        let res = resolve(scanned.clone(), &vendors).unwrap();
        assert_eq!(res.skills.len(), 1);

        // Matching by dir_name does NOT work.
        let vendors = vec![vendor(
            "a/x",
            SkillsFilter::Only(vec!["review-dir".to_string()]),
        )];
        let res = resolve(scanned, &vendors).unwrap();
        assert!(res.skills.is_empty());
        assert!(res.notes.iter().any(|n| n.kind == NoteKind::Hint));
    }

    #[test]
    fn empty_allowlist_pulls_nothing() {
        let scanned = vec![skill("a/x", "s1", "s1"), skill("a/x", "s2", "s2")];
        let vendors = vec![vendor("a/x", SkillsFilter::Only(vec![]))];
        let res = resolve(scanned, &vendors).unwrap();
        assert!(res.skills.is_empty());
        assert!(
            res.notes
                .iter()
                .any(|n| n.kind == NoteKind::Skip && n.message.contains("pulls nothing"))
        );
    }

    #[test]
    fn unknown_allowlist_name_warns_but_does_not_abort() {
        let scanned = vec![skill("a/x", "real", "real")];
        let vendors = vec![vendor(
            "a/x",
            SkillsFilter::Only(vec!["real".to_string(), "ghost".to_string()]),
        )];
        let res = resolve(scanned, &vendors).unwrap();
        assert_eq!(res.skills.len(), 1);
        let hints: Vec<&Note> = res
            .notes
            .iter()
            .filter(|n| n.kind == NoteKind::Hint)
            .collect();
        assert_eq!(hints.len(), 1);
        assert!(hints[0].message.contains("'ghost'"), "{}", hints[0].message);
    }

    #[test]
    fn allowlisted_out_skill_cannot_conflict() {
        // Both donors ship "clash", but donor b excludes it via allowlist.
        let scanned = vec![
            skill("a/x", "clash", "clash"),
            skill("b/y", "clash", "clash"),
        ];
        let vendors = vec![
            vendor("a/x", SkillsFilter::All),
            vendor("b/y", SkillsFilter::Only(vec![])),
        ];
        let res = resolve(scanned, &vendors).unwrap();
        assert_eq!(res.skills.len(), 1);
        assert_eq!(res.skills[0].vendor, VendorName::new("a/x"));
    }
}
