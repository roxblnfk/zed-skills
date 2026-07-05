//! Stage 2 — Discover: ask every provider for donor references.

use std::sync::Arc;

use crate::domain::VendorRef;
use crate::error::DiscoverError;
use crate::pipeline::ctx::Ctx;
use crate::traits::VendorProvider;

/// Run all providers in order and concatenate their vendor references.
///
/// Providers run sequentially: M1 has a single local provider; concurrent
/// discovery becomes interesting with network providers in M2.
pub async fn discover(
    ctx: &Ctx,
    providers: &[Arc<dyn VendorProvider>],
) -> Result<Vec<VendorRef>, DiscoverError> {
    let mut out = Vec::new();
    for provider in providers {
        out.extend(provider.discover(ctx).await?);
    }
    Ok(out)
}
