pub mod add;
pub mod init;
pub mod show;
pub mod update;

use std::sync::Arc;

use skills_core::traits::{Auditor, SkillLocator, VendorProvider};
use skills_providers::http::{HttpClient, ReqwestClient};
use skills_providers::{
    ComposerDeclaredLocator, DeclaredLocator, DirProvider, GithubProvider, GitlabProvider,
    UrlProvider, WellKnownLocator,
};

use crate::CliError;

/// Shared HTTP client for all remote providers.
pub(crate) fn http_client() -> Result<Arc<dyn HttpClient>, CliError> {
    Ok(Arc::new(ReqwestClient::new().map_err(|e| {
        CliError::config(format!("cannot initialize HTTP client: {e}"))
    })?))
}

/// The full M2 provider set (tokens from `GITHUB_TOKEN` / `GITLAB_TOKEN`),
/// optionally narrowed by `--from=ID`.
pub(crate) fn providers(from: Option<&str>) -> Result<Vec<Arc<dyn VendorProvider>>, CliError> {
    let http = http_client()?;
    let all: Vec<Arc<dyn VendorProvider>> = vec![
        Arc::new(DirProvider),
        Arc::new(GithubProvider::from_env(Arc::clone(&http))),
        Arc::new(GitlabProvider::from_env(Arc::clone(&http))),
        Arc::new(UrlProvider::new(http)),
    ];
    let Some(from) = from else {
        return Ok(all);
    };
    let filtered: Vec<Arc<dyn VendorProvider>> = all
        .into_iter()
        .filter(|p| p.id().as_str() == from)
        .collect();
    if filtered.is_empty() {
        return Err(CliError::config(format!(
            "unknown --from value '{from}' (expected one of: dir, github, gitlab, url)"
        )));
    }
    Ok(filtered)
}

/// Locator chain: composer-declared source → well-known containers →
/// explicit root (local dir donors). RecursiveFallback lands in M3.
pub(crate) fn locators() -> Vec<Arc<dyn SkillLocator>> {
    vec![
        Arc::new(ComposerDeclaredLocator),
        Arc::new(WellKnownLocator),
        Arc::new(DeclaredLocator),
    ]
}

pub(crate) fn auditors() -> Vec<Arc<dyn Auditor>> {
    skills_audit::noop_chain()
}
