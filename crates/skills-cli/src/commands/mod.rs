pub mod init;
pub mod show;
pub mod update;

use std::sync::Arc;

use skills_core::traits::{Auditor, SkillLocator, VendorProvider};
use skills_providers::{DeclaredLocator, DirProvider};

/// The M1 wiring: local dir donors, explicit-root locator, no-op audit.
pub(crate) fn providers() -> Vec<Arc<dyn VendorProvider>> {
    vec![Arc::new(DirProvider)]
}

pub(crate) fn locators() -> Vec<Arc<dyn SkillLocator>> {
    vec![Arc::new(DeclaredLocator)]
}

pub(crate) fn auditors() -> Vec<Arc<dyn Auditor>> {
    skills_audit::noop_chain()
}
