pub mod add;
pub mod init;
pub mod lsp;
pub mod show;
pub mod update;

use std::sync::Arc;

use skills_core::pattern::VendorPattern;
use skills_core::pipeline::ChainEntry;
use skills_core::pipeline::ctx::RunOptions;
use skills_core::traits::{SkillLocator, VendorProvider};
use skills_providers::http::{HttpClient, ReqwestClient};
use skills_providers::{
    ComposerDeclaredLocator, ComposerProvider, DeclaredLocator, DirProvider, GithubProvider,
    GitlabProvider, RecursiveFallbackLocator, UrlProvider, WellKnownLocator,
};

use crate::CliError;

/// Raw (unparsed) per-invocation filters shared by `update` and `show`.
pub(crate) struct RawFilters {
    /// Positional `PACKAGE` / `VENDOR/*` arguments.
    pub packages: Vec<String>,
    /// `--trust=PATTERN` values.
    pub trust: Vec<String>,
    /// `--discovery` flag.
    pub discovery: bool,
}

impl RawFilters {
    /// Parse into pipeline [`RunOptions`]; pattern syntax errors are usage
    /// errors (exit 1).
    pub fn into_run_options(self, scoped: bool) -> Result<RunOptions, CliError> {
        Ok(RunOptions {
            packages: parse_patterns(&self.packages, "package argument")?,
            trust: parse_patterns(&self.trust, "--trust value")?,
            discovery: self.discovery.then_some(true),
            scoped,
            re_audit: false,
        })
    }
}

fn parse_patterns(raw: &[String], what: &str) -> Result<Vec<VendorPattern>, CliError> {
    raw.iter()
        .map(|p| {
            VendorPattern::parse(p).map_err(|reason| CliError::config(format!("{what}: {reason}")))
        })
        .collect()
}

/// Shared HTTP client for all remote providers.
pub(crate) fn http_client() -> Result<Arc<dyn HttpClient>, CliError> {
    Ok(Arc::new(ReqwestClient::new().map_err(|e| {
        CliError::config(format!("cannot initialize HTTP client: {e}"))
    })?))
}

/// The full provider set (tokens from `GITHUB_TOKEN` / `GITLAB_TOKEN`),
/// optionally narrowed by `--from=ID`.
pub(crate) fn providers(from: Option<&str>) -> Result<Vec<Arc<dyn VendorProvider>>, CliError> {
    let http = http_client()?;
    let all: Vec<Arc<dyn VendorProvider>> = vec![
        Arc::new(DirProvider),
        Arc::new(ComposerProvider),
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
            "unknown --from value '{from}' (expected one of: dir, composer, github, gitlab, url)"
        )));
    }
    Ok(filtered)
}

/// Locator chain: composer-declared source → well-known containers →
/// recursive fallback (discovery-gated) → explicit root (local dir donors).
pub(crate) fn locators(discovery: bool) -> Vec<Arc<dyn SkillLocator>> {
    vec![
        Arc::new(ComposerDeclaredLocator),
        Arc::new(WellKnownLocator),
        Arc::new(RecursiveFallbackLocator::new(discovery)),
        Arc::new(DeclaredLocator),
    ]
}

/// Build the audit chain from the manifest (`audit.pipeline`); referencing a
/// not-yet-implemented auditor is a config error (exit 1).
pub(crate) fn audit_chain(
    manifest: &skills_core::manifest::Manifest,
) -> Result<Vec<ChainEntry>, CliError> {
    skills_audit::build_chain(manifest).map_err(|e| CliError::config(e.to_string()))
}
