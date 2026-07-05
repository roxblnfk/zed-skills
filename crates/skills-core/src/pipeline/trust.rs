//! Stage 3 — TrustFilter.
//!
//! M1: pass-through. The trust model (builtin list, `trusted` patterns,
//! `--trust`, direct-dependency grants) lands in M3, but the stage slot is
//! kept so the pipeline shape does not change.

use crate::domain::VendorRef;
use crate::pipeline::ctx::Ctx;

pub fn trust_filter(_ctx: &Ctx, vendors: Vec<VendorRef>) -> Vec<VendorRef> {
    vendors
}
