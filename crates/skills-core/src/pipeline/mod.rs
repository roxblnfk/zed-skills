//! The typed sync pipeline.
//!
//! ```text
//! Prepare → Discover → TrustFilter → Materialize → Locate+Scan
//!         → Resolve (barrier) → Audit → Plan → Sync (transactional)
//! ```
//!
//! Every stage is an independently testable function; only Sync writes to
//! the project, so any failure before it leaves the filesystem untouched.

pub mod audit;
pub mod ctx;
pub mod discover;
pub mod materialize;
pub mod plan;
pub mod resolve;
pub mod scan;
pub mod sync;
pub mod trust;

use std::sync::Arc;

use crate::error::PipelineError;
use crate::traits::{Auditor, SkillLocator, VendorProvider};

pub use ctx::{Ctx, PrepareOptions, prepare};
pub use plan::SyncPlan;
pub use resolve::Resolution;
pub use sync::{SyncAction, SyncEntry, SyncReport};

/// Run the full pipeline after Prepare. Honors `ctx.dry_run` (full pipeline
/// including conflict detection, zero writes).
pub async fn run_update(
    ctx: &Ctx,
    providers: &[Arc<dyn VendorProvider>],
    locators: &[Arc<dyn SkillLocator>],
    auditors: &[Arc<dyn Auditor>],
) -> Result<SyncReport, PipelineError> {
    let vendor_refs = discover::discover(ctx, providers).await?;
    let vendor_refs = trust::trust_filter(ctx, vendor_refs);
    let vendors = materialize::materialize_all(ctx, vendor_refs).await?;
    let scanned = scan::locate_and_scan(&vendors, locators).await?;
    let resolution = resolve::resolve(scanned, &vendors)?;
    let audited = audit::audit_all(resolution.skills, auditors, ctx.manifest.audit_mode()).await?;
    let sync_plan = plan::plan(&ctx.lockfile, &audited);
    let report = sync::sync(ctx, sync_plan, resolution.notes)?;
    Ok(report)
}
