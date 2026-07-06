use std::path::Path;

use skills_core::pipeline::ctx::{PrepareOptions, prepare};
use skills_core::pipeline::run_update;

use crate::CliError;
use crate::render;

pub async fn run(
    cwd: &Path,
    dry_run: bool,
    target: Option<String>,
    from: Option<String>,
    refresh: bool,
) -> Result<(), CliError> {
    let ctx = prepare(
        cwd,
        PrepareOptions {
            target_override: target,
            dry_run,
            refresh,
        },
    )
    .map_err(skills_core::error::PipelineError::from)?;

    let providers = super::providers(from.as_deref())?;
    let report = run_update(&ctx, &providers, &super::locators(), &super::auditors()).await?;

    print!("{}", render::render_update(&report));
    Ok(())
}
