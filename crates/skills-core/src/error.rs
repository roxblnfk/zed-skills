//! Typed errors, one per pipeline stage.

use std::io;
use std::path::PathBuf;

use thiserror::Error;

use crate::domain::{ProviderId, SkillId, VendorName};

/// Errors reading or validating `skills.json`.
#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("skills.json not found at {path} (run `skills init` to create one)")]
    NotFound { path: PathBuf },
    #[error("skills.json: failed to read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("skills.json: {0}")]
    Parse(#[source] serde_json::Error),
    #[error("skills.json: {0}")]
    Invalid(String),
}

/// Errors reading or writing `skills.lock`.
#[derive(Debug, Error)]
pub enum LockfileError {
    #[error("skills.lock: failed to access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("skills.lock: {0}")]
    Parse(#[source] serde_json::Error),
}

/// Prepare stage failure.
#[derive(Debug, Error)]
pub enum PrepareError {
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error(transparent)]
    Lockfile(#[from] LockfileError),
    #[error("invalid --target value: {0}")]
    InvalidTarget(String),
    #[error("invalid alias: {0}")]
    InvalidAlias(String),
}

/// Discover stage failure.
#[derive(Debug, Error)]
pub enum DiscoverError {
    #[error("provider {provider}: {message}")]
    Provider {
        provider: ProviderId,
        message: String,
    },
    #[error("provider {provider}: io error at {path}: {source}")]
    Io {
        provider: ProviderId,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// TrustFilter stage failure (usage errors around positional filters).
#[derive(Debug, Error)]
pub enum TrustError {
    #[error("no installed donor package matches: {patterns}")]
    NoPackageMatch { patterns: String },
}

/// Materialize stage failure.
#[derive(Debug, Error)]
pub enum MaterializeError {
    #[error("vendor {vendor}: {message}")]
    Vendor { vendor: VendorName, message: String },
    #[error("vendor {vendor}: io error at {path}: {source}")]
    Io {
        vendor: VendorName,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Offline (cache-only) materialization found no usable cache entry.
    /// Not a failure of the vendor — it just has not been downloaded yet.
    #[error("vendor {vendor}: not fetched yet — run `skills update`")]
    NotFetched { vendor: VendorName },
    #[error("materialize task panicked: {0}")]
    Task(String),
}

/// Locate+Scan stage failure.
#[derive(Debug, Error)]
pub enum ScanError {
    #[error("vendor {vendor}: no skill locator applies")]
    NoLocator { vendor: VendorName },
    #[error("vendor {vendor}: io error at {path}: {source}")]
    Io {
        vendor: VendorName,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("scan task panicked: {0}")]
    Task(String),
}

/// A single dir-name collision between donors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub id: SkillId,
    pub vendors: Vec<VendorName>,
}

/// Resolve stage failure (the pipeline barrier).
#[derive(Debug, Error)]
pub enum ResolveError {
    #[error("skill name conflict: {}", format_conflicts(.0))]
    Conflict(Vec<Conflict>),
}

fn format_conflicts(conflicts: &[Conflict]) -> String {
    conflicts
        .iter()
        .map(|c| {
            let vendors = c
                .vendors
                .iter()
                .map(VendorName::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            format!("'{}' provided by [{vendors}]", c.id)
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Audit stage failure.
#[derive(Debug, Error)]
pub enum AuditError {
    #[error("auditor {auditor}: {message}")]
    Auditor { auditor: String, message: String },
    #[error("audit blocked skill '{skill}': {reason}")]
    Blocked { skill: SkillId, reason: String },
}

/// Sync stage failure.
#[derive(Debug, Error)]
pub enum SyncError {
    #[error("sync: io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Lockfile(#[from] LockfileError),
}

/// Aggregate error for a full pipeline run; the CLI maps this to exit codes.
#[derive(Debug, Error)]
pub enum PipelineError {
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Discover(#[from] DiscoverError),
    #[error(transparent)]
    Trust(#[from] TrustError),
    #[error(transparent)]
    Materialize(#[from] MaterializeError),
    #[error(transparent)]
    Scan(#[from] ScanError),
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    #[error(transparent)]
    Audit(#[from] AuditError),
    #[error(transparent)]
    Sync(#[from] SyncError),
}
