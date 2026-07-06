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
pub mod pattern;
pub mod pipeline;
pub mod traits;

pub use audit::{
    AuditFinding, AuditReport, AuditedSkill, AuditorId, Finding, Severity, auditor_set_hash,
};
pub use domain::{
    DonorStatus, MaterializedVendor, Note, NoteKind, Origin, ProviderId, ResolvedSkill,
    ScannedSkill, SkillId, SkillsFilter, SkillsRoot, SourceHint, TrustBasis, VendorName, VendorRef,
};
pub use error::{
    DiscoverError, LockfileError, ManifestError, MaterializeError, PipelineError, PrepareError,
    ResolveError, ScanError, SyncError, TrustError,
};
pub use lockfile::{AuditCacheEntry, LockedSkill, Lockfile, SyncStatus};
pub use manifest::{
    AuditConfig, AuditMode, AuditStep, DEFAULT_TARGET, HttpStep, LlmStep, MANIFEST_NAME, Manifest,
    OnFail, StaticStep,
};
pub use pattern::VendorPattern;
pub use pipeline::ChainEntry;
pub use pipeline::ctx::{Ctx, PrepareOptions};
pub use traits::{Auditor, Cache, Located, SkillLocator, Vendor, VendorProvider};
