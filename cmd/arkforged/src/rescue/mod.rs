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

use crate::dispatch::{NativeRockUsbPort, device_from_descriptor};
use arkforge_artifact::cas::{CasQuota, ContentAddressedStore};
use arkforge_artifact::manifest::PartitionTableFact;
use arkforge_core::digest::{
    CanonicalCbor, CborValue, Domain, Sha256Digest, decode_canonical, digest_in_domain, sha256,
};
use arkforge_core::profile::DeviceProfile;
use arkforge_platform::{protect_path, sync_directory};
use arkforge_provider::rockchip_execute::{
    ROCKUSB_SECTOR_BYTES, RockUsbDevice, RockUsbLocation, RockUsbMutationReceipt,
    RockUsbObservation, RockUsbPort, RockUsbPortFailure, StagedImage, observed_layout_digest,
    validate_partition_table_for_profile,
};
use core::fmt;
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[path = "parts/05_manager_apply.rs"]
mod manager_apply;
#[path = "parts/04_manager_plan.rs"]
mod manager_plan;
#[path = "parts/03_manager_read.rs"]
mod manager_read;
#[path = "parts/06_manager_store.rs"]
mod manager_store;
#[path = "parts/plan_receipt.rs"]
mod plan_receipt;
#[cfg(test)]
#[path = "parts/tests.rs"]
mod tests;
#[path = "parts/types.rs"]
mod types;
#[path = "parts/07_validate_acknowledgements.rs"]
mod validate_acknowledgements;

pub use plan_receipt::*;
pub use types::*;
pub use validate_acknowledgements::*;
