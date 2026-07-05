use std::path::Path;

use skills_core::pipeline::ctx::{PrepareOptions, prepare};
use skills_core::pipeline::run_update;

use crate::CliError;
use crate::render;

pub async fn run(cwd: &Path, dry_run: bool, target: Option<String>) -> Result<(), CliError> {
    let ctx = prepare(
        cwd,
        PrepareOptions {
            target_override: target,
            dry_run,
        },
    )
    .map_err(skills_core::error::PipelineError::from)?;

    let report = run_update(
        &ctx,
        &super::providers(),
        &super::locators(),
        &super::auditors(),
    )
    .await?;

    print!("{}", render::render_update(&report));
    Ok(())
}
