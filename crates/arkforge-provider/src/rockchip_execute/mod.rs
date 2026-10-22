//! Native RockUSB execution for the DAYU200 Loader path.
//!
//! Stored actions carry typed semantics only. The executor validates the
//! Profile, observed partition table, staged artifact and read domain before
//! calling the native [`RockUsbPort`]. No caller-controlled subprocess or argv
//! surface exists; macOS file validation uses one fixed system SHA-256 command
//! fed only an already-open descriptor.

use arkforge_artifact::manifest::PartitionTableFact;
use arkforge_core::Sha256Digest;
use arkforge_core::digest::{CborValue, Sha256, sha256};
use arkforge_core::effect::ByteRange;
use arkforge_core::ids::{ActionId, OpaqueId, StepId};
use arkforge_core::outcome::ActionDisposition;
use arkforge_core::profile::DeviceProfile;
use arkforge_core::projection::PrivateActionRecord;
use arkforge_core::verification::{
    FailureClassification, TypedSkipReason, VerificationOutcome, VerificationStrength,
};
use core::fmt;
use std::collections::BTreeMap;
use std::fs::File;
#[cfg(not(target_os = "macos"))]
use std::io::Read;
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};
use std::thread::JoinHandle;
use std::time::Instant;

#[path = "parts/actions.rs"]
mod actions;
#[path = "parts/device.rs"]
mod device;
#[path = "parts/errors.rs"]
mod errors;
#[path = "parts/03_execute_action.rs"]
mod execute_action;
#[path = "parts/04_readback_partition.rs"]
mod readback_partition;
#[cfg(test)]
#[path = "parts/tests.rs"]
mod tests;

pub use actions::*;
pub use device::*;
pub use execute_action::*;
pub use readback_partition::*;
