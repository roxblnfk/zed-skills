use std::path::Path;

use skills_core::error::PipelineError;
use skills_core::lockfile::{SyncStatus, sync_status};
use skills_core::paths::rel_to_path;
use skills_core::pipeline::ctx::{PrepareOptions, prepare};
use skills_core::pipeline::{discover, materialize, scan, trust};

use crate::CliError;
use crate::render::{self, ShowLine, ShowVendor};

/// Read-only report: donors, their skills, sync status against the lockfile.
pub async fn run(cwd: &Path) -> Result<(), CliError> {
    let ctx = prepare(cwd, PrepareOptions::default()).map_err(PipelineError::from)?;

    let refs = discover::discover(&ctx, &super::providers())
        .await
        .map_err(PipelineError::from)?;
    let refs = trust::trust_filter(&ctx, refs);
    let vendors = materialize::materialize_all(&ctx, refs)
        .await
        .map_err(PipelineError::from)?;
    let scanned = scan::locate_and_scan(&vendors, &super::locators())
        .await
        .map_err(PipelineError::from)?;

    let mut groups: Vec<ShowVendor> = Vec::new();
    for vendor in &vendors {
        let mut lines = Vec::new();
        for skill in scanned.iter().filter(|s| s.vendor == vendor.name) {
            let status = match ctx.lockfile.find(&skill.id) {
                None => SyncStatus::NotSynced,
                Some(locked) => {
                    sync_status(&ctx.target_abs.join(rel_to_path(skill.id.as_str())), locked)
                }
            };
            lines.push(ShowLine {
                id: skill.id.as_str().to_string(),
                description: skill.description.clone(),
                status,
            });
        }
        groups.push(ShowVendor {
            name: vendor.name.as_str().to_string(),
            lines,
        });
    }

    print!("{}", render::render_show(&ctx.target_rel, &groups));
    Ok(())
}
