//! Stage 3 — TrustFilter (SPEC §8): decide which discovered donors take part
//! in the run, before anything is downloaded or scanned.
//!
//! Per donor, in order:
//!
//! 1. Malformed donors (invalid `extra.skills.source`) are dropped with a
//!    `[warn]` note — one bad vendor never blocks the rest.
//! 2. Undeclared donors (discovery candidates) are admitted only when
//!    discovery is enabled globally (manifest flag / `--discovery`) or the
//!    donor was named positionally. Left-out candidates drive the trailing
//!    `--discovery` hint.
//! 3. Positional filters (`skills update acme/foo` / `acme/*`) restrict the
//!    run to matching donors; naming a donor is an implicit trust grant.
//! 4. Everything else must clear the effective trust list:
//!    `trusted-replace: true` ? (project ∪ --trust) : (builtin ∪ project ∪
//!    --trust). User-declared donors (`local.dir`, `remote[]`) and direct
//!    dependencies (unless `trusted-replace`) bypass the list. Untrusted
//!    transitive donors are silently skipped and surfaced in the trailing
//!    `[skip]` block.

use crate::domain::{DonorStatus, Note, TrustBasis, VendorName, VendorRef};
use crate::error::TrustError;
use crate::pattern::{VendorPattern, matches_any};
use crate::pipeline::ctx::Ctx;

/// Built-in trusted-vendors list for the composer provider, embedded from
/// `resources/trusted-composer.txt` (SPEC §8).
const BUILTIN_COMPOSER_TRUST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../resources/trusted-composer.txt"
));

/// Parse the embedded built-in list: one pattern per line, `#` comments and
/// blank lines ignored.
pub fn builtin_trusted() -> Vec<VendorPattern> {
    BUILTIN_COMPOSER_TRUST
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            VendorPattern::parse(line)
                .expect("resources/trusted-composer.txt must contain only valid patterns")
        })
        .collect()
}

/// Which trust list approved a kept donor — used by `skills show` for the
/// `[builtin]` / `[direct-dep]` annotations. Priority: project → cli →
/// builtin → direct-dep (the most explicit source takes credit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustSource {
    Project,
    Cli,
    Builtin,
    DirectDep,
}

/// A donor that passed the filter, with its trust attribution.
#[derive(Debug, Clone)]
pub struct KeptDonor {
    pub vendor_ref: VendorRef,
    /// `None` for user-declared donors and positional grants.
    pub trust_source: Option<TrustSource>,
    /// Undeclared donor admitted via discovery (`[discovered]` in show).
    pub discovered: bool,
}

/// Why a donor did not take part in this run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// Transitive donor with no matching trust pattern.
    Untrusted,
    /// Invalid `extra.skills.source`; the payload is the reason detail.
    Malformed(String),
    /// Dropped by a positional package filter.
    FilteredOut,
    /// Discovery candidate left out because discovery is off.
    NotDeclared,
}

#[derive(Debug, Clone)]
pub struct SkippedDonor {
    pub name: VendorName,
    pub reason: SkipReason,
}

/// Output of the TrustFilter stage.
#[derive(Debug, Clone, Default)]
pub struct TrustOutcome {
    pub kept: Vec<KeptDonor>,
    pub skipped: Vec<SkippedDonor>,
    /// Diagnostics for the update report (`[warn]` malformed, `[skip]`
    /// untrusted, trailing `[hint]` about `--discovery`).
    pub notes: Vec<Note>,
}

impl TrustOutcome {
    /// Consume the outcome into the vendor refs that continue down the
    /// pipeline.
    pub fn into_kept_refs(self) -> Vec<VendorRef> {
        self.kept.into_iter().map(|k| k.vendor_ref).collect()
    }
}

pub fn trust_filter(ctx: &Ctx, vendors: Vec<VendorRef>) -> Result<TrustOutcome, TrustError> {
    let filters = &ctx.run.packages;
    let cli = &ctx.run.trust;
    let discovery_on = ctx.discovery_enabled();
    let replace = ctx.manifest.trusted_replace.unwrap_or(false);
    let project = ctx.manifest.trusted_patterns();
    let builtin = if replace {
        Vec::new()
    } else {
        builtin_trusted()
    };

    let mut outcome = TrustOutcome::default();
    let mut untrusted: Vec<VendorName> = Vec::new();
    let mut candidates: usize = 0;

    for donor in vendors {
        let name = donor.name.clone();

        // 1. Malformed donors: warn + drop, never block the run.
        if let DonorStatus::Malformed { reason } = &donor.status {
            outcome
                .notes
                .push(Note::warn(format!("{name}: {reason} — donor skipped")));
            outcome.skipped.push(SkippedDonor {
                reason: SkipReason::Malformed(reason.clone()),
                name,
            });
            continue;
        }

        let named = !filters.is_empty() && matches_any(filters, name.as_str());

        // 2. Discovery gate for undeclared candidates.
        if donor.status == DonorStatus::Undeclared && !discovery_on && !named {
            candidates += 1;
            outcome.skipped.push(SkippedDonor {
                name,
                reason: SkipReason::NotDeclared,
            });
            continue;
        }

        // 3. Positional package filter.
        if !filters.is_empty() && !named {
            outcome.skipped.push(SkippedDonor {
                name,
                reason: SkipReason::FilteredOut,
            });
            continue;
        }

        // 4. Trust. Positional naming is an implicit grant.
        let approved = named
            || match donor.trust {
                TrustBasis::UserDeclared => true,
                TrustBasis::DirectDependency if !replace => true,
                _ => {
                    matches_any(&project, name.as_str())
                        || matches_any(cli, name.as_str())
                        || matches_any(&builtin, name.as_str())
                }
            };
        if !approved {
            untrusted.push(name.clone());
            outcome.skipped.push(SkippedDonor {
                name,
                reason: SkipReason::Untrusted,
            });
            continue;
        }

        let trust_source = attribute(&donor, &project, cli, &builtin, replace);
        outcome.kept.push(KeptDonor {
            discovered: donor.status == DonorStatus::Undeclared,
            trust_source,
            vendor_ref: donor,
        });
    }

    // A positional filter that matches nothing is a usage error.
    if !filters.is_empty() && outcome.kept.is_empty() {
        return Err(TrustError::NoPackageMatch {
            patterns: filters
                .iter()
                .map(VendorPattern::as_str)
                .collect::<Vec<_>>()
                .join(", "),
        });
    }

    for name in untrusted {
        outcome.notes.push(Note::skip(format!(
            "{name}: untrusted package not synced (add it to \"trusted\" in skills.json or rerun \
             with --trust={name})"
        )));
    }
    if candidates > 0 && !discovery_on {
        outcome.notes.push(Note::hint(format!(
            "{candidates} package(s) ship undeclared skills; rerun with --discovery to include \
             them, or set \"discovery\": true in skills.json"
        )));
    }

    Ok(outcome)
}

/// Which list gets credit for trusting a kept donor.
fn attribute(
    donor: &VendorRef,
    project: &[VendorPattern],
    cli: &[VendorPattern],
    builtin: &[VendorPattern],
    replace: bool,
) -> Option<TrustSource> {
    let name = donor.name.as_str();
    if matches_any(project, name) {
        Some(TrustSource::Project)
    } else if matches_any(cli, name) {
        Some(TrustSource::Cli)
    } else if !replace && matches_any(builtin, name) {
        Some(TrustSource::Builtin)
    } else if !replace && donor.trust == TrustBasis::DirectDependency {
        Some(TrustSource::DirectDep)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        MaterializedVendor, NoteKind, Origin, ProviderId, SkillsFilter, SourceHint,
    };
    use crate::error::MaterializeError;
    use crate::manifest::MANIFEST_NAME;
    use crate::pipeline::ctx::{PrepareOptions, RunOptions, prepare};
    use crate::traits::{Cache, Vendor};
    use std::sync::Arc;

    struct StubVendor {
        name: VendorName,
        origin: Origin,
    }

    #[async_trait::async_trait]
    impl Vendor for StubVendor {
        fn name(&self) -> &VendorName {
            &self.name
        }
        fn origin(&self) -> &Origin {
            &self.origin
        }
        async fn materialize(&self, _: &Cache) -> Result<MaterializedVendor, MaterializeError> {
            Ok(MaterializedVendor {
                name: self.name.clone(),
                origin: self.origin.clone(),
                root: std::path::PathBuf::new(),
                ref_resolved: None,
                filter: SkillsFilter::All,
                source_hint: SourceHint::Probe,
            })
        }
    }

    fn donor(name: &str, trust: TrustBasis, status: DonorStatus) -> VendorRef {
        let origin = Origin::Local {
            path: format!("vendor/{name}"),
        };
        VendorRef {
            provider: ProviderId::Composer,
            name: VendorName::new(name),
            origin: origin.clone(),
            filter: SkillsFilter::All,
            trust,
            status,
            vendor: Arc::new(StubVendor {
                name: VendorName::new(name),
                origin,
            }),
        }
    }

    fn ctx_with(manifest: &str, run: RunOptions) -> (tempfile::TempDir, Ctx) {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(MANIFEST_NAME), manifest).unwrap();
        let ctx = prepare(
            tmp.path(),
            PrepareOptions {
                run,
                ..Default::default()
            },
        )
        .unwrap();
        (tmp, ctx)
    }

    fn patterns(raw: &[&str]) -> Vec<VendorPattern> {
        raw.iter()
            .map(|p| VendorPattern::parse(p).unwrap())
            .collect()
    }

    fn kept_names(outcome: &TrustOutcome) -> Vec<&str> {
        outcome
            .kept
            .iter()
            .map(|k| k.vendor_ref.name.as_str())
            .collect()
    }

    #[test]
    fn builtin_list_parses_with_expected_entries() {
        let builtin = builtin_trusted();
        let raw: Vec<&str> = builtin.iter().map(VendorPattern::as_str).collect();
        assert_eq!(
            raw,
            [
                "cycle/*",
                "doctrine/*",
                "internal/*",
                "laravel/*",
                "llm/*",
                "moonshine/*",
                "spiral/*",
                "symfony/*",
                "tempest/*",
                "temporal/*",
                "testo/*",
                "yiisoft/*",
            ]
        );
    }

    #[test]
    fn untrusted_transitive_is_skipped_with_note() {
        let (_tmp, ctx) = ctx_with("{}", RunOptions::default());
        let outcome = trust_filter(
            &ctx,
            vec![donor(
                "evil/payload",
                TrustBasis::Transitive,
                DonorStatus::Declared,
            )],
        )
        .unwrap();
        assert!(outcome.kept.is_empty());
        assert_eq!(outcome.skipped.len(), 1);
        assert_eq!(outcome.skipped[0].reason, SkipReason::Untrusted);
        assert!(
            outcome
                .notes
                .iter()
                .any(|n| n.kind == NoteKind::Skip && n.message.contains("evil/payload"))
        );
    }

    #[test]
    fn user_declared_donors_bypass_trust() {
        let (_tmp, ctx) = ctx_with("{}", RunOptions::default());
        let outcome = trust_filter(
            &ctx,
            vec![donor(
                "evil/remote",
                TrustBasis::UserDeclared,
                DonorStatus::Declared,
            )],
        )
        .unwrap();
        assert_eq!(kept_names(&outcome), ["evil/remote"]);
        assert_eq!(outcome.kept[0].trust_source, None);
    }

    #[test]
    fn direct_dependency_implicitly_trusted_unless_replace() {
        let (_tmp, ctx) = ctx_with("{}", RunOptions::default());
        let refs = vec![donor(
            "acme/direct",
            TrustBasis::DirectDependency,
            DonorStatus::Declared,
        )];
        let outcome = trust_filter(&ctx, refs.clone()).unwrap();
        assert_eq!(kept_names(&outcome), ["acme/direct"]);
        assert_eq!(outcome.kept[0].trust_source, Some(TrustSource::DirectDep));

        let (_tmp, ctx) = ctx_with(r#"{ "trusted-replace": true }"#, RunOptions::default());
        let outcome = trust_filter(&ctx, refs).unwrap();
        assert!(outcome.kept.is_empty());
        assert_eq!(outcome.skipped[0].reason, SkipReason::Untrusted);
    }

    #[test]
    fn builtin_list_approves_transitive_when_replace_false() {
        let (_tmp, ctx) = ctx_with("{}", RunOptions::default());
        let outcome = trust_filter(
            &ctx,
            vec![donor(
                "spiral/demo",
                TrustBasis::Transitive,
                DonorStatus::Declared,
            )],
        )
        .unwrap();
        assert_eq!(kept_names(&outcome), ["spiral/demo"]);
        assert_eq!(outcome.kept[0].trust_source, Some(TrustSource::Builtin));
    }

    #[test]
    fn trusted_replace_disables_builtin_and_limits_to_project_list() {
        let (_tmp, ctx) = ctx_with(
            r#"{ "trusted": ["acme/pkg"], "trusted-replace": true }"#,
            RunOptions::default(),
        );
        let outcome = trust_filter(
            &ctx,
            vec![
                donor("spiral/demo", TrustBasis::Transitive, DonorStatus::Declared),
                donor("acme/pkg", TrustBasis::Transitive, DonorStatus::Declared),
            ],
        )
        .unwrap();
        assert_eq!(kept_names(&outcome), ["acme/pkg"]);
        assert_eq!(outcome.kept[0].trust_source, Some(TrustSource::Project));
    }

    #[test]
    fn project_wildcard_and_cli_trust_apply() {
        let (_tmp, ctx) = ctx_with(
            r#"{ "trusted": ["evil/*"] }"#,
            RunOptions {
                trust: patterns(&["clash/skills-conflict"]),
                ..Default::default()
            },
        );
        let outcome = trust_filter(
            &ctx,
            vec![
                donor(
                    "evil/payload",
                    TrustBasis::Transitive,
                    DonorStatus::Declared,
                ),
                donor(
                    "clash/skills-conflict",
                    TrustBasis::Transitive,
                    DonorStatus::Declared,
                ),
                donor("other/pkg", TrustBasis::Transitive, DonorStatus::Declared),
            ],
        )
        .unwrap();
        assert_eq!(
            kept_names(&outcome),
            ["evil/payload", "clash/skills-conflict"]
        );
        assert_eq!(outcome.kept[0].trust_source, Some(TrustSource::Project));
        assert_eq!(outcome.kept[1].trust_source, Some(TrustSource::Cli));
    }

    #[test]
    fn positional_filter_restricts_and_implicitly_trusts() {
        let (_tmp, ctx) = ctx_with(
            "{}",
            RunOptions {
                packages: patterns(&["evil/payload"]),
                ..Default::default()
            },
        );
        let outcome = trust_filter(
            &ctx,
            vec![
                donor(
                    "evil/payload",
                    TrustBasis::Transitive,
                    DonorStatus::Declared,
                ),
                donor(
                    "acme/direct",
                    TrustBasis::DirectDependency,
                    DonorStatus::Declared,
                ),
            ],
        )
        .unwrap();
        assert_eq!(kept_names(&outcome), ["evil/payload"]);
        // Silent: no untrusted note for the named donor.
        assert!(outcome.notes.iter().all(|n| n.kind != NoteKind::Skip));
        assert!(
            outcome
                .skipped
                .iter()
                .any(|s| s.name.as_str() == "acme/direct" && s.reason == SkipReason::FilteredOut)
        );
    }

    #[test]
    fn positional_matching_nothing_is_a_usage_error() {
        let (_tmp, ctx) = ctx_with(
            "{}",
            RunOptions {
                packages: patterns(&["ghost/package"]),
                ..Default::default()
            },
        );
        let err = trust_filter(
            &ctx,
            vec![donor(
                "acme/direct",
                TrustBasis::DirectDependency,
                DonorStatus::Declared,
            )],
        )
        .unwrap_err();
        assert!(err.to_string().contains("ghost/package"), "{err}");
    }

    #[test]
    fn undeclared_candidates_hint_without_discovery() {
        let (_tmp, ctx) = ctx_with("{}", RunOptions::default());
        let outcome = trust_filter(
            &ctx,
            vec![donor(
                "acme/undeclared",
                TrustBasis::DirectDependency,
                DonorStatus::Undeclared,
            )],
        )
        .unwrap();
        assert!(outcome.kept.is_empty());
        assert_eq!(outcome.skipped[0].reason, SkipReason::NotDeclared);
        assert!(
            outcome
                .notes
                .iter()
                .any(|n| n.kind == NoteKind::Hint && n.message.contains("--discovery"))
        );
    }

    #[test]
    fn discovery_flag_admits_candidates_and_drops_the_hint() {
        let (_tmp, ctx) = ctx_with(
            "{}",
            RunOptions {
                discovery: Some(true),
                ..Default::default()
            },
        );
        let outcome = trust_filter(
            &ctx,
            vec![donor(
                "acme/undeclared",
                TrustBasis::DirectDependency,
                DonorStatus::Undeclared,
            )],
        )
        .unwrap();
        assert_eq!(kept_names(&outcome), ["acme/undeclared"]);
        assert!(outcome.kept[0].discovered);
        assert!(outcome.notes.iter().all(|n| n.kind != NoteKind::Hint));
    }

    #[test]
    fn manifest_discovery_flag_works_and_cli_overrides() {
        let refs = |status| {
            vec![donor(
                "acme/undeclared",
                TrustBasis::DirectDependency,
                status,
            )]
        };
        let (_tmp, ctx) = ctx_with(r#"{ "discovery": true }"#, RunOptions::default());
        let outcome = trust_filter(&ctx, refs(DonorStatus::Undeclared)).unwrap();
        assert_eq!(outcome.kept.len(), 1);

        // CLI override wins over the manifest.
        let (_tmp, ctx) = ctx_with(
            r#"{ "discovery": true }"#,
            RunOptions {
                discovery: Some(false),
                ..Default::default()
            },
        );
        let outcome = trust_filter(&ctx, refs(DonorStatus::Undeclared)).unwrap();
        assert!(outcome.kept.is_empty());
    }

    #[test]
    fn naming_undeclared_package_grants_discovery_for_it_only() {
        let (_tmp, ctx) = ctx_with(
            "{}",
            RunOptions {
                packages: patterns(&["acme/*"]),
                ..Default::default()
            },
        );
        let outcome = trust_filter(
            &ctx,
            vec![
                donor(
                    "acme/undeclared",
                    TrustBasis::Transitive,
                    DonorStatus::Undeclared,
                ),
                donor(
                    "nested/tree",
                    TrustBasis::Transitive,
                    DonorStatus::Undeclared,
                ),
            ],
        )
        .unwrap();
        assert_eq!(kept_names(&outcome), ["acme/undeclared"]);
        assert!(outcome.kept[0].discovered);
        // The unnamed candidate still drives the hint.
        assert!(outcome.notes.iter().any(|n| n.kind == NoteKind::Hint));
    }

    #[test]
    fn malformed_donor_warns_and_never_blocks_others() {
        let (_tmp, ctx) = ctx_with("{}", RunOptions::default());
        let outcome = trust_filter(
            &ctx,
            vec![
                donor(
                    "acme/broken",
                    TrustBasis::DirectDependency,
                    DonorStatus::Malformed {
                        reason: "extra.skills.source must not escape the package root".into(),
                    },
                ),
                donor(
                    "acme/fine",
                    TrustBasis::DirectDependency,
                    DonorStatus::Declared,
                ),
            ],
        )
        .unwrap();
        assert_eq!(kept_names(&outcome), ["acme/fine"]);
        assert!(
            outcome
                .notes
                .iter()
                .any(|n| n.kind == NoteKind::Warn && n.message.contains("acme/broken"))
        );
        assert!(matches!(
            &outcome.skipped[0].reason,
            SkipReason::Malformed(reason) if reason.contains("escape")
        ));
    }
}
