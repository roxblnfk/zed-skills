//! In-process execution of the real sync pipeline for the `skills.update`
//! command (the code action's target). Unlike the analysis path this runs
//! with network access and the normal cache — it is exactly `skills update`.

use std::path::Path;
use std::sync::Arc;

use skills_core::pipeline::ctx::{PrepareOptions, prepare};
use skills_core::pipeline::{SyncAction, SyncReport, run_update};
use skills_core::traits::VendorProvider;
use skills_providers::http::{HttpClient, ReqwestClient};
use skills_providers::{
    ComposerProvider, DirProvider, GithubProvider, GitlabProvider, UrlProvider,
};

use crate::analysis::locator_chain;

/// Run the full pipeline (Prepare → … → Sync) against `project_root`.
/// Errors come back as display strings — the server surfaces them via
/// `window/showMessage`.
pub async fn run_real_update(project_root: &Path) -> Result<SyncReport, String> {
    let ctx = prepare(project_root, PrepareOptions::default()).map_err(|e| e.to_string())?;
    let http: Arc<dyn HttpClient> =
        Arc::new(ReqwestClient::new().map_err(|e| format!("cannot initialize HTTP client: {e}"))?);
    let providers: Vec<Arc<dyn VendorProvider>> = vec![
        Arc::new(DirProvider),
        Arc::new(ComposerProvider),
        Arc::new(GithubProvider::from_env(Arc::clone(&http))),
        Arc::new(GitlabProvider::from_env(Arc::clone(&http))),
        Arc::new(UrlProvider::new(http)),
    ];
    let locators = locator_chain();
    let chain = skills_audit::build_chain(&ctx.manifest).map_err(|e| e.to_string())?;
    let report = run_update(&ctx, &providers, &locators, &chain)
        .await
        .map_err(|e| e.to_string())?;
    if report.alias_failed() {
        return Err("one or more aliases could not be created".to_string());
    }
    Ok(report)
}

/// Human-readable outcome line for `window/showMessage`.
pub fn summarize(report: &SyncReport) -> String {
    format!(
        "skills update: {} added, {} updated, {} removed, {} unchanged",
        report.count(SyncAction::Add),
        report.count(SyncAction::Update),
        report.count(SyncAction::Remove),
        report.count(SyncAction::Skip),
    )
}
