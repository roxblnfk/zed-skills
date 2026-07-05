//! Core domain, manifest/lockfile formats, trait contracts and the sync
//! pipeline for the `skills` CLI. No network access and no CLI dependencies.

pub mod audit;
pub mod domain;
pub mod error;
pub mod frontmatter;
pub mod fsutil;
pub mod lockfile;
pub mod manifest;
pub mod paths;
pub mod pipeline;
pub mod traits;

pub use audit::{AuditReport, AuditedSkill, AuditorId, Finding, Severity};
pub use domain::{
    MaterializedVendor, Note, NoteKind, Origin, ProviderId, ResolvedSkill, ScannedSkill, SkillId,
    SkillsFilter, SkillsRoot, VendorName, VendorRef,
};
pub use error::{
    DiscoverError, LockfileError, ManifestError, MaterializeError, PipelineError, PrepareError,
    ResolveError, ScanError, SyncError,
};
pub use lockfile::{LockedSkill, Lockfile, SyncStatus};
pub use manifest::{AuditMode, DEFAULT_TARGET, MANIFEST_NAME, Manifest};
pub use pipeline::ctx::{Ctx, PrepareOptions};
pub use traits::{Auditor, Cache, Located, SkillLocator, Vendor, VendorProvider};
