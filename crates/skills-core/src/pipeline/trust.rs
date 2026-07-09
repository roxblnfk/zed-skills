//! Stage 3 — TrustFilter: decide which discovered donors take part
//! in the run, before anything is downloaded or scanned.
//!
//! Per donor, in order:
//!
//! 1. Malformed donors (invalid `extra.skills.source`) are dropped with a
//!    `[warn]` note — one bad vendor never blocks the rest.
//! 2. Positional filters (`skills update acme/foo` / `acme/*`) restrict the
//!    run to matching donors; naming a donor is an implicit trust grant.
//! 3. Everything else must clear the effective trust list of its *own* package
//!    manager (composer / npm): `dependencies.<mgr>.trusted-replace: true` ?
//!    project : (builtin ∪ project), with the composer `--trust` CLI list also
//!    applied to composer donors. A donor is only ever matched against its own
//!    manager's lists, so composer and npm never cross-approve. User-declared
//!    donors (`sources[]`, incl. `dir` entries) have no trust manager and are
//!    approved outright; direct dependencies bypass the list unless their
//!    manager sets `trusted-replace`. Untrusted transitive donors are silently
//!    skipped and surfaced in the trailing `[skip]` block.

use std::collections::HashMap;

use crate::domain::{DonorStatus, Note, TrustBasis, VendorName, VendorRef};
use crate::error::TrustError;
use crate::pattern::{TrustPattern, VendorPattern, matches_any, trust_matches_any};
use crate::pipeline::ctx::Ctx;

/// Built-in trusted-vendors list for the composer provider, embedded from
/// `resources/trusted-composer.txt`.
const BUILTIN_COMPOSER_TRUST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../resources/trusted-composer.txt"
));

/// Built-in trusted-scopes list for the npm provider, embedded from
/// `resources/trusted-npm.txt`.
const BUILTIN_NPM_TRUST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../resources/trusted-npm.txt"
));

/// Built-in trusted-namespaces list for the go provider, embedded from
/// `resources/trusted-go.txt`.
const BUILTIN_GO_TRUST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../resources/trusted-go.txt"
));

/// Meaningful lines of an embedded built-in list: `#` comments and blank lines
/// dropped, surrounding whitespace trimmed.
fn trust_lines(raw: &'static str) -> impl Iterator<Item = &'static str> {
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
}

/// Parse the embedded composer built-in list into vendor patterns.
///
/// This is the only built-in list the live pipeline consumes today: npm and go
/// patterns are exposed raw via [`builtin_trusted_raw`] and their grammars are
/// enforced only when those providers land.
pub fn builtin_trusted() -> Vec<VendorPattern> {
    trust_lines(BUILTIN_COMPOSER_TRUST)
        .map(|line| {
            VendorPattern::parse(line)
                .expect("resources/trusted-composer.txt must contain only valid patterns")
        })
        .collect()
}

/// Raw built-in trust patterns for a package manager, one entry per meaningful
/// line (comments and blanks dropped, whitespace trimmed).
///
/// - `"composer"` → composer lines (same strings as [`builtin_trusted`])
/// - `"npm"` → npm scope patterns
/// - `"go"` → go module-path prefix patterns
/// - anything else → empty
///
/// Keyed by package-manager id (`composer`/`npm`/`go`) rather than
/// [`crate::domain::ProviderId`], which enumerates vendor *providers*
/// (`dir`/`composer`/`github`/…) and has no npm/go variants.
///
/// The npm and go grammars are only structural today; they are validated when
/// their providers land. The live pipeline consumes only the composer list.
pub fn builtin_trusted_raw(manager: &str) -> Vec<&'static str> {
    let raw = match manager {
        "composer" => BUILTIN_COMPOSER_TRUST,
        "npm" => BUILTIN_NPM_TRUST,
        "go" => BUILTIN_GO_TRUST,
        _ => return Vec::new(),
    };
    trust_lines(raw).collect()
}

/// Parse the built-in list of `manager` under its trust grammar. Only the
/// managers with a live grammar (`composer`, `npm`) are parsed; anything else
/// (including `go`, whose entries are module-path prefixes with no grammar
/// yet) yields no patterns. The embedded resources are asserted valid.
fn builtin_trust_patterns(manager: &str) -> Vec<TrustPattern> {
    if !matches!(manager, "composer" | "npm") {
        return Vec::new();
    }
    builtin_trusted_raw(manager)
        .into_iter()
        .map(|line| {
            TrustPattern::parse(manager, line)
                .expect("known manager")
                .unwrap_or_else(|e| panic!("resources/trusted-{manager}.txt: {e}"))
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
    /// Undeclared donor: its skills are located by the always-on well-known /
    /// recursive fallback (`[discovered]` in show).
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
    /// untrusted).
    pub notes: Vec<Note>,
}

impl TrustOutcome {
    /// Consume the outcome into the vendor refs that continue down the
    /// pipeline.
    pub fn into_kept_refs(self) -> Vec<VendorRef> {
        self.kept.into_iter().map(|k| k.vendor_ref).collect()
    }
}

/// The effective trust context for one package manager, computed on demand and
/// memoized per `trust_filter` call. `builtin` is empty when `replace` is set
/// (the project list replaces both the built-in list and direct-dependency
/// trust for that manager).
struct TrustCtx {
    project: Vec<TrustPattern>,
    builtin: Vec<TrustPattern>,
    replace: bool,
}

impl TrustCtx {
    fn build(ctx: &Ctx, manager: &str) -> TrustCtx {
        let replace = ctx.manifest.trusted_replace_for(manager);
        TrustCtx {
            project: ctx.manifest.trusted_patterns_for(manager),
            builtin: if replace {
                Vec::new()
            } else {
                builtin_trust_patterns(manager)
            },
            replace,
        }
    }
}

pub fn trust_filter(ctx: &Ctx, vendors: Vec<VendorRef>) -> Result<TrustOutcome, TrustError> {
    let filters = &ctx.run.packages;
    // The `--trust` CLI list is composer-grammar and applies to composer donors
    // only; npm bare/scoped names cannot be expressed by it anyway.
    let cli = &ctx.run.trust;

    // Per-manager trust context, memoized: composer and npm are the only
    // manager-scoped providers, so this cache holds at most two entries.
    let mut ctx_cache: HashMap<&'static str, TrustCtx> = HashMap::new();

    let mut outcome = TrustOutcome::default();
    // Untrusted donors, with the manager whose list they failed (for the note).
    let mut untrusted: Vec<(VendorName, Option<&'static str>)> = Vec::new();

    for donor in vendors {
        let name = donor.name.clone();

        // 1. Malformed donors: warn + drop, never block the run.
        if let DonorStatus::Malformed { reason } = &donor.status {
            outcome
                .notes
                .push(Note::warn(format!("{name}: {reason} (donor skipped)")));
            outcome.skipped.push(SkippedDonor {
                reason: SkipReason::Malformed(reason.clone()),
                name,
            });
            continue;
        }

        let named = !filters.is_empty() && matches_any(filters, name.as_str());

        // 2. Positional package filter. Undeclared donors are always admitted
        // past this point (discovery is always-on); they still have to clear
        // the trust list below.
        if !filters.is_empty() && !named {
            outcome.skipped.push(SkippedDonor {
                name,
                reason: SkipReason::FilteredOut,
            });
            continue;
        }

        // 3. Trust. A donor is matched only against its own manager's lists.
        // `sources[]` donors (`dir`/`github`/`gitlab`/`url`) have no trust
        // manager: they are `UserDeclared` and approved before any list check.
        let manager = donor.provider.trust_manager();
        let tctx = manager.map(|m| {
            &*ctx_cache
                .entry(m)
                .or_insert_with(|| TrustCtx::build(ctx, m))
        });

        // Whether the donor matches any effective trust list of its manager
        // (project → cli → builtin). Under `trusted-replace` the builtin list
        // is empty, so only the project (and composer `--trust`) list applies.
        let on_trust_list = |t: &TrustCtx| {
            trust_matches_any(&t.project, name.as_str())
                || (manager == Some("composer") && matches_any(cli, name.as_str()))
                || trust_matches_any(&t.builtin, name.as_str())
        };

        // Positional naming is an implicit grant.
        let approved = named
            || match donor.trust {
                TrustBasis::UserDeclared => true,
                TrustBasis::DirectDependency => match tctx {
                    // Direct-dep trust applies unless `trusted-replace` drops
                    // it — but a direct dep explicitly on the project list is
                    // still trusted (the list is the sole authority under
                    // replace).
                    Some(t) => !t.replace || on_trust_list(t),
                    // Not manager-scoped: direct-dep trust always applies.
                    None => true,
                },
                TrustBasis::Transitive => match tctx {
                    Some(t) => on_trust_list(t),
                    // A transitive donor with no manager cannot be on any list.
                    None => false,
                },
            };
        if !approved {
            untrusted.push((name.clone(), manager));
            outcome.skipped.push(SkippedDonor {
                name,
                reason: SkipReason::Untrusted,
            });
            continue;
        }

        let trust_source = attribute(&donor, manager, tctx, cli);
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

    for (name, manager) in untrusted {
        // Composer (and any non-manager edge case) keeps the historical wording
        // including the `--trust` hint; npm has no CLI trust expression.
        let note = if manager == Some("npm") {
            format!(
                "{name}: untrusted package not synced (add it to \
                 \"dependencies.npm.trusted\" in skills.json)"
            )
        } else {
            format!(
                "{name}: untrusted package not synced (add it to \
                 \"dependencies.composer.trusted\" in skills.json or rerun with --trust={name})"
            )
        };
        outcome.notes.push(Note::skip(note));
    }

    Ok(outcome)
}

/// Which list gets credit for trusting a kept donor, using the donor's own
/// manager lists. Priority: project → cli → builtin → direct-dep. Donors with
/// no trust manager (user-declared `sources[]`) get no chip.
fn attribute(
    donor: &VendorRef,
    manager: Option<&str>,
    tctx: Option<&TrustCtx>,
    cli: &[VendorPattern],
) -> Option<TrustSource> {
    let name = donor.name.as_str();
    let t = tctx?;
    if trust_matches_any(&t.project, name) {
        Some(TrustSource::Project)
    } else if manager == Some("composer") && matches_any(cli, name) {
        Some(TrustSource::Cli)
    } else if !t.replace && trust_matches_any(&t.builtin, name) {
        Some(TrustSource::Builtin)
    } else if !t.replace && donor.trust == TrustBasis::DirectDependency {
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

    fn donor_with(
        provider: ProviderId,
        name: &str,
        trust: TrustBasis,
        status: DonorStatus,
    ) -> VendorRef {
        let origin = Origin::Local {
            path: format!("vendor/{name}"),
        };
        VendorRef {
            provider,
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

    fn donor(name: &str, trust: TrustBasis, status: DonorStatus) -> VendorRef {
        donor_with(ProviderId::Composer, name, trust, status)
    }

    fn npm_donor(name: &str, trust: TrustBasis, status: DonorStatus) -> VendorRef {
        donor_with(ProviderId::Npm, name, trust, status)
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
    fn builtin_npm_raw_has_expected_entries() {
        let npm = builtin_trusted_raw("npm");
        assert_eq!(
            npm,
            [
                "@anthropic-ai/*",
                "@modelcontextprotocol/*",
                "@openai/*",
                "@google/*",
                "@angular/*",
                "@vue/*",
                "@sveltejs/*",
                "@nestjs/*",
                "@nuxt/*",
                "@astrojs/*",
                "@remix-run/*",
                "@vercel/*",
            ]
        );
    }

    #[test]
    fn builtin_go_raw_has_expected_entries() {
        let go = builtin_trusted_raw("go");
        assert_eq!(
            go,
            [
                "github.com/anthropics/*",
                "github.com/modelcontextprotocol/*",
                "github.com/golang/*",
                "golang.org/x/*",
                "google.golang.org/*",
                "github.com/spiral/*",
                "github.com/roadrunner-server/*",
                "github.com/temporalio/*",
                "github.com/grpc/*",
                "github.com/uber-go/*",
            ]
        );
    }

    #[test]
    fn builtin_raw_lists_are_clean_and_composer_matches_parsed() {
        // Composer raw list matches the parsed patterns 1:1.
        let composer_raw = builtin_trusted_raw("composer");
        let parsed = builtin_trusted();
        let composer_parsed: Vec<&str> = parsed.iter().map(VendorPattern::as_str).collect();
        assert_eq!(composer_raw, composer_parsed);

        for manager in ["composer", "npm", "go"] {
            let list = builtin_trusted_raw(manager);
            assert!(
                list.iter().all(|e| !e.is_empty()),
                "{manager}: no empty entries"
            );
            let mut sorted = list.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), list.len(), "{manager}: no duplicate entries");
        }

        // Scoped-only posture: every npm entry is a scope wildcard.
        assert!(
            builtin_trusted_raw("npm")
                .iter()
                .all(|e| e.starts_with('@') && e.ends_with("/*"))
        );
        // Go entries are module-path prefixes: at least one path separator.
        assert!(builtin_trusted_raw("go").iter().all(|e| e.contains('/')));
    }

    #[test]
    fn builtin_raw_unknown_manager_is_empty() {
        assert!(builtin_trusted_raw("cargo").is_empty());
        assert!(builtin_trusted_raw("").is_empty());
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

        let (_tmp, ctx) = ctx_with(
            r#"{ "dependencies": { "composer": { "trusted-replace": true } } }"#,
            RunOptions::default(),
        );
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
            r#"{ "dependencies": { "composer": { "trusted": ["acme/pkg"], "trusted-replace": true } } }"#,
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
            r#"{ "dependencies": { "composer": { "trusted": ["evil/*"] } } }"#,
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
    fn undeclared_donors_are_always_admitted_when_trusted() {
        // Discovery is always-on: an undeclared donor no longer needs an
        // opt-in flag. A trusted (here: direct-dependency) undeclared donor is
        // kept unconditionally and flagged `discovered`, with no trailing hint.
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
        assert_eq!(kept_names(&outcome), ["acme/undeclared"]);
        assert!(outcome.kept[0].discovered);
        assert!(outcome.notes.iter().all(|n| n.kind != NoteKind::Hint));
    }

    #[test]
    fn undeclared_untrusted_transitive_is_still_skipped_as_untrusted() {
        // Always-on discovery admits undeclared donors past the declaration
        // gate, but they must still clear the trust list.
        let (_tmp, ctx) = ctx_with("{}", RunOptions::default());
        let outcome = trust_filter(
            &ctx,
            vec![donor(
                "evil/undeclared",
                TrustBasis::Transitive,
                DonorStatus::Undeclared,
            )],
        )
        .unwrap();
        assert!(outcome.kept.is_empty());
        assert_eq!(outcome.skipped[0].reason, SkipReason::Untrusted);
        assert!(outcome.notes.iter().all(|n| n.kind != NoteKind::Hint));
    }

    #[test]
    fn positional_filter_still_restricts_undeclared_donors() {
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
        // Naming trusts + keeps the matched undeclared donor; the unnamed one
        // is filtered out (no discovery hint anymore).
        assert_eq!(kept_names(&outcome), ["acme/undeclared"]);
        assert!(outcome.kept[0].discovered);
        assert!(outcome.notes.iter().all(|n| n.kind != NoteKind::Hint));
        assert!(
            outcome
                .skipped
                .iter()
                .any(|s| s.name.as_str() == "nested/tree" && s.reason == SkipReason::FilteredOut)
        );
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

    // --- npm per-manager trust ----------------------------------------------

    #[test]
    fn npm_project_list_approves_scoped_and_bare() {
        let (_tmp, ctx) = ctx_with(
            r#"{ "dependencies": { "npm": { "trusted": ["@myorg/*", "lodash"] } } }"#,
            RunOptions::default(),
        );
        let outcome = trust_filter(
            &ctx,
            vec![
                npm_donor(
                    "@myorg/thing",
                    TrustBasis::Transitive,
                    DonorStatus::Declared,
                ),
                npm_donor("lodash", TrustBasis::Transitive, DonorStatus::Declared),
                npm_donor("react", TrustBasis::Transitive, DonorStatus::Declared),
            ],
        )
        .unwrap();
        assert_eq!(kept_names(&outcome), ["@myorg/thing", "lodash"]);
        assert_eq!(outcome.kept[0].trust_source, Some(TrustSource::Project));
        assert_eq!(outcome.kept[1].trust_source, Some(TrustSource::Project));
        // The untrusted npm donor gets an npm-worded note (no --trust hint).
        assert!(outcome.notes.iter().any(|n| n.kind == NoteKind::Skip
            && n.message.contains("react")
            && n.message.contains("dependencies.npm.trusted")
            && !n.message.contains("--trust")));
    }

    #[test]
    fn npm_builtin_list_approves_transitive() {
        let (_tmp, ctx) = ctx_with("{}", RunOptions::default());
        let outcome = trust_filter(
            &ctx,
            vec![npm_donor(
                "@anthropic-ai/sdk",
                TrustBasis::Transitive,
                DonorStatus::Declared,
            )],
        )
        .unwrap();
        assert_eq!(kept_names(&outcome), ["@anthropic-ai/sdk"]);
        assert_eq!(outcome.kept[0].trust_source, Some(TrustSource::Builtin));
    }

    #[test]
    fn npm_untrusted_transitive_is_skipped() {
        let (_tmp, ctx) = ctx_with("{}", RunOptions::default());
        let outcome = trust_filter(
            &ctx,
            vec![npm_donor(
                "@evil/payload",
                TrustBasis::Transitive,
                DonorStatus::Declared,
            )],
        )
        .unwrap();
        assert!(outcome.kept.is_empty());
        assert_eq!(outcome.skipped[0].reason, SkipReason::Untrusted);
    }

    #[test]
    fn npm_trusted_replace_drops_builtin() {
        let (_tmp, ctx) = ctx_with(
            r#"{ "dependencies": { "npm": { "trusted": ["@myorg/*"], "trusted-replace": true } } }"#,
            RunOptions::default(),
        );
        let outcome = trust_filter(
            &ctx,
            vec![
                npm_donor(
                    "@anthropic-ai/sdk",
                    TrustBasis::Transitive,
                    DonorStatus::Declared,
                ),
                npm_donor(
                    "@myorg/thing",
                    TrustBasis::Transitive,
                    DonorStatus::Declared,
                ),
            ],
        )
        .unwrap();
        assert_eq!(kept_names(&outcome), ["@myorg/thing"]);
        assert_eq!(outcome.kept[0].trust_source, Some(TrustSource::Project));
    }

    #[test]
    fn npm_direct_dependency_trusted_without_list() {
        let (_tmp, ctx) = ctx_with("{}", RunOptions::default());
        let outcome = trust_filter(
            &ctx,
            vec![npm_donor(
                "@myorg/direct",
                TrustBasis::DirectDependency,
                DonorStatus::Declared,
            )],
        )
        .unwrap();
        assert_eq!(kept_names(&outcome), ["@myorg/direct"]);
        assert_eq!(outcome.kept[0].trust_source, Some(TrustSource::DirectDep));
    }

    #[test]
    fn npm_direct_dependency_dropped_when_npm_replace() {
        let (_tmp, ctx) = ctx_with(
            r#"{ "dependencies": { "npm": { "trusted-replace": true } } }"#,
            RunOptions::default(),
        );
        let outcome = trust_filter(
            &ctx,
            vec![npm_donor(
                "@myorg/direct",
                TrustBasis::DirectDependency,
                DonorStatus::Declared,
            )],
        )
        .unwrap();
        assert!(outcome.kept.is_empty());
        assert_eq!(outcome.skipped[0].reason, SkipReason::Untrusted);
    }

    #[test]
    fn composer_and_npm_lists_do_not_cross_approve() {
        // A composer project pattern shaped like a scoped npm name must not
        // approve an npm donor; an npm scope-wildcard must not approve a
        // composer donor. Each donor is matched only against its own manager.
        let (_tmp, ctx) = ctx_with(
            r#"{ "dependencies": {
                "composer": { "trusted": ["@scope/thing"] },
                "npm": { "trusted": ["@scope/*"] }
            } }"#,
            RunOptions::default(),
        );
        let outcome = trust_filter(
            &ctx,
            vec![
                // npm donor: only the npm list can approve it.
                npm_donor(
                    "@scope/thing",
                    TrustBasis::Transitive,
                    DonorStatus::Declared,
                ),
                // composer donor named like an npm scope pattern: the npm
                // `@scope/*` must NOT approve it; the composer `@scope/thing`
                // (parsed as vendor `@scope`, package `thing`) does.
                donor(
                    "@scope/thing",
                    TrustBasis::Transitive,
                    DonorStatus::Declared,
                ),
                // composer donor the npm list would match if it crossed over.
                donor(
                    "@scope/other",
                    TrustBasis::Transitive,
                    DonorStatus::Declared,
                ),
            ],
        )
        .unwrap();
        // First two kept (npm by npm list, composer by composer exact); the
        // third composer donor is not approved by npm's `@scope/*`.
        assert_eq!(kept_names(&outcome), ["@scope/thing", "@scope/thing"]);
        assert!(
            outcome
                .skipped
                .iter()
                .any(|s| s.name.as_str() == "@scope/other" && s.reason == SkipReason::Untrusted)
        );
    }

    #[test]
    fn cli_trust_does_not_apply_to_npm_donors() {
        // `--trust` is composer-grammar; a scoped npm name cannot be expressed
        // by it, so an npm donor is not approved via the CLI list.
        let (_tmp, ctx) = ctx_with(
            "{}",
            RunOptions {
                trust: patterns(&["scope/thing"]),
                ..Default::default()
            },
        );
        let outcome = trust_filter(
            &ctx,
            vec![npm_donor(
                "@scope/thing",
                TrustBasis::Transitive,
                DonorStatus::Declared,
            )],
        )
        .unwrap();
        assert!(outcome.kept.is_empty());
        assert_eq!(outcome.skipped[0].reason, SkipReason::Untrusted);
    }
}
