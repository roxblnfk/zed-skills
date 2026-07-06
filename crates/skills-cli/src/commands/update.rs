use std::path::Path;

use skills_core::pipeline::ctx::{PrepareOptions, prepare};
use skills_core::pipeline::run_update;

use crate::CliError;
use crate::commands::RawFilters;
use crate::render;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    cwd: &Path,
    dry_run: bool,
    target: Option<String>,
    alias: Vec<String>,
    from: Option<String>,
    refresh: bool,
    re_audit: bool,
    filters: RawFilters,
) -> Result<(), CliError> {
    let mut run = filters.into_run_options(from.is_some())?;
    run.re_audit = re_audit;
    // Passing `--alias` at all is a takeover of the project `aliases` list.
    let alias_override = (!alias.is_empty()).then_some(alias);
    let ctx = prepare(
        cwd,
        PrepareOptions {
            target_override: target,
            alias_override,
            dry_run,
            refresh,
            run,
        },
    )
    .map_err(skills_core::error::PipelineError::from)?;

    let providers = super::providers(from.as_deref())?;
    let locators = super::locators(ctx.discovery_enabled());
    let chain = super::audit_chain(&ctx.manifest)?;
    let report = run_update(&ctx, &providers, &locators, &chain).await?;

    print!("{}", render::render_update(&report));

    // A failed alias leaves the copied target intact but is a config error
    // for the run as a whole (exit 1).
    if report.alias_failed() {
        return Err(CliError::config(
            "one or more aliases could not be created (see the Aliases section above)",
        ));
    }
    Ok(())
}
