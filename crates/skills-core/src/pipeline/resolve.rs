//! Stage 6 — Resolve (barrier): allowlist filtering, dir-name safety and
//! conflict detection.
//!
//! Allowlists (`skills: [...]`) are applied first — a skill the user opted
//! out of can neither cause a conflict nor trip the safety check (the
//! allowlist is the user's escape hatch; an excluded skill is never
//! written). Then two barrier checks, both aborting before any filesystem
//! write:
//!
//! 1. **FS-dangerous directory names** ([`crate::naming::dir_name_danger`]):
//!    reserved Windows device names, trailing dot/space, illegal or control
//!    characters. Checked on every host (portability guarantee — a manifest
//!    synced on Linux may be checked out on Windows).
//! 2. **Conflicts**, grouped by the *normalized* directory name
//!    ([`crate::naming::conflict_key`]: NFC + case fold) so `Foo`/`foo`/NFD
//!    variants — which merge into one directory on case-insensitive
//!    filesystems — are conflicts even across spellings, listing all
//!    offenders with their original spellings.
//!
//! Allowlists match the *canonical* name; unknown allowlist names produce a
//! hint, never an abort.

use std::collections::BTreeMap;

use crate::domain::{
    MaterializedVendor, Note, ResolvedSkill, ScannedSkill, SkillsFilter, VendorName,
};
use crate::error::{Conflict, DangerousName, ResolveError};
use crate::naming;

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

    // 2. Hard safety: a dir name that cannot be created on every supported
    // filesystem aborts the run before any write (like a conflict).
    let mut dangerous: Vec<DangerousName> = kept
        .iter()
        .filter_map(|skill| {
            naming::dir_name_danger(skill.id.as_str()).map(|reason| DangerousName {
                id: skill.id.clone(),
                vendor: skill.vendor.clone(),
                reason,
            })
        })
        .collect();
    if !dangerous.is_empty() {
        dangerous.sort_by(|a, b| (&a.id, &a.vendor).cmp(&(&b.id, &b.vendor)));
        dangerous.dedup();
        return Err(ResolveError::DangerousName(dangerous));
    }

    // 3. Conflict detection on the normalized dir_name (NFC + case fold):
    // spellings that merge into one directory on a case-insensitive
    // filesystem conflict even within a single donor.
    let mut by_key: BTreeMap<String, Vec<&ScannedSkill>> = BTreeMap::new();
    for skill in &kept {
        by_key
            .entry(naming::conflict_key(skill.id.as_str()))
            .or_default()
            .push(skill);
    }
    let mut conflicts = Vec::new();
    for (_, owners) in by_key {
        let mut ids: Vec<_> = owners.iter().map(|s| s.id.clone()).collect();
        ids.sort();
        ids.dedup();
        let mut vendors_of: Vec<VendorName> = owners.iter().map(|s| s.vendor.clone()).collect();
        vendors_of.sort();
        vendors_of.dedup();
        if vendors_of.len() > 1 || ids.len() > 1 {
            conflicts.push(Conflict {
                ids,
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
            source_hint: crate::domain::SourceHint::ExplicitRoot,
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
        let ResolveError::Conflict(conflicts) = err else {
            panic!("expected a conflict, got: {err}");
        };
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].ids, vec![SkillId::new("clash")]);
        assert_eq!(conflicts[0].display_ids(), "'clash'");
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
        let err = resolve(scanned, &vendors).unwrap_err();
        let ResolveError::Conflict(conflicts) = err else {
            panic!("expected a conflict, got: {err}");
        };
        assert_eq!(conflicts.len(), 2);
    }

    #[test]
    fn case_variants_from_two_donors_conflict_with_original_spellings() {
        // Windows/macOS filesystems are case-insensitive: `Foo` and `foo`
        // would silently merge into one target directory.
        let scanned = vec![skill("a/x", "Foo", "foo-a"), skill("b/y", "foo", "foo-b")];
        let vendors = vec![
            vendor("a/x", SkillsFilter::All),
            vendor("b/y", SkillsFilter::All),
        ];
        let err = resolve(scanned, &vendors).unwrap_err();
        let ResolveError::Conflict(conflicts) = err else {
            panic!("expected a conflict, got: {err}");
        };
        assert_eq!(conflicts.len(), 1);
        assert_eq!(
            conflicts[0].ids,
            vec![SkillId::new("Foo"), SkillId::new("foo")]
        );
        assert_eq!(conflicts[0].display_ids(), "'Foo'/'foo'");
        assert_eq!(
            conflicts[0].vendors,
            vec![VendorName::new("a/x"), VendorName::new("b/y")]
        );
        // The rendered error carries both original spellings.
        assert_eq!(
            ResolveError::Conflict(conflicts).to_string(),
            "skill name conflict: 'Foo'/'foo' provided by [a/x, b/y]"
        );
    }

    #[test]
    fn unicode_normalization_variants_conflict() {
        // NFC "café" vs NFD "café": one directory on macOS (and confusing
        // everywhere else).
        let scanned = vec![
            skill("a/x", "caf\u{e9}", "cafe-a"),
            skill("b/y", "cafe\u{301}", "cafe-b"),
        ];
        let vendors = vec![
            vendor("a/x", SkillsFilter::All),
            vendor("b/y", SkillsFilter::All),
        ];
        let err = resolve(scanned, &vendors).unwrap_err();
        let ResolveError::Conflict(conflicts) = err else {
            panic!("expected a conflict, got: {err}");
        };
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].ids.len(), 2, "both spellings listed");
    }

    #[test]
    fn case_variants_within_one_donor_conflict() {
        // A single donor shipping `Foo` and `foo` (possible in an archive
        // extracted on a case-sensitive filesystem) still collides in the
        // target on Windows/macOS.
        let scanned = vec![skill("a/x", "Foo", "foo-1"), skill("a/x", "foo", "foo-2")];
        let vendors = vec![vendor("a/x", SkillsFilter::All)];
        let err = resolve(scanned, &vendors).unwrap_err();
        let ResolveError::Conflict(conflicts) = err else {
            panic!("expected a conflict, got: {err}");
        };
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].display_ids(), "'Foo'/'foo'");
        assert_eq!(conflicts[0].vendors, vec![VendorName::new("a/x")]);
    }

    #[test]
    fn exact_duplicate_within_one_donor_is_not_a_conflict() {
        // Same spelling twice from one donor (two skills roots): the
        // pre-existing behavior — not a cross-donor conflict.
        let scanned = vec![skill("a/x", "same", "s1"), skill("a/x", "same", "s2")];
        let vendors = vec![vendor("a/x", SkillsFilter::All)];
        assert!(resolve(scanned, &vendors).is_ok());
    }

    #[test]
    fn dangerous_dir_name_aborts_naming_skill_donor_and_reason() {
        let scanned = vec![
            skill("a/x", "fine", "fine"),
            skill("a/x", "nul.txt", "sneaky"),
            skill("b/y", "trailing.", "dotty"),
        ];
        let vendors = vec![
            vendor("a/x", SkillsFilter::All),
            vendor("b/y", SkillsFilter::All),
        ];
        let err = resolve(scanned, &vendors).unwrap_err();
        let ResolveError::DangerousName(dangerous) = err else {
            panic!("expected a dangerous-name abort, got: {err}");
        };
        assert_eq!(dangerous.len(), 2);
        assert_eq!(dangerous[0].id.as_str(), "nul.txt");
        assert_eq!(dangerous[0].vendor, VendorName::new("a/x"));
        assert!(
            dangerous[0].reason.contains("'NUL'"),
            "{}",
            dangerous[0].reason
        );
        assert_eq!(dangerous[1].id.as_str(), "trailing.");
        assert!(
            dangerous[1].reason.contains("ends with a dot"),
            "{}",
            dangerous[1].reason
        );
        // The rendered error names skill, donor and reason.
        let message = ResolveError::DangerousName(dangerous).to_string();
        assert!(
            message.contains("'nul.txt' from a/x is the reserved Windows device name 'NUL'"),
            "{message}"
        );
    }

    #[test]
    fn allowlisted_out_dangerous_skill_does_not_abort() {
        // The allowlist is the user's escape hatch: an excluded skill is
        // never written, so its name cannot hurt.
        let scanned = vec![skill("a/x", "fine", "fine"), skill("a/x", "CON", "con")];
        let vendors = vec![vendor("a/x", SkillsFilter::Only(vec!["fine".to_string()]))];
        let res = resolve(scanned, &vendors).unwrap();
        assert_eq!(res.skills.len(), 1);
        assert_eq!(res.skills[0].id.as_str(), "fine");
    }

    #[test]
    fn dangerous_names_are_checked_before_conflicts() {
        // Both problems present: safety wins (the conflict grouping would
        // only be reachable after a rename anyway).
        let scanned = vec![skill("a/x", "NUL", "n1"), skill("b/y", "nul", "n2")];
        let vendors = vec![
            vendor("a/x", SkillsFilter::All),
            vendor("b/y", SkillsFilter::All),
        ];
        let err = resolve(scanned, &vendors).unwrap_err();
        assert!(matches!(err, ResolveError::DangerousName(_)), "{err}");
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
