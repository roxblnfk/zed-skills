//! Vendor providers for the `skills` CLI.
//!
//! M1 shipped [`dir::DirProvider`]; M2 adds the remote providers
//! ([`github::GithubProvider`], [`gitlab::GitlabProvider`],
//! [`url::UrlProvider`]) with a project-local archive cache, plus the
//! composer-declared and well-known skill locators. The composer *local*
//! provider lands in M3.

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
pub mod url;

pub use addparse::{ParsedAdd, parse_add_input};
pub use dir::{DirProvider, DirVendor};
pub use github::{GithubProvider, GithubVendor};
pub use gitlab::{GitlabProvider, GitlabVendor};
pub use http::{HttpClient, HttpError, HttpResponse, MockHttp, ReqwestClient};
pub use locate::{ComposerDeclaredLocator, DeclaredLocator, WellKnownLocator};
pub use url::{UrlProvider, UrlVendor};
