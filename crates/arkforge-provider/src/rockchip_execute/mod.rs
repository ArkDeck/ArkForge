//! Native RockUSB execution for the DAYU200 Loader path.
//!
//! Stored actions carry typed semantics only. The executor validates the
//! Profile, observed partition table, staged artifact and read domain before
//! calling the native [`RockUsbPort`]. No caller-controlled subprocess or argv
//! surface exists; macOS file validation uses one fixed system SHA-256 command
//! fed only an already-open descriptor.

include!("parts/device.rs");
include!("parts/actions.rs");
include!("parts/03_execute_action.rs");
include!("parts/04_readback_partition.rs");
include!("parts/errors.rs");
include!("parts/tests.rs");
