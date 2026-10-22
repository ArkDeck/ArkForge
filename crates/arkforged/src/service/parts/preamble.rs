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

/// Adapts the daemon's safe native IOKit boundary to the read-only transport
/// record shape. Unlike the historical `ioreg` enumerator this performs no
/// process spawn or text parsing, so mode-rebind polling can be both fast and
/// cheap. Interface records belonging to one USB device are collapsed before
/// the transport applies a Profile's VID/PID allowlist.
#[derive(Debug, Clone, Copy)]
struct NativeIokitEnumerator;

/// Resolves HDC's raw USB connect key without spawning its slow target-list
/// command. The serial is returned only to the in-process CLI authority and is
/// selected by both sealed descriptor and topology digests; it is never put in
/// public observations, journals, receipts, or errors.
pub fn native_hdc_connect_key(
    serial_sha256: &str,
    topology_sha256: &str,
) -> Result<Option<String>, String> {
    let Ok(interfaces) = arkforge_usb::NativeUsb::new(30_000).enumerate() else {
        return Ok(None);
    };
    select_hdc_connect_key(
        interfaces
            .into_iter()
            .map(|interface| (interface.location_id, interface.serial)),
        serial_sha256,
        topology_sha256,
    )
}

fn select_hdc_connect_key(
    candidates: impl IntoIterator<Item = (u32, Option<String>)>,
    serial_sha256: &str,
    topology_sha256: &str,
) -> Result<Option<String>, String> {
    let mut matches = candidates
        .into_iter()
        .filter(|(location_id, _)| {
            digest_in_domain(Domain::UsbTopology, &location_id.to_be_bytes()).to_hex()
                == topology_sha256
        })
        .filter_map(|(_, serial)| serial)
        .filter(|serial| {
            !serial.is_empty()
                && serial.len() <= 128
                && serial
                    .bytes()
                    .all(|byte| byte.is_ascii() && !byte.is_ascii_whitespace())
                && digest_in_domain(Domain::DeviceSerial, serial.as_bytes()).to_hex()
                    == serial_sha256
        })
        .collect::<Vec<_>>();
    matches.sort();
    matches.dedup();
    match matches.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(one.clone())),
        _ => Err("multiple native USB descriptors match the sealed HDC target".into()),
    }
}

/// Recompute the retired Windows location-path encoding from a live Normal
/// node that exactly matches all current identity digests. No raw serial leaves
/// this boundary, and no historical PnP node can contribute evidence.
pub fn native_normal_topology_projection(
    observation_id: &str,
    topology_sha256: &str,
    descriptor_sha256: &str,
    serial_sha256: &str,
) -> Result<Option<(String, String)>, String> {
    let observations = arkforge_usb::NativeUsb::new(30_000)
        .observe_windows_legacy_topologies()
        .map_err(|error| error.to_string())?;
    select_normal_topology_projection(
        observations,
        observation_id,
        topology_sha256,
        descriptor_sha256,
        serial_sha256,
    )
}

fn select_normal_topology_projection(
    observations: Vec<arkforge_usb::LegacyTopologyObservation>,
    observation_id: &str,
    topology_sha256: &str,
    descriptor_sha256: &str,
    serial_sha256: &str,
) -> Result<Option<(String, String)>, String> {
    let mut matches = Vec::new();
    for observed in observations {
        let interface = observed.descriptor;
        if interface.vendor_id != 0x2207
            || interface.product_id != 0x5000
            || interface.serial.as_ref().is_none_or(String::is_empty)
        {
            continue;
        }
        let record = UsbDeviceRecord {
            vendor_id: interface.vendor_id,
            product_id: interface.product_id,
            location_id: interface.location_id,
            serial: interface.serial,
            product_name: interface.product_name,
            vendor_name: interface.vendor_name,
            bcd_device: Some(interface.device_release),
        };
        let current_id = format!(
            "USB-{:04x}-{:04x}-{:08x}",
            record.vendor_id, record.product_id, record.location_id
        );
        let serial = match record.serial_evidence() {
            arkforge_transport::SerialEvidence::Descriptor { digest } => digest.to_hex(),
            _ => continue,
        };
        if current_id != observation_id
            || record.topology_digest().to_hex() != topology_sha256
            || record.descriptor_digest().to_hex() != descriptor_sha256
            || serial != serial_sha256
        {
            continue;
        }
        matches.push((
            format!(
                "USB-{:04x}-{:04x}-{:08x}",
                record.vendor_id, record.product_id, observed.legacy_location_id
            ),
            digest_in_domain(
                Domain::UsbTopology,
                &observed.legacy_location_id.to_be_bytes(),
            )
            .to_hex(),
        ));
    }
    match matches.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(one.clone())),
        _ => Err("Multiple present USB nodes match the identity projection.".into()),
    }
}

impl UsbEnumerator for NativeIokitEnumerator {
    fn enumerate(&self) -> Result<Vec<UsbDeviceRecord>, TransportError> {
        let interfaces = arkforge_usb::NativeUsb::new(30_000)
            .enumerate()
            .map_err(|error| TransportError::Evidence(error.to_string()))?;
        let mut devices = Vec::new();
        for interface in interfaces {
            let record = UsbDeviceRecord {
                vendor_id: interface.vendor_id,
                product_id: interface.product_id,
                location_id: interface.location_id,
                serial: interface.serial,
                product_name: interface.product_name,
                vendor_name: interface.vendor_name,
                bcd_device: Some(interface.device_release),
            };
            if !devices.contains(&record) {
                devices.push(record);
            }
        }
        Ok(devices)
    }
}

/// Where this daemon's notion of "now" comes from.
///
/// It used to be a `u64` captured at startup and never reassigned, which meant
/// every timestamp the daemon stamped was its own launch time. An admission
/// snapshot carries `observed_at_epoch_ms` and a 60s `snapshot_lifetime_ms`, so
/// once the daemon had been up for a minute every admission it offered was
/// already expired when the authority read it, and every one was refused. The
/// clock has to be read when a fact is stamped, not once at boot.
///
/// `Fixed` exists so tests keep asserting exact timestamps; production is
/// `System` and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clock {
    System,
    Fixed(u64),
}

impl Clock {
    pub fn now_epoch_ms(&self) -> u64 {
        match self {
            Clock::System => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|delta| delta.as_millis() as u64)
                .unwrap_or(0),
            Clock::Fixed(value) => *value,
        }
    }
}

/// Everything the daemon serves from.
#[derive(Debug)]
pub struct Service {
    store: ContentAddressedStore,
    engine: Engine,
    rockchip: RockchipProvider,
    unisoc: UnisocProvider,
    maturity: MaturityRegistry,
    profiles: BTreeMap<String, DeviceProfile>,
    manifests: BTreeMap<String, ArtifactManifest>,
    /// Every transport this daemon can observe through.
    ///
    /// Boxed rather than one concrete type because the daemon needs both: a
    /// transcript replays a captured device, and `UsbTransport` sees the one
    /// actually plugged in. It held only transcripts until 2026-08-17, which
    /// meant `discoverDevices` answered "no devices observed" on a host whose
    /// `ioreg` was listing the board — and `materializePlan`, which matches an
    /// observation before probing, could therefore never reach a real device
    /// (AD-027).
    transports: Vec<Box<dyn DeviceTransport>>,
    /// Exact observation/transport used when each executable plan was sealed.
    /// Starting a plan re-opens this observation; it never performs a fresh
    /// unqualified "first matching device" selection.
    plan_observations: BTreeMap<String, PlanObservationContext>,
    /// Open continuity session for every running job.
    job_sessions: BTreeMap<String, JobObservationSession>,
    /// Jobs with a proven mode transition: either the authority accepted its
    /// own managed-control receipt, or ArkForge durably recorded semantic
    /// success for a sealed native step whose expected mode changed.
    ///
    /// Only those jobs may replace a detached session with the single device
    /// observed in the next expected mode. A periodic sweep must never turn an
    /// unexplained detach into permission to select a new device.
    authorized_rebinds: BTreeSet<String>,
    pending_preparations: VecDeque<PendingPreparation>,
    pending_reconciliations: VecDeque<PendingReconcile>,
    clock: Clock,
    jobs: JobRegistry,
    /// The secret the authority handed this daemon at startup. Held here and
    /// nowhere else; there is no getter.
    pairing: Option<ControllerPairingSecret>,
    /// What this daemon can do, as standing facts. Kept beside the secret
    /// rather than derived from it, because pairing is only half of it.
    readiness: ExecutionReadiness,
    /// The exact RockUSB backend identity bound to the dispatcher.
    ///
    /// `ExecutionReadiness` deliberately carries only the id and digest needed
    /// by the engine's start gate. Materialization additionally needs the
    /// backend kind and version because those are part of the maturity key, so
    /// the daemon retains the full native identity here.
    rockchip_toolchain: Option<ToolchainIdentity>,
    /// The acceptance campaign this daemon runs, if any. Held because
    /// Native dispatcher binding republishes maturity and must publish the same
    /// campaign the construction did — a binding that silently dropped it
    /// would turn an authorized campaign back into `hardwareGated`.
    campaign: Option<String>,
}

#[derive(Debug, Clone)]
struct PlanObservationContext {
    transport_index: usize,
    observation: DeviceObservation,
}

#[derive(Debug, Clone)]
struct RecoverySurfaceContext {
    possible: PossibleEffectSet,
    declaration: RecoveryDeclaration,
    contract: Option<RecoveryContractRef>,
}

#[derive(Debug)]
struct JobObservationSession {
    transport_index: usize,
    session: Box<dyn TransportSession>,
}
