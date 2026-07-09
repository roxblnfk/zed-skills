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
    check: bool,
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
            // --check is a dry run with a compact report and a status exit
            // code: the full pipeline runs up to and including Plan (normal
            // cache/network semantics so remote drift is caught), Sync never
            // writes. Conflicts and audit blocks abort the same way (2 / 3).
            dry_run: dry_run || check,
            refresh,
            run,
        },
    )
    .map_err(skills_core::error::PipelineError::from)?;

    // One-shot deprecation hint: the manifest still relies on the legacy
    // `remote` key (never emitted when `sources` is used).
    if ctx.manifest.uses_deprecated_remote() {
        eprintln!("warning: skills.json uses the deprecated 'remote' key; rename it to 'sources'");
    }

    let providers = super::providers(from.as_deref())?;
    let locators = super::locators();
    let chain = super::audit_chain(&ctx.manifest)?;
    let report = run_update(&ctx, &providers, &locators, &chain).await?;

    if check {
        print!("{}", render::render_check(&report));
        if render::check_pending(&report) {
            return Err(CliError::changes_pending());
        }
        return Ok(());
    }

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
