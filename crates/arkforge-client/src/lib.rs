//! Typed clients for the versioned local ArkForge protocol.
//!
//! This crate is intentionally below every product authority. It knows how to
//! negotiate, frame and decode ArkForge IPC, but it cannot decide whether a
//! destructive step is authorized and does not depend on `arkforged`.

#![forbid(unsafe_code)]

mod controller;
mod public;

pub use controller::{ControllerClient, MaterializeInput};
pub use public::{
    ClientError, DeviceObservationView, DeviceProbeView, PublicClient, PublicClientError,
    PublicRuntimeInfo, RecoveryGuideView,
};
