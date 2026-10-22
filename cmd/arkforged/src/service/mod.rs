//! The daemon's request handler, independent of any socket.
//!
//! architecture.md 15.3. Keeping dispatch transport-agnostic is what lets the
//! API-surface tests assert "the public socket cannot start execution" and
//! "startExecution is unavailable" without opening a socket — the properties
//! belong to the service, not to the plumbing.

use crate::artifact_ops::{inspect_stored_container, manifest_response};
use crate::dispatch::{PendingPreparation, PendingReconcile, ReconcileDispatchOutcome};
use crate::jobs::{AdmissionFacts, JobRegistry};
use arkforge_artifact::cas::{CasQuota, ContentAddressedStore};
use arkforge_artifact::manifest::ArtifactManifest;
use arkforge_artifact::{dayu200, pac};
use arkforge_authority_api::{
    ControllerPairingSecret, CurrentFacts, EffectSetCompleteness, PossibleEffectSet, StepPermit,
};
use arkforge_core::digest::{Domain, digest_canonical, digest_in_domain, sha256};
use arkforge_core::identity::{
    HostPlatform, MaturityKey, ToolchainIdentity, ToolchainKind, Version,
};
use arkforge_core::ids::{OpaqueId, PlanId};
use arkforge_core::outcome::ActionDisposition;
use arkforge_core::plan::RecoveryContractRef;
use arkforge_core::plan::{ExecutionPurpose, PlanMaterialization};
use arkforge_core::profile::{DeviceProfile, RecoveryDeclaration};
use arkforge_core::{
    AuthorityBindingRef, AuthorityNamespace, AuthoritySupportBinding, AuthoritySupportState,
    DeviceMode, PersistentEffect, Sha256Digest, TransientEffect,
};
use arkforge_engine::superseding::{
    EffectObservation, RecoveryBlocker, SupersedingRecoveryAssessment, assess_superseding_recovery,
    possible_effects as assess_possible_effects,
};
use arkforge_engine::{BoundToolchain, Engine, ExecutionReadiness, StoredPlan};
use arkforge_engine::{journal::JournalRecordKind, recovery::PermitLedger};
use arkforge_ipc::messages::{
    Assessment, Effect, ErrorBody, ExecutablePlan, JobSummary, KeyValue, ManagedControlAction,
    MaterializePlanResponse, PublicStep, Request, Response, SubmissionOutcome,
    SubmitManagedControlReceiptRequest, SubmitStepPermitRequest, WatchJobRequest,
};
use arkforge_ipc::{Api, SessionKind, Status};
use arkforge_provider::rockchip::{RockchipProvider, publish_dayu200_maturity};
use arkforge_provider::unisoc::{UnisocProvider, publish_af_v3_maturity};
use arkforge_provider::{
    FlashIntent, FlashProvider, MaterializeRequest, MaturityRegistry, ProbeContext,
    ReadOnlyReconcilePlan, ReconcileRequest,
};
use arkforge_transport::replay::TranscriptTransport;
use arkforge_transport::usb::{UsbDeviceRecord, UsbEnumerator, UsbTransport};
use arkforge_transport::{
    DeviceObservation, DeviceTransport, TransportError, TransportSession, TypedDiscoveryFilter,
    transcript,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::Read;
use std::path::Path;

#[path = "parts/helpers.rs"]
mod helpers;
#[path = "parts/lifecycle.rs"]
mod lifecycle;
#[path = "parts/materialize.rs"]
mod materialize;
#[path = "parts/permits.rs"]
mod permits;
#[path = "parts/preamble.rs"]
mod preamble;
#[path = "parts/recovery.rs"]
mod recovery;
#[path = "parts/requests.rs"]
mod requests;
#[cfg(test)]
#[path = "parts/tests.rs"]
mod tests;

pub use helpers::*;
pub use preamble::*;
