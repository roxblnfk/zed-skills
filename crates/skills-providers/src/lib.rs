//! Vendor providers for the `skills` CLI.
//!
//! M1 ships only [`dir::DirProvider`]. The other modules are structural
//! stubs receiving their implementations in later milestones.

pub mod composer;
pub mod dir;
pub mod github;
pub mod gitlab;
pub mod http;
pub mod locate;
pub mod testkit;

pub use dir::{DirProvider, DirVendor};
pub use locate::DeclaredLocator;
