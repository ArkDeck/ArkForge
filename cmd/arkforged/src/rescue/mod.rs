//! Explicit native RockUSB rescue.
//!
//! Rescue is deliberately separate from normal [`crate::jobs`] execution. It
//! has no ArkDeck authority, does not mint a `FlashPlan`, and cannot produce a
//! normal action receipt. Its safety contract is instead:
//!
//! - inspect one exact Loader observation;
//! - seal device, profile, layout, image and native-build facts into a plan;
//! - require the exact plan digest and acknowledgement set at apply;
//! - durably record a single-use intent before mutating USB I/O;
//! - never replay a plan after an interrupted or unknown outcome.

include!("parts/types.rs");
include!("parts/plan_receipt.rs");
include!("parts/03_manager_read.rs");
include!("parts/04_manager_plan.rs");
include!("parts/05_manager_apply.rs");
include!("parts/06_manager_store.rs");
include!("parts/07_validate_acknowledgements.rs");
include!("parts/tests.rs");
