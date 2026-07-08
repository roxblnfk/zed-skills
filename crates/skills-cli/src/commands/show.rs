use std::collections::BTreeMap;
use std::path::Path;

use skills_core::domain::{NoteKind, TrustBasis};
use skills_core::error::PipelineError;
use skills_core::lockfile::{SyncStatus, sync_status};
use skills_core::paths::rel_to_path;
use skills_core::pipeline::ctx::{PrepareOptions, prepare};
use skills_core::pipeline::{SkipReason, TrustSource, discover, materialize, scan, trust};

use crate::CliError;
use crate::commands::RawFilters;
use crate::render::{self, ShowLine, ShowSkipped, ShowVendor};

/// Read-only report: donors with trust annotations, their skills and sync
/// status against the lockfile, skipped donors with reasons.
pub async fn run(cwd: &Path, from: Option<String>, filters: RawFilters) -> Result<(), CliError> {
    let run = filters.into_run_options(from.is_some())?;
    let ctx = prepare(
        cwd,
        PrepareOptions {
            run,
            ..Default::default()
        },
    )
    .map_err(PipelineError::from)?;

    let providers = super::providers(from.as_deref())?;
    let refs = discover::discover(&ctx, &providers)
        .await
        .map_err(PipelineError::from)?;
    let outcome = trust::trust_filter(&ctx, refs).map_err(PipelineError::from)?;

    // Annotations per kept vendor: trust source + discovered mark.
    let mut annotations: BTreeMap<String, Vec<&'static str>> = BTreeMap::new();
    for kept in &outcome.kept {
        let mut chips = Vec::new();
        match kept.trust_source {
            Some(TrustSource::Builtin) => chips.push("[builtin]"),
            Some(TrustSource::DirectDep) => chips.push("[direct-dep]"),
            // Project/CLI-trusted donors are not annotated: the user's own
            // explicit decision needs no callout.
            Some(TrustSource::Project) | Some(TrustSource::Cli) | None => {}
        }
        // A `sources[]` donor is trusted because it is declared in
        // skills.json — labeled so a coincidentally-matching trust-list entry
        // never takes silent credit for it.
        if kept.vendor_ref.trust == TrustBasis::UserDeclared {
            chips.push("[declared in skills.json]");
        }
        if kept.discovered {
            chips.push("[discovered]");
        }
        annotations.insert(kept.vendor_ref.name.as_str().to_string(), chips);
    }

    let kept_refs: Vec<_> = outcome.kept.iter().map(|k| k.vendor_ref.clone()).collect();
    let vendors = materialize::materialize_all(&ctx, kept_refs)
        .await
        .map_err(PipelineError::from)?;
    let scanned = scan::locate_and_scan(&vendors, &super::locators(ctx.discovery_enabled()))
        .await
        .map_err(PipelineError::from)?;

    let mut groups: Vec<ShowVendor> = Vec::new();
    for vendor in &vendors {
        let mut lines = Vec::new();
        for skill in scanned.iter().filter(|s| s.vendor == vendor.name) {
            let locked = ctx.lockfile.find(&skill.id);
            let status = match locked {
                None => SyncStatus::NotSynced,
                Some(locked) => {
                    sync_status(&ctx.target_abs.join(rel_to_path(skill.id.as_str())), locked)
                }
            };
            // Cached verdict from the lockfile; passing verdicts stay quiet.
            let audit = locked
                .and_then(|l| l.audit.as_ref())
                .filter(|a| a.verdict != "pass")
                .map(|a| a.verdict.clone());
            lines.push(ShowLine {
                id: skill.id.as_str().to_string(),
                description: skill.description.clone(),
                status,
                audit,
            });
        }
        groups.push(ShowVendor {
            name: vendor.name.as_str().to_string(),
            annotations: annotations
                .get(vendor.name.as_str())
                .map(|chips| chips.iter().map(|c| c.to_string()).collect())
                .unwrap_or_default(),
            lines,
        });
    }

    let skipped: Vec<ShowSkipped> = outcome
        .skipped
        .iter()
        .map(|s| ShowSkipped {
            name: s.name.as_str().to_string(),
            reason: match &s.reason {
                SkipReason::Untrusted => "untrusted".to_string(),
                SkipReason::Malformed(detail) => format!("malformed: {detail}"),
                SkipReason::FilteredOut => "filtered-out".to_string(),
                SkipReason::NotDeclared => "not-declared".to_string(),
            },
        })
        .collect();

    // Malformed details already sit in the Skipped section; only the
    // trailing hints carry over to show.
    let hints: Vec<_> = outcome
        .notes
        .into_iter()
        .filter(|n| n.kind == NoteKind::Hint)
        .collect();

    print!(
        "{}",
        render::render_show(&ctx.target_rel, &groups, &skipped, &hints)
    );
    Ok(())
}
