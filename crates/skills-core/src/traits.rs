//! Trait contracts between the pipeline and its pluggable pieces.

use std::path::PathBuf;

use async_trait::async_trait;

use crate::audit::{AuditReport, AuditorId};
use crate::domain::{
    MaterializedVendor, Origin, ProviderId, ResolvedSkill, SkillsRoot, VendorName, VendorRef,
};
use crate::error::{AuditError, DiscoverError, MaterializeError, ScanError};
use crate::pipeline::ctx::Ctx;

/// Local cache used by remote vendors during materialization. Local vendors
/// ignore it. The directory is created lazily by whoever needs it.
#[derive(Debug, Clone)]
pub struct Cache {
    pub root: PathBuf,
    /// `skills update --refresh`: delete matching cache entries before
    /// materializing (forces re-download; there is no TTL otherwise).
    pub refresh: bool,
    /// Cache-only materialization (no network): a remote vendor missing from
    /// the cache yields [`crate::error::MaterializeError::NotFetched`]
    /// instead of downloading. Used by analysis frontends (the LSP server).
    pub offline: bool,
}

impl Cache {
    pub fn new(root: PathBuf) -> Self {
        Cache {
            root,
            refresh: false,
            offline: false,
        }
    }
}

/// Discovers donor references from the project context.
#[async_trait]
pub trait VendorProvider: Send + Sync {
    fn id(&self) -> ProviderId;
    async fn discover(&self, ctx: &Ctx) -> Result<Vec<VendorRef>, DiscoverError>;
}

/// A single donor. After `materialize()` local and remote vendors are
/// indistinguishable: both are just a directory on disk.
#[async_trait]
pub trait Vendor: Send + Sync {
    fn name(&self) -> &VendorName;
    fn origin(&self) -> &Origin;
    async fn materialize(&self, cache: &Cache) -> Result<MaterializedVendor, MaterializeError>;
}

/// Result of a locator attempt: either it found skills roots or it does not
/// apply to this vendor (the chain tries the next locator).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Located {
    Found(Vec<SkillsRoot>),
    NotApplicable,
}

/// Finds skills roots inside a materialized vendor.
pub trait SkillLocator: Send + Sync {
    fn locate(&self, vendor: &MaterializedVendor) -> Result<Located, ScanError>;
}

/// One auditor in the configurable audit chain.
#[async_trait]
pub trait Auditor: Send + Sync {
    fn id(&self) -> AuditorId;
    async fn audit(&self, skill: &ResolvedSkill) -> Result<AuditReport, AuditError>;
}
