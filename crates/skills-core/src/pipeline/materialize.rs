//! Stage 4 — Materialize: turn vendor references into directories on disk.
//!
//! Runs concurrently with a bounded semaphore (remote vendors download
//! archives; local vendors are no-ops).

use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::domain::{MaterializedVendor, VendorRef};
use crate::error::MaterializeError;
use crate::pipeline::ctx::Ctx;

/// Maximum number of vendors materialized at once.
const CONCURRENCY: usize = 4;

pub async fn materialize_all(
    ctx: &Ctx,
    vendors: Vec<VendorRef>,
) -> Result<Vec<MaterializedVendor>, MaterializeError> {
    let semaphore = Arc::new(Semaphore::new(CONCURRENCY));
    let mut join_set: JoinSet<Result<(usize, MaterializedVendor), MaterializeError>> =
        JoinSet::new();

    for (idx, vendor_ref) in vendors.into_iter().enumerate() {
        let semaphore = Arc::clone(&semaphore);
        let cache = ctx.cache.clone();
        join_set.spawn(async move {
            let _permit = semaphore
                .acquire_owned()
                .await
                .expect("semaphore never closed");
            let mut mv = vendor_ref.vendor.materialize(&cache).await?;
            // The manifest-declared allowlist belongs to the reference, not
            // to the vendor implementation.
            mv.filter = vendor_ref.filter.clone();
            Ok((idx, mv))
        });
    }

    let mut results: Vec<(usize, MaterializedVendor)> = Vec::new();
    while let Some(joined) = join_set.join_next().await {
        let (idx, mv) = joined.map_err(|e| MaterializeError::Task(e.to_string()))??;
        results.push((idx, mv));
    }
    // Restore input order regardless of completion order.
    results.sort_by_key(|(idx, _)| *idx);
    Ok(results.into_iter().map(|(_, mv)| mv).collect())
}
