//! Message codec for `api/arkforge.proto`.
//!
//! Only the AF-V1 surface is encoded here. The execute-side payloads exist in
//! the schema so the contract is complete, but this build answers those calls
//! with `STATUS_UNAVAILABLE` and never needs to decode their bodies.

include!("parts/session.rs");
include!("parts/plan.rs");
include!("parts/jobs.rs");
include!("parts/04_receipt_digest.rs");
include!("parts/permits.rs");
include!("parts/tests.rs");
