//! Vendor providers for the `skills` CLI.
//!
//! M1 shipped [`dir::DirProvider`]; M2 added the remote providers
//! ([`github::GithubProvider`], [`gitlab::GitlabProvider`],
//! [`url::UrlProvider`]) with a project-local archive cache; M3 adds the
//! composer local provider ([`composer::ComposerProvider`]) and completes
//! the locator chain with the discovery-gated
//! [`locate::RecursiveFallbackLocator`].

pub mod addparse;
pub mod archive;
pub mod cachepath;
pub mod composer;
pub mod dir;
pub mod github;
pub mod gitlab;
pub mod http;
pub mod locate;
pub mod refresolver;
mod remote;
pub mod testkit;
pub mod treescan;
pub mod url;

pub use addparse::{ParsedAdd, parse_add_input};
pub use composer::{ComposerProvider, ComposerVendor};
pub use dir::{DirProvider, DirVendor};
pub use github::{GithubProvider, GithubVendor};
pub use gitlab::{GitlabProvider, GitlabVendor};
pub use http::{HttpClient, HttpError, HttpResponse, MockHttp, ReqwestClient};
pub use locate::{
    ComposerDeclaredLocator, DeclaredLocator, RecursiveFallbackLocator, WellKnownLocator,
};
pub use url::{UrlProvider, UrlVendor};
