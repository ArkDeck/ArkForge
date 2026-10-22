//! Message codec for `api/arkforge.proto`.
//!
//! Only the AF-V1 surface is encoded here. The execute-side payloads exist in
//! the schema so the contract is complete, but this build answers those calls
//! with `STATUS_UNAVAILABLE` and never needs to decode their bodies.

use crate::wire::{self, Reader, WireError, decode_enum};
use crate::{Api, SessionKind, Status};
use arkforge_core::digest::{
    CanonicalCbor, CborError, CborValue, Domain, Sha256Digest, decode_canonical, digest_canonical,
};
use arkforge_core::ids::OpaqueId;
use std::collections::BTreeMap;

#[path = "parts/jobs.rs"]
mod jobs;
#[path = "parts/permits.rs"]
mod permits;
#[path = "parts/plan.rs"]
mod plan;
#[path = "parts/04_receipt_digest.rs"]
mod receipt_digest;
#[path = "parts/session.rs"]
mod session;
#[cfg(test)]
#[path = "parts/tests.rs"]
mod tests;

pub use jobs::*;
pub use permits::*;
pub use plan::*;
pub use receipt_digest::*;
pub use session::*;
