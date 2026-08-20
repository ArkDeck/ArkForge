//! The daemon's request handler, independent of any socket.
//!
//! architecture.md 15.3. Keeping dispatch transport-agnostic is what lets the
//! API-surface tests assert "the public socket cannot start execution" and
//! "startExecution is unavailable" without opening a socket — the properties
//! belong to the service, not to the plumbing.

use crate::jobs::{AdmissionFacts, JobRegistry};
use arkforge_artifact::cas::{CasQuota, ContentAddressedStore};
use arkforge_artifact::manifest::ArtifactManifest;
use arkforge_artifact::{dayu200, pac};
use arkforge_authority_api::{
    ControllerPairingSecret, CurrentFacts, EffectSetCompleteness, PossibleEffectSet, StepPermit,
};
use arkforge_core::digest::{Domain, digest_canonical};
use arkforge_core::identity::{HostPlatform, ToolchainIdentity, ToolchainKind, Version};
use arkforge_core::ids::{OpaqueId, PlanId};
use arkforge_core::outcome::ActionDisposition;
use arkforge_core::plan::RecoveryContractRef;
use arkforge_core::plan::{ExecutionPurpose, PlanMaterialization};
use arkforge_core::profile::{DeviceProfile, RecoveryDeclaration};
use arkforge_core::{
    AuthorityBindingRef, AuthorityNamespace, DeviceMode, PersistentEffect, Sha256Digest,
    TransientEffect,
};
use arkforge_engine::superseding::{
    RecoveryBlocker, SupersedingRecoveryAssessment, assess_superseding_recovery,
    possible_effects as assess_possible_effects,
};
use arkforge_engine::{BoundToolchain, Engine, ExecutionReadiness, StoredPlan};
use arkforge_ipc::messages::{
    ArchiveMember, Assessment, Effect, ErrorBody, ExecutablePlan, InspectArtifactResponse,
    JobSummary, KeyValue, MaterializePlanResponse, PartitionEntry, PublicStep, Request, Response,
    SubmissionOutcome, SubmitManagedControlReceiptRequest, SubmitStepPermitRequest,
    WatchJobRequest,
};
use arkforge_ipc::{Api, SessionKind, Status};
use arkforge_provider::rockchip::{RockchipProvider, publish_dayu200_maturity};
use arkforge_provider::unisoc::{UnisocProvider, publish_af_v3_maturity};
use arkforge_provider::{
    FlashIntent, FlashProvider, MaterializeRequest, MaturityRegistry, ProbeContext,
};
use arkforge_transport::replay::TranscriptTransport;
use arkforge_transport::usb::UsbTransport;
use arkforge_transport::{
    DeviceObservation, DeviceTransport, TransportSession, TypedDiscoveryFilter, transcript,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;

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

impl Service {
    /// Builds the service.
    ///
    /// `campaign` is the acceptance campaign this daemon is running, if any.
    /// It is a parameter with no default because it decides whether any
    /// DAYU200 plan can be executable at all: `None` publishes `HardwareGated`
    /// and the daemon can materialize assessments only, which is what every
    /// ordinary run wants. See [`publish_dayu200_maturity`].
    pub fn new(
        store_root: &Path,
        profiles: Vec<DeviceProfile>,
        transcripts: Vec<String>,
        clock: Clock,
        campaign: Option<&str>,
    ) -> Result<Self, String> {
        let store = ContentAddressedStore::open(store_root, CasQuota::dayu200_default())
            .map_err(|error| error.to_string())?;
        let rockchip = RockchipProvider::new();
        let unisoc = UnisocProvider::new();

        let mut maturity = MaturityRegistry::new();
        let mut profile_map = BTreeMap::new();
        for profile in profiles {
            // Every profile publishes its maturity as it is loaded, so a
            // combination is never merely absent from the registry. Which
            // provider is published for a profile follows the profile's own
            // declared artifact formats — the daemon does not decide that a
            // device belongs to a vendor.
            if profile
                .artifact_formats
                .iter()
                .any(|format| format.as_str() == dayu200::FORMAT_ID)
            {
                for toolchain in [
                    unbound_native_toolchain_identity(),
                    replay_toolchain_identity(),
                ] {
                    publish_dayu200_maturity(
                        &mut maturity,
                        &rockchip,
                        &profile,
                        &toolchain,
                        &HostPlatform::current(),
                        driver_facts_digest(),
                        evidence_set_digest(),
                        campaign,
                    )
                    .map_err(|error| error.to_string())?;
                }
            }
            if profile
                .artifact_formats
                .iter()
                .any(|format| format.as_str() == pac::FORMAT_ID)
            {
                publish_af_v3_maturity(
                    &mut maturity,
                    &unisoc,
                    &profile,
                    &research_toolchain_identity(),
                    &HostPlatform::current(),
                    driver_facts_digest(),
                    evidence_set_digest(),
                )
                .map_err(|error| error.to_string())?;
            }
            profile_map.insert(profile.id.as_str().to_string(), profile);
        }

        let mut loaded: Vec<Box<dyn DeviceTransport>> = Vec::new();
        for source in transcripts {
            let parsed = transcript::parse(&source).map_err(|error| error.to_string())?;
            loaded.push(Box::new(TranscriptTransport::new(parsed)));
        }
        // One USB transport per loaded profile, because the transport reads
        // its identity table from the profile: which vendor/product pairs in
        // which mode are this device, rather than "any Rockchip in Loader".
        // Read-only ioreg enumeration — this opens no HDC server and takes no
        // device, so it coexists with whatever else owns the board.
        for profile in profile_map.values() {
            loaded.push(Box::new(UsbTransport::with_ioreg(profile)));
        }

        Ok(Service {
            store,
            engine: Engine::new(),
            rockchip,
            unisoc,
            maturity,
            profiles: profile_map,
            manifests: BTreeMap::new(),
            transports: loaded,
            plan_observations: BTreeMap::new(),
            job_sessions: BTreeMap::new(),
            authorized_rebinds: BTreeSet::new(),
            clock,
            jobs: JobRegistry::open(store_root.join("jobs")).map_err(|error| error.to_string())?,
            pairing: None,
            readiness: ExecutionReadiness::default(),
            rockchip_toolchain: None,
            campaign: campaign.map(str::to_string),
        })
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn jobs(&self) -> &JobRegistry {
        &self.jobs
    }

    /// Pairs an authority with this daemon.
    ///
    /// Consuming the secret by value is the point: the caller cannot keep a
    /// copy to hand to something else, and there is no getter to read it back.
    pub fn pair_authority(&mut self, secret: ControllerPairingSecret) {
        self.pairing = Some(secret);
        self.readiness.authority_paired = true;
    }

    /// Binds ArkForge's in-process RockUSB implementation.
    ///
    /// Its backend digest is the currently running `arkforged` executable,
    /// not a vendor binary and not a source-revision string. This makes every
    /// build a distinct maturity combination and prevents an acceptance result
    /// for one native build from authorizing another.
    pub fn bind_native_dispatcher(&mut self, toolchain: BoundToolchain) {
        let identity = ToolchainIdentity {
            id: toolchain.id.clone(),
            ..native_toolchain_identity(toolchain.backend_digest)
        };
        self.bind_rockchip_dispatcher(toolchain, identity);
    }

    fn bind_rockchip_dispatcher(&mut self, toolchain: BoundToolchain, identity: ToolchainIdentity) {
        self.readiness.dispatcher = Some(toolchain);
        self.rockchip_toolchain = Some(identity.clone());
        for profile in self.profiles.values() {
            if !profile
                .artifact_formats
                .iter()
                .any(|format| format.as_str() == dayu200::FORMAT_ID)
            {
                continue;
            }
            // Ignored deliberately: a profile whose identity cannot be formed
            // published nothing at construction either, and a binding is not
            // the place to discover a malformed profile.
            let _ = publish_dayu200_maturity(
                &mut self.maturity,
                &self.rockchip,
                profile,
                &identity,
                &HostPlatform::current(),
                driver_facts_digest(),
                evidence_set_digest(),
                self.campaign.as_deref(),
            );
        }
    }

    /// The RockUSB identity of the backend this daemon actually bound.
    ///
    /// Falls back to a non-executable native placeholder when nothing is bound, which is
    /// the read-only daemon: it materializes assessments, and an assessment
    /// names the combination it was assessed against rather than one it
    /// could run.
    fn bound_rockchip_toolchain_identity(&self) -> ToolchainIdentity {
        self.rockchip_toolchain
            .clone()
            .unwrap_or_else(unbound_native_toolchain_identity)
    }

    /// What this daemon can do. Standing facts, established at startup.
    pub fn readiness(&self) -> &ExecutionReadiness {
        &self.readiness
    }

    pub fn authority_paired(&self) -> bool {
        self.pairing.is_some()
    }

    /// Hands a dispatcher the next piece of work.
    ///
    /// Separate from [`Self::complete_dispatch`] on purpose: the caller takes
    /// the work, **releases this service's lock**, runs it, and comes back.
    /// A single call that did both would hold the lock across a partition
    /// write, and the event stream reporting on that write runs through the
    /// same lock.
    pub fn take_pending_dispatch(&mut self) -> Option<crate::jobs::PendingDispatch> {
        self.jobs.take_pending_dispatch()
    }

    /// Classifies control requests whose deadline passed unanswered.
    ///
    /// Runs on the dispatcher's sweep, beside [`Self::take_pending_dispatch`],
    /// so a silent authority costs one deadline rather than leaving the job
    /// parked at `permitConsuming` forever. Returns the ids it classified,
    /// for the caller's log line.
    pub fn expire_stale_controls(&mut self) -> Vec<String> {
        self.jobs.expire_stale_controls(self.clock.now_epoch_ms())
    }

    /// Retries only jobs parked before admission. A missing device leaves the
    /// job parked; ambiguity or an identity/mode mismatch never chooses one.
    pub fn refresh_pending_admissions(&mut self) {
        for job_id in self.jobs.jobs_needing_admission() {
            let allow_unique_rebind = self.authorized_rebinds.contains(&job_id);
            let _ = self.publish_live_admission(&job_id, allow_unique_rebind);
        }
    }

    /// Records what a dispatcher observed.
    pub fn complete_dispatch(
        &mut self,
        job_id: &str,
        outcome: crate::jobs::DispatchOutcome,
    ) -> Result<(), String> {
        let stored = self
            .stored_plan_for_job(job_id)
            .ok_or_else(|| format!("no job {job_id}"))?;
        let completed_step_id = self
            .jobs
            .job(job_id)
            .map(crate::jobs::Job::current_step_id)
            .ok_or_else(|| format!("no job {job_id}"))?;
        let opens_exact_rebind = stored
            .envelope
            .public_steps
            .iter()
            .find(|step| step.step_id.as_str() == completed_step_id)
            .is_some_and(|step| {
                successful_dispatch_requires_rebind(
                    outcome.disposition,
                    step.expected_mode_before.as_ref(),
                    step.expected_mode_after.as_ref(),
                )
            });
        self.jobs
            .complete_dispatch(
                job_id,
                outcome,
                &stored.envelope,
                &stored.private_plan,
                self.clock.now_epoch_ms(),
            )
            .map_err(|error| error.to_string())?;
        if opens_exact_rebind
            && self
                .jobs
                .job(job_id)
                .is_some_and(crate::jobs::Job::needs_admission)
        {
            // DEVICE_RESET has a durable semantic receipt at this point. Its
            // sealed mode transition is therefore as real as an accepted
            // managed-control transition, and the old Loader session must not
            // be reused for the normal-mode postflight. Grant exactly one
            // expected-mode rebind; discovery ambiguity still refuses.
            self.job_sessions.remove(job_id);
            self.authorized_rebinds.insert(job_id.to_string());
        }
        // Same-mode steps reuse and re-read the open session. A proven mode
        // transition instead waits for the one authorized exact rebind; the
        // dispatcher sweep retries while the device is still booting.
        let allow_unique_rebind = self.authorized_rebinds.contains(job_id);
        let _ = self.publish_live_admission(job_id, allow_unique_rebind);
        Ok(())
    }

    fn publish_live_admission(
        &mut self,
        job_id: &str,
        allow_unique_rebind: bool,
    ) -> Result<(), String> {
        let stored = self
            .stored_plan_for_job(job_id)
            .ok_or_else(|| format!("no job {job_id}"))?;
        if !self
            .jobs
            .job(job_id)
            .is_some_and(crate::jobs::Job::needs_admission)
        {
            return Ok(());
        }

        let now = self.clock.now_epoch_ms();
        let existing = if let Some(context) = self.job_sessions.get_mut(job_id) {
            match context.session.reread_identity() {
                Ok(mut observation) => {
                    observation.observed_at_epoch_ms = now;
                    Some((
                        observation,
                        context.session.session_digest(),
                        context.transport_index,
                    ))
                }
                Err(_) => None,
            }
        } else {
            None
        };

        let (observation, session_digest, transport_index) = match existing {
            Some(facts) => facts,
            None if allow_unique_rebind => {
                let plan_context = self
                    .plan_observations
                    .get(&stored.envelope.plan_digest.to_hex())
                    .cloned()
                    .ok_or_else(|| "the plan has no sealed observation context".to_string())?;
                let expected_mode = next_expected_mode(&stored, self.jobs.job(job_id))?;
                let transport = self
                    .transports
                    .get(plan_context.transport_index)
                    .ok_or_else(|| "the plan transport is no longer loaded".to_string())?;
                let mut observations = transport
                    .discover(
                        &TypedDiscoveryFilter {
                            modes: expected_mode.into_iter().collect(),
                            ..TypedDiscoveryFilter::default()
                        },
                        now,
                    )
                    .map_err(|error| error.to_string())?;
                if observations.len() != 1 {
                    return Err(format!(
                        "expected exactly one device for rebind, observed {}",
                        observations.len()
                    ));
                }
                let mut observation = observations.remove(0);
                observation.observed_at_epoch_ms = now;
                let mut session = transport
                    .open_exact(&observation)
                    .map_err(|error| error.to_string())?;
                let mut reread = session
                    .reread_identity()
                    .map_err(|error| error.to_string())?;
                reread.observed_at_epoch_ms = now;
                let digest = session.session_digest();
                self.job_sessions.insert(
                    job_id.to_string(),
                    JobObservationSession {
                        transport_index: plan_context.transport_index,
                        session,
                    },
                );
                // The one authorization granted by an accepted managed
                // control receipt is consumed by opening this exact session.
                self.authorized_rebinds.remove(job_id);
                (reread, digest, plan_context.transport_index)
            }
            None => return Err("the exact admission session no longer observes the device".into()),
        };

        let admission = admission_facts(&stored.envelope, observation, session_digest)?;
        self.jobs
            .request_next_admission(
                job_id,
                &stored.envelope,
                &stored.private_plan,
                &admission,
                now,
            )
            .map_err(|error| error.to_string())?;
        if let Some(context) = self.job_sessions.get_mut(job_id) {
            context.transport_index = transport_index;
        }
        Ok(())
    }

    fn current_facts_for_job(&mut self, job_id: &str) -> Result<CurrentFacts, String> {
        let stored = self
            .stored_plan_for_job(job_id)
            .ok_or_else(|| format!("no job {job_id}"))?;
        let now = self.clock.now_epoch_ms();
        let context = self
            .job_sessions
            .get_mut(job_id)
            .ok_or_else(|| "the job has no open admission session".to_string())?;
        let mut observation = context
            .session
            .reread_identity()
            .map_err(|error| error.to_string())?;
        observation.observed_at_epoch_ms = now;
        let admission = admission_facts(
            &stored.envelope,
            observation,
            context.session.session_digest(),
        )?;
        Ok(CurrentFacts {
            now_epoch_ms: now,
            device_facts_digest: admission
                .observation
                .admission_facts_digest()
                .map_err(|error| error.to_string())?,
            transport_session_digest: Some(admission.transport_session_digest),
            saw_detach_since_snapshot: context.session.saw_detach(),
            provider_facts_digest: admission.provider_facts_digest,
            toolchain_facts_digest: admission.toolchain_facts_digest,
            artifact_facts_digest: admission.artifact_facts_digest,
        })
    }

    /// Dispatches one request.
    ///
    /// `artifact_stream` is present only for `importArtifact`, which is the one
    /// call that carries bulk content. The daemon never opens a path the caller
    /// named (architecture.md 10.1).
    pub fn handle(
        &mut self,
        session: SessionKind,
        request: &Request,
        artifact_stream: Option<&mut dyn Read>,
    ) -> Response {
        if !session.may_call(request.api) {
            return self.refuse(
                request,
                Status::Refused,
                "SESSION_NOT_PERMITTED",
                &format!(
                    "{} is not available on the {:?} socket",
                    request.api, session
                ),
            );
        }

        match request.api {
            Api::ImportArtifact => self.import_artifact(request, artifact_stream),
            Api::InspectArtifact => self.inspect_artifact(request),
            Api::DiscoverDevices => self.discover_devices(request),
            Api::ProbeDevice => self.probe_device(request),
            Api::MaterializePlan => self.materialize_plan(request),
            Api::StartExecution => self.start_execution(request),
            Api::WatchJob => self.watch_job(request),
            Api::GetJob => self.get_job(request),
            Api::ListJobs => self.list_jobs(request),
            Api::CancelJob => self.cancel_job(request),
            Api::SubmitStepPermit => self.submit_step_permit(request),
            Api::SubmitManagedControlReceipt => self.submit_control_receipt(request),
            Api::ReconcileJob => self.reconcile_job(request),
            Api::PlanSupersedingRecovery => self.plan_superseding_recovery(request),
            Api::GetRecoveryGuide => self.get_recovery_guide(request),
        }
    }

    fn refuse(&self, request: &Request, status: Status, code: &str, message: &str) -> Response {
        Response {
            request_id: request.request_id.clone(),
            api: request.api,
            status,
            payload: ErrorBody {
                code: code.to_string(),
                message: message.to_string(),
            }
            .encode(),
            stream_sequence: 0,
            stream_end: true,
        }
    }

    fn ok(&self, request: &Request, payload: Vec<u8>) -> Response {
        Response {
            request_id: request.request_id.clone(),
            api: request.api,
            status: Status::Ok,
            payload,
            stream_sequence: 0,
            stream_end: true,
        }
    }

    fn import_artifact(
        &mut self,
        request: &Request,
        artifact_stream: Option<&mut dyn Read>,
    ) -> Response {
        let Some(stream) = artifact_stream else {
            return self.refuse(
                request,
                Status::InvalidArgument,
                "NO_CONTENT_STREAM",
                "importArtifact carries its content on the controller stream",
            );
        };
        let payload = match decode_import_request(&request.payload) {
            Ok(payload) => payload,
            Err(message) => {
                return self.refuse(
                    request,
                    Status::InvalidArgument,
                    "MALFORMED_REQUEST",
                    &message,
                );
            }
        };
        let expected_digest = match payload.expected_sha256.as_deref() {
            None => None,
            Some(hex) => match Sha256Digest::parse_hex(hex) {
                Ok(digest) => Some(digest),
                Err(error) => {
                    return self.refuse(
                        request,
                        Status::InvalidArgument,
                        "MALFORMED_DIGEST",
                        &error.to_string(),
                    );
                }
            },
        };

        match self
            .store
            .import(stream, payload.expected_size_bytes, expected_digest)
        {
            Ok(imported) => {
                let mut out = Vec::new();
                arkforge_ipc::wire::write_string(&mut out, 1, &imported.digest.to_hex());
                arkforge_ipc::wire::write_string(&mut out, 2, &imported.digest.to_hex());
                arkforge_ipc::wire::write_uint64(&mut out, 3, imported.size_bytes);
                arkforge_ipc::wire::write_bool(&mut out, 4, imported.deduplicated);
                self.ok(request, out)
            }
            Err(error) => self.refuse(
                request,
                Status::Refused,
                "IMPORT_REFUSED",
                &error.to_string(),
            ),
        }
    }

    fn inspect_artifact(&mut self, request: &Request) -> Response {
        let artifact_id = match first_string_field(&request.payload, 1) {
            Some(value) => value,
            None => {
                return self.refuse(
                    request,
                    Status::InvalidArgument,
                    "MISSING_ARTIFACT_ID",
                    "inspectArtifact requires an artifact id",
                );
            }
        };
        let digest = match Sha256Digest::parse_hex(&artifact_id) {
            Ok(digest) => digest,
            Err(error) => {
                return self.refuse(
                    request,
                    Status::InvalidArgument,
                    "MALFORMED_ARTIFACT_ID",
                    &error.to_string(),
                );
            }
        };

        let manifest = match self.manifests.get(&artifact_id) {
            Some(manifest) => manifest.clone(),
            None => {
                let object = match self.store.open_object(&digest) {
                    Ok(object) => object,
                    Err(error) => {
                        return self.refuse(
                            request,
                            Status::NotFound,
                            "ARTIFACT_NOT_FOUND",
                            &error.to_string(),
                        );
                    }
                };
                match inspect_container(object) {
                    Ok(manifest) => {
                        self.manifests.insert(artifact_id.clone(), manifest.clone());
                        manifest
                    }
                    Err(error) => {
                        return self.refuse(request, Status::Refused, "ARTIFACT_REJECTED", &error);
                    }
                }
            }
        };

        self.ok(request, encode_manifest(&manifest).encode())
    }

    fn discover_devices(&mut self, request: &Request) -> Response {
        let filter = TypedDiscoveryFilter::default();
        let mut out = Vec::new();
        for transport in &self.transports {
            let observations = match transport.discover(&filter, self.clock.now_epoch_ms()) {
                Ok(observations) => observations,
                Err(error) => {
                    return self.refuse(
                        request,
                        Status::Internal,
                        "DISCOVERY_FAILED",
                        &error.to_string(),
                    );
                }
            };
            for observation in observations {
                arkforge_ipc::wire::write_message(&mut out, 1, &encode_observation(&observation));
            }
        }
        self.ok(request, out)
    }

    fn probe_device(&mut self, request: &Request) -> Response {
        let observation_id = first_string_field(&request.payload, 1).unwrap_or_default();
        let profile_id = first_string_field(&request.payload, 2).unwrap_or_default();
        let Some(profile) = self.profiles.get(&profile_id) else {
            return self.refuse(
                request,
                Status::NotFound,
                "PROFILE_NOT_FOUND",
                &format!("no loaded profile {profile_id}"),
            );
        };

        for transport in &self.transports {
            let Ok(observations) =
                transport.discover(&TypedDiscoveryFilter::default(), self.clock.now_epoch_ms())
            else {
                continue;
            };
            let Some(observation) = observations
                .iter()
                .find(|candidate| candidate.observation_id.as_str() == observation_id)
            else {
                continue;
            };
            let provider = provider_for(profile, &self.rockchip, &self.unisoc);
            let Some(provider) = provider else {
                return self.refuse(
                    request,
                    Status::NotFound,
                    "NO_PROVIDER_FOR_PROFILE",
                    &format!(
                        "no registered provider handles the artifact formats profile {} declares",
                        profile.id
                    ),
                );
            };
            return match provider.probe(&ProbeContext {
                transport: transport.as_ref(),
                observation,
                profile,
            }) {
                Ok(probe) => {
                    let mut out = Vec::new();
                    arkforge_ipc::wire::write_message(
                        &mut out,
                        1,
                        &encode_observation(&probe.observation),
                    );
                    for (key, value) in &probe.protocol_facts {
                        arkforge_ipc::wire::write_message(
                            &mut out,
                            2,
                            &KeyValue {
                                key: key.to_string(),
                                value: value.clone(),
                            }
                            .encode(),
                        );
                    }
                    arkforge_ipc::wire::write_string(&mut out, 3, profile.id.as_str());
                    arkforge_ipc::wire::write_string(&mut out, 4, &probe.facts_digest.to_hex());
                    self.ok(request, out)
                }
                Err(error) => self.refuse(
                    request,
                    Status::Refused,
                    "PROBE_REFUSED",
                    &error.to_string(),
                ),
            };
        }

        self.refuse(
            request,
            Status::NotFound,
            "OBSERVATION_NOT_FOUND",
            &format!("no observation {observation_id}"),
        )
    }

    // -----------------------------------------------------------------------
    // The controller execution/admission surface (architecture.md 8, 13, 15.3)
    // -----------------------------------------------------------------------

    /// Creates a job for a plan this daemon materialized.
    ///
    /// Everything a caller can supply is an internal identifier. There is no
    /// partition, address, tool, timeout or effect override in the request,
    /// and the plan is resolved out of the daemon's own store rather than
    /// rebuilt from anything the caller sent (architecture.md 15.3).
    fn start_execution(&mut self, request: &Request) -> Response {
        // Standing blockers first, because they are facts about this daemon
        // and the payload is a fact about one request. A daemon with no
        // authority answering "unknown plan" would send an operator to fix a
        // plan that could not have run either way.
        let standing = self.readiness.standing_blockers();
        if !standing.is_empty() {
            return self.refuse(
                request,
                Status::Unavailable,
                standing[0].code(),
                &blocker_list(&standing),
            );
        }
        let plan_id = first_string_field(&request.payload, 1).unwrap_or_default();
        let plan_sha256 = first_string_field(&request.payload, 2).unwrap_or_default();
        let execution_purpose = first_string_field(&request.payload, 3).unwrap_or_default();
        let controller_session_id = first_string_field(&request.payload, 4).unwrap_or_default();

        let Ok(plan_id) = PlanId::new(&plan_id) else {
            return self.refuse(
                request,
                Status::InvalidArgument,
                "BAD_PLAN_ID",
                "planId is not a usable identifier",
            );
        };
        let Ok(expected) = Sha256Digest::parse_hex(&plan_sha256) else {
            return self.refuse(
                request,
                Status::InvalidArgument,
                "BAD_PLAN_DIGEST",
                "planSha256 is not 64 hex characters",
            );
        };
        let Ok(controller_session_id) =
            arkforge_core::ids::ControllerSessionId::new(&controller_session_id)
        else {
            return self.refuse(
                request,
                Status::InvalidArgument,
                "BAD_CONTROLLER_SESSION_ID",
                "controllerSessionId is not a usable identifier",
            );
        };

        let readiness = self.readiness.clone();
        let stored = match self.engine.start_execution(&plan_id, expected, &readiness) {
            Ok(stored) => stored.clone(),
            Err(arkforge_engine::EngineError::ExecutionDisabled(blockers)) => {
                let code = blockers
                    .first()
                    .map(|blocker| blocker.code())
                    .unwrap_or("EXECUTION_DISABLED");
                return self.refuse(request, Status::Unavailable, code, &blocker_list(&blockers));
            }
            Err(error) => {
                return self.refuse(
                    request,
                    Status::NotFound,
                    "PLAN_NOT_STARTABLE",
                    &error.to_string(),
                );
            }
        };
        if execution_purpose != stored.envelope.execution_purpose.as_str() {
            return self.refuse(
                request,
                Status::Refused,
                "EXECUTION_PURPOSE_MISMATCH",
                "executionPurpose does not match the sealed plan",
            );
        }

        let Some(observation_context) = self
            .plan_observations
            .get(&stored.envelope.plan_digest.to_hex())
            .cloned()
        else {
            return self.refuse(
                request,
                Status::Unavailable,
                "PLAN_OBSERVATION_UNAVAILABLE",
                "the executable plan has no sealed device observation context",
            );
        };
        let Some(transport) = self.transports.get(observation_context.transport_index) else {
            return self.refuse(
                request,
                Status::Unavailable,
                "PLAN_TRANSPORT_UNAVAILABLE",
                "the transport used to materialize this plan is no longer loaded",
            );
        };
        let mut session = match transport.open_exact(&observation_context.observation) {
            Ok(session) => session,
            Err(error) => {
                return self.refuse(
                    request,
                    Status::Unavailable,
                    "EXACT_DEVICE_UNAVAILABLE",
                    &error.to_string(),
                );
            }
        };
        let now = self.clock.now_epoch_ms();
        let mut observation = match session.reread_identity() {
            Ok(observation) => observation,
            Err(error) => {
                return self.refuse(
                    request,
                    Status::Unavailable,
                    "DEVICE_REREAD_FAILED",
                    &error.to_string(),
                );
            }
        };
        observation.observed_at_epoch_ms = now;
        let admission =
            match admission_facts(&stored.envelope, observation, session.session_digest()) {
                Ok(admission) => admission,
                Err(error) => {
                    return self.refuse(
                        request,
                        Status::Internal,
                        "ADMISSION_FACTS_FAILED",
                        &error,
                    );
                }
            };

        match self.jobs.start(
            &stored.envelope,
            &stored.private_plan,
            controller_session_id,
            &admission,
            now,
        ) {
            Ok(job_id) => {
                self.job_sessions.insert(
                    job_id.clone(),
                    JobObservationSession {
                        transport_index: observation_context.transport_index,
                        session,
                    },
                );
                let mut payload = Vec::new();
                arkforge_ipc::wire::write_string(&mut payload, 1, &job_id);
                self.ok(request, payload)
            }
            Err(error) => self.refuse(request, Status::Internal, error.code(), &error.to_string()),
        }
    }

    /// Returns the events a job has published since `from_sequence`.
    ///
    /// A poll rather than a push, because the daemon serves every connection
    /// under one lock and a handler that parked waiting for the next event
    /// would stop every other call — including the one that produces it.
    fn watch_job(&mut self, request: &Request) -> Response {
        let Ok(watch) = WatchJobRequest::decode(&request.payload) else {
            return self.refuse(
                request,
                Status::InvalidArgument,
                "BAD_WATCH_REQUEST",
                "watchJob payload does not decode",
            );
        };
        let Some(job) = self.jobs.job(&watch.job_id) else {
            return self.refuse(
                request,
                Status::NotFound,
                "UNKNOWN_JOB",
                &format!("no job {}", watch.job_id),
            );
        };
        let events = job.events_from(watch.from_sequence);
        let mut payload = Vec::new();
        for event in &events {
            arkforge_ipc::wire::write_message(&mut payload, 1, &event.encode());
        }
        self.ok(request, payload)
    }

    fn get_job(&self, request: &Request) -> Response {
        let job_id = first_string_field(&request.payload, 1).unwrap_or_default();
        let Some(job) = self.jobs.job(&job_id) else {
            return self.refuse(
                request,
                Status::NotFound,
                "UNKNOWN_JOB",
                &format!("no job {job_id}"),
            );
        };
        let mut payload = Vec::new();
        arkforge_ipc::wire::write_message(&mut payload, 1, &encode_job_summary(job).encode());
        self.ok(request, payload)
    }

    fn list_jobs(&self, request: &Request) -> Response {
        let mut payload = Vec::new();
        for job in self.jobs.all_jobs() {
            arkforge_ipc::wire::write_message(&mut payload, 1, &encode_job_summary(job).encode());
        }
        self.ok(request, payload)
    }

    /// Returns the strongest read-only conclusion currently supported.
    ///
    /// DAYU200's measured RockUSB read face does not cover most writable
    /// partitions, so an unresolved write normally remains unknown. Returning
    /// that typed verdict is still materially different from replaying it or
    /// pretending the recovery API does not exist.
    fn reconcile_job(&self, request: &Request) -> Response {
        let job_id = first_string_field(&request.payload, 1).unwrap_or_default();
        let Some(job) = self.jobs.job(&job_id) else {
            return self.refuse(
                request,
                Status::NotFound,
                "UNKNOWN_JOB",
                &format!("no job {job_id}"),
            );
        };
        let state = job.state().as_str();
        let context = self.recovery_surface_context(&job_id);
        let mut payload = Vec::new();
        arkforge_ipc::wire::write_string(&mut payload, 1, &job_id);
        if state == "outcomeUnknown" {
            arkforge_ipc::wire::write_string(&mut payload, 2, "stillUnknown");
            arkforge_ipc::wire::write_string(
                &mut payload,
                3,
                if context.is_some() {
                    "the safe read-only face cannot establish every possible persistent effect; \
                     the original outcome remains immutable"
                } else {
                    "the durable job was recovered without its private plan, so its possible \
                     effects cannot be bounded in this daemon process"
                },
            );
        } else {
            arkforge_ipc::wire::write_string(&mut payload, 2, "nothingToReconcile");
            arkforge_ipc::wire::write_string(&mut payload, 3, "the job has no unresolved outcome");
        }
        arkforge_ipc::wire::write_string(
            &mut payload,
            4,
            context
                .as_ref()
                .map(|context| completeness_name(context.possible.completeness))
                .unwrap_or("unbounded"),
        );
        if let Some(context) = &context {
            for effect in &context.possible.effects.persistent {
                arkforge_ipc::wire::write_string(&mut payload, 5, &persistent_effect_name(effect));
            }
        }
        arkforge_ipc::wire::write_string(&mut payload, 6, state);
        self.ok(request, payload)
    }

    /// Assesses whether a distinct complete-overwrite plan may supersede an
    /// unknown outcome. This never starts or replays either plan.
    fn plan_superseding_recovery(&self, request: &Request) -> Response {
        let job_id = first_string_field(&request.payload, 1).unwrap_or_default();
        let Some(job) = self.jobs.job(&job_id) else {
            return self.refuse(
                request,
                Status::NotFound,
                "UNKNOWN_JOB",
                &format!("no job {job_id}"),
            );
        };
        let mut payload = Vec::new();
        arkforge_ipc::wire::write_string(&mut payload, 1, &job_id);
        let context = self.recovery_surface_context(&job_id);
        let assessment = if job.state().as_str() != "outcomeUnknown" {
            SupersedingRecoveryAssessment::Ineligible(RecoveryBlocker::NothingToRecover)
        } else if let Some(context) = &context {
            assess_superseding_recovery(&context.possible, &context.declaration)
        } else {
            SupersedingRecoveryAssessment::Ineligible(RecoveryBlocker::EffectsUnbounded)
        };
        match &assessment {
            SupersedingRecoveryAssessment::Eligible { covers } => {
                arkforge_ipc::wire::write_bool(&mut payload, 2, true);
                for effect in covers {
                    arkforge_ipc::wire::write_string(
                        &mut payload,
                        5,
                        &persistent_effect_name(effect),
                    );
                }
            }
            SupersedingRecoveryAssessment::Ineligible(blocker) => {
                arkforge_ipc::wire::write_bool(&mut payload, 2, false);
                arkforge_ipc::wire::write_string(&mut payload, 3, recovery_blocker_code(blocker));
                arkforge_ipc::wire::write_string(&mut payload, 4, &blocker.to_string());
            }
        }
        if let Some(context) = context
            && let Some(contract) = context.contract
        {
            arkforge_ipc::wire::write_string(&mut payload, 6, contract.id.as_str());
            arkforge_ipc::wire::write_string(&mut payload, 7, &contract.version.to_string());
            arkforge_ipc::wire::write_string(&mut payload, 8, &contract.digest.to_hex());
        }
        self.ok(request, payload)
    }

    /// A typed operator/agent guide available on the public read-only socket.
    fn get_recovery_guide(&self, request: &Request) -> Response {
        let job_id = first_string_field(&request.payload, 1).unwrap_or_default();
        let Some(job) = self.jobs.job(&job_id) else {
            return self.refuse(
                request,
                Status::NotFound,
                "UNKNOWN_JOB",
                &format!("no job {job_id}"),
            );
        };
        let context = self.recovery_surface_context(&job_id);
        let mut payload = Vec::new();
        arkforge_ipc::wire::write_string(&mut payload, 1, &job_id);
        arkforge_ipc::wire::write_string(&mut payload, 2, job.state().as_str());
        arkforge_ipc::wire::write_bool(&mut payload, 3, true);
        arkforge_ipc::wire::write_bool(&mut payload, 4, true);
        arkforge_ipc::wire::write_string(
            &mut payload,
            5,
            "do not replay the original intent or reuse its permit",
        );
        arkforge_ipc::wire::write_string(
            &mut payload,
            5,
            "ask the paired authority to validate the same target, artifact, toolchain and \
             uncertain-effect set",
        );
        arkforge_ipc::wire::write_string(
            &mut payload,
            5,
            "if eligible, materialize and authorize a distinct complete-overwrite execution",
        );
        let supported = context
            .as_ref()
            .is_some_and(|context| context.declaration.supports_complete_overwrite);
        arkforge_ipc::wire::write_bool(&mut payload, 6, supported);
        if let Some(context) = context
            && let Some(contract) = context.contract
        {
            arkforge_ipc::wire::write_string(&mut payload, 7, contract.id.as_str());
            arkforge_ipc::wire::write_string(&mut payload, 8, &contract.version.to_string());
            arkforge_ipc::wire::write_string(&mut payload, 9, &contract.digest.to_hex());
        }
        self.ok(request, payload)
    }

    fn recovery_surface_context(&self, job_id: &str) -> Option<RecoverySurfaceContext> {
        let stored = self.stored_plan_for_job(job_id)?;
        let profile = self.profiles.get(stored.envelope.profile.id.as_str())?;
        let job = self.jobs.job(job_id)?;
        Some(RecoverySurfaceContext {
            possible: assess_possible_effects(
                job.journal(),
                &stored.private_plan,
                &profile.data_impact,
            ),
            declaration: profile.recovery.clone(),
            contract: stored.envelope.recovery_contract,
        })
    }

    fn cancel_job(&mut self, request: &Request) -> Response {
        let job_id = first_string_field(&request.payload, 1).unwrap_or_default();
        match self.jobs.cancel(&job_id, self.clock.now_epoch_ms()) {
            Ok(state) => {
                let mut payload = Vec::new();
                arkforge_ipc::wire::write_string(&mut payload, 1, state.as_str());
                self.ok(request, payload)
            }
            // "No such job" and "cancelling this job would hide an unresolved
            // effect" are different answers to different questions, and an
            // operator needs to tell them apart.
            Err(crate::jobs::JobError::UnknownJob) => self.refuse(
                request,
                Status::NotFound,
                "UNKNOWN_JOB",
                &format!("no job {job_id}"),
            ),
            Err(error) => self.refuse(request, Status::Refused, error.code(), &error.to_string()),
        }
    }

    /// Answers an admission the daemon asked for on the watchJob stream.
    fn submit_step_permit(&mut self, request: &Request) -> Response {
        let Ok(submission) = SubmitStepPermitRequest::decode(&request.payload) else {
            return self.refuse(
                request,
                Status::InvalidArgument,
                "BAD_SUBMISSION",
                "submitStepPermit payload does not decode",
            );
        };
        if !submission.is_well_formed() {
            return self.refuse(
                request,
                Status::InvalidArgument,
                "SUBMISSION_AMBIGUOUS",
                "a submission carries a permit or a refusal, never both and never neither",
            );
        }
        let Some(secret) = self.pairing.clone() else {
            return self.refuse(
                request,
                Status::Unavailable,
                arkforge_engine::ExecutionBlocker::NoPairedAuthority.code(),
                &arkforge_engine::ExecutionBlocker::NoPairedAuthority.to_string(),
            );
        };
        let Some(stored) = self.stored_plan_for_job(&submission.job_id) else {
            return self.refuse(
                request,
                Status::NotFound,
                "UNKNOWN_JOB",
                &format!("no job {}", submission.job_id),
            );
        };

        let permit = if submission.permit_cbor.is_empty() {
            None
        } else {
            match StepPermit::from_canonical_bytes(&submission.permit_cbor) {
                Ok(permit) => Some((
                    permit,
                    submission.integrity_tag.clone(),
                    submission.pairing_epoch,
                )),
                Err(error) => {
                    return self.refuse(
                        request,
                        Status::InvalidArgument,
                        "PERMIT_NOT_DECODABLE",
                        &error.to_string(),
                    );
                }
            }
        };

        let Some(profile) = self
            .profiles
            .get(stored.envelope.profile.id.as_str())
            .cloned()
        else {
            return self.refuse(
                request,
                Status::NotFound,
                "PROFILE_NOT_LOADED",
                &format!(
                    "the plan names profile {}, which this daemon has not loaded",
                    stored.envelope.profile.id
                ),
            );
        };
        let current_facts = if permit.is_some() {
            match self.current_facts_for_job(&submission.job_id) {
                Ok(facts) => Some(facts),
                Err(error) => {
                    return self.ok(
                        request,
                        SubmissionOutcome::rejected("CURRENT_FACTS_UNAVAILABLE", error).encode(),
                    );
                }
            }
        } else {
            None
        };
        let result = self.jobs.submit_permit(
            &submission.job_id,
            &submission.request_id,
            permit,
            &submission.refusal,
            &secret,
            &stored.envelope,
            &stored.private_plan,
            &profile,
            current_facts,
            self.clock.now_epoch_ms(),
        );
        if matches!(
            result,
            Err(crate::jobs::JobError::SnapshotExpired)
                | Err(crate::jobs::JobError::ContinuityBroken(_))
        ) {
            let _ = self.publish_live_admission(&submission.job_id, false);
        }
        let outcome = match result {
            Ok(()) => SubmissionOutcome::accepted(),
            Err(error) => SubmissionOutcome::rejected(error.code(), error.to_string()),
        };
        self.ok(request, outcome.encode())
    }

    /// Records what the authority's own control channel observed.
    fn submit_control_receipt(&mut self, request: &Request) -> Response {
        let Ok(receipt) = SubmitManagedControlReceiptRequest::decode(&request.payload) else {
            return self.refuse(
                request,
                Status::InvalidArgument,
                "BAD_RECEIPT",
                "submitManagedControlReceipt payload does not decode",
            );
        };
        if self.pairing.is_none() {
            return self.refuse(
                request,
                Status::Unavailable,
                arkforge_engine::ExecutionBlocker::NoPairedAuthority.code(),
                &arkforge_engine::ExecutionBlocker::NoPairedAuthority.to_string(),
            );
        }
        let Some(stored) = self.stored_plan_for_job(&receipt.job_id) else {
            return self.refuse(
                request,
                Status::NotFound,
                "UNKNOWN_JOB",
                &format!("no job {}", receipt.job_id),
            );
        };
        let result = self.jobs.submit_control_receipt(
            &receipt,
            &stored.envelope,
            &stored.private_plan,
            self.clock.now_epoch_ms(),
        );
        if result.is_ok() && receipt.accepted {
            // A successful managed control action is the authority's proof of
            // the mode transition. The old session must not be reused across
            // its detach/re-enumeration; the next admission opens a unique new
            // observation and the authority checks its raw identity facts.
            self.job_sessions.remove(&receipt.job_id);
            self.authorized_rebinds.insert(receipt.job_id.clone());
            let _ = self.publish_live_admission(&receipt.job_id, true);
        }
        let outcome = match result {
            Ok(()) => SubmissionOutcome::accepted(),
            Err(error) => SubmissionOutcome::rejected(error.code(), error.to_string()),
        };
        self.ok(request, outcome.encode())
    }

    /// The stored plan a job is running.
    ///
    /// Looked up through the job rather than taken from the caller: a
    /// submission that could name its own plan could answer one job's
    /// admission with another job's permit.
    fn stored_plan_for_job(&self, job_id: &str) -> Option<StoredPlan> {
        let job = self.jobs.job(job_id)?;
        let plan_id = PlanId::new(job.plan_id()).ok()?;
        self.engine
            .plans()
            .get(&plan_id, job.plan_digest())
            .ok()
            .cloned()
    }

    fn materialize_plan(&mut self, request: &Request) -> Response {
        let artifact_id = first_string_field(&request.payload, 1).unwrap_or_default();
        let profile_id = first_string_field(&request.payload, 2).unwrap_or_default();
        let observation_id = first_string_field(&request.payload, 3).unwrap_or_default();
        let intent = first_string_field(&request.payload, 4).unwrap_or_default();
        let requested_toolchain_id = first_string_field(&request.payload, 5).unwrap_or_default();
        let authority_namespace = first_string_field(&request.payload, 6).unwrap_or_default();
        let binding_id = first_string_field(&request.payload, 7).unwrap_or_default();
        let binding_revision = first_u64_field(&request.payload, 8).unwrap_or_default();
        let stable_identity = first_bytes_field(&request.payload, 9).unwrap_or_default();
        let execution_purpose = first_string_field(&request.payload, 10).unwrap_or_default();

        if intent != FlashIntent::FullRestore.as_str() {
            return self.refuse(
                request,
                Status::InvalidArgument,
                "UNSUPPORTED_FLASH_INTENT",
                "materializePlan currently requires intent=fullRestore",
            );
        }
        let Some(execution_purpose) = ExecutionPurpose::parse(&execution_purpose) else {
            return self.refuse(
                request,
                Status::InvalidArgument,
                "UNSUPPORTED_EXECUTION_PURPOSE",
                "materializePlan requires executionPurpose=primaryFlash or supersedingRecovery",
            );
        };

        let Some(manifest) = self.manifests.get(&artifact_id).cloned() else {
            return self.refuse(
                request,
                Status::NotFound,
                "ARTIFACT_NOT_INSPECTED",
                "materializePlan requires an inspected artifact",
            );
        };
        let Some(profile) = self.profiles.get(&profile_id).cloned() else {
            return self.refuse(
                request,
                Status::NotFound,
                "PROFILE_NOT_FOUND",
                &format!("no loaded profile {profile_id}"),
            );
        };
        if execution_purpose == ExecutionPurpose::SupersedingRecovery
            && !profile.recovery.supports_complete_overwrite
        {
            return self.refuse(
                request,
                Status::Refused,
                "RECOVERY_CONTRACT_UNAVAILABLE",
                "the selected profile publishes no complete-overwrite recovery contract",
            );
        }
        let toolchain = if profile
            .artifact_formats
            .iter()
            .any(|format| format.as_str() == pac::FORMAT_ID)
        {
            research_toolchain_identity()
        } else {
            self.bound_rockchip_toolchain_identity()
        };
        if requested_toolchain_id != toolchain.id.as_str() {
            return self.refuse(
                request,
                Status::Refused,
                "TOOLCHAIN_ID_MISMATCH",
                &format!(
                    "materializePlan requested toolchain {requested_toolchain_id}; this daemon binds {}",
                    toolchain.id
                ),
            );
        }
        let Ok(authority_namespace) = AuthorityNamespace::new(authority_namespace) else {
            return self.refuse(
                request,
                Status::InvalidArgument,
                "BAD_AUTHORITY_NAMESPACE",
                "authorityNamespace is not a usable identifier",
            );
        };
        let Ok(binding_id) = OpaqueId::new(binding_id) else {
            return self.refuse(
                request,
                Status::InvalidArgument,
                "BAD_BINDING_ID",
                "bindingId is not a usable identifier",
            );
        };
        let Some(stable_identity_digest) = digest_from_bytes(&stable_identity) else {
            return self.refuse(
                request,
                Status::InvalidArgument,
                "BAD_STABLE_IDENTITY_DIGEST",
                "stableIdentitySha256 must contain exactly 32 bytes",
            );
        };
        if binding_revision == 0 {
            return self.refuse(
                request,
                Status::InvalidArgument,
                "BAD_BINDING_REVISION",
                "bindingRevision must be greater than zero",
            );
        }

        let Some(provider) = provider_for(&profile, &self.rockchip, &self.unisoc) else {
            return self.refuse(
                request,
                Status::NotFound,
                "NO_PROVIDER_FOR_PROFILE",
                &format!(
                    "no registered provider handles the artifact formats profile {} declares",
                    profile.id
                ),
            );
        };

        let mut probed = None;
        for (transport_index, transport) in self.transports.iter().enumerate() {
            let Ok(observations) =
                transport.discover(&TypedDiscoveryFilter::default(), self.clock.now_epoch_ms())
            else {
                continue;
            };
            if let Some(observation) = observations
                .iter()
                .find(|candidate| candidate.observation_id.as_str() == observation_id)
            {
                probed = provider
                    .probe(&ProbeContext {
                        transport: transport.as_ref(),
                        observation,
                        profile: &profile,
                    })
                    .ok()
                    .map(|probe| (probe, transport_index, observation.clone()));
                break;
            }
        }
        let Some((probe, transport_index, plan_observation)) = probed else {
            return self.refuse(
                request,
                Status::NotFound,
                "OBSERVATION_NOT_FOUND",
                &format!("no probe for observation {observation_id}"),
            );
        };

        let materialize = MaterializeRequest {
            plan_id: PlanId::new(format!("PLAN-{}", &artifact_id[..12]))
                .unwrap_or_else(|_| PlanId::new("PLAN-UNNAMED").expect("literal")),
            execution_purpose,
            intent: FlashIntent::FullRestore,
            artifact: &manifest,
            artifact_id: OpaqueId::new(&artifact_id[..32])
                .unwrap_or_else(|_| OpaqueId::new("ART-UNNAMED").expect("literal identifier")),
            profile: &profile,
            probe: &probe,
            authority_binding: AuthorityBindingRef {
                authority_namespace,
                binding_id,
                binding_revision,
                stable_identity_digest,
            },
            // The native implementation this daemon actually bound, not a
            // source-revision constant. A plan materialized for one build and
            // started on another is refused because the executable digest is
            // part of the maturity combination.
            toolchain,
            host_platform: HostPlatform::current(),
            driver_facts_digest: driver_facts_digest(),
            evidence_set_digest: evidence_set_digest(),
            created_at_epoch_ms: self.clock.now_epoch_ms(),
            plan_lifetime_ms: 3_600_000,
        };

        let materialized =
            match provider.materialize_with_private_plan(&materialize, &self.maturity) {
                Ok(materialized) => materialized,
                Err(error) => {
                    return self.refuse(
                        request,
                        Status::Refused,
                        "MATERIALIZATION_REFUSED",
                        &error.to_string(),
                    );
                }
            };
        // Store the executable plan and its private half together. A job can
        // only ever be started against a plan the daemon materialized itself,
        // which is what keeps `startExecution` free of anything a caller could
        // supply (architecture.md 15.3).
        if let (PlanMaterialization::Executable(envelope), Some(private_plan)) =
            (&materialized.materialization, &materialized.private_plan)
        {
            if let Err(error) = self.engine.plans_mut().insert(StoredPlan {
                envelope: (**envelope).clone(),
                private_plan: private_plan.clone(),
            }) {
                return self.refuse(
                    request,
                    Status::Internal,
                    "PLAN_NOT_STORABLE",
                    &error.to_string(),
                );
            }
            self.plan_observations.insert(
                envelope.plan_digest.to_hex(),
                PlanObservationContext {
                    transport_index,
                    observation: plan_observation,
                },
            );
        }

        match Ok::<_, arkforge_provider::ProviderError>(materialized.materialization) {
            Ok(PlanMaterialization::Assessment(assessment)) => {
                let mut message = Assessment {
                    availability: assessment.availability.as_str().to_string(),
                    unavailable_reason: match &assessment.availability {
                        arkforge_core::plan::ExecutionAvailability::Unavailable { reason } => {
                            reason.clone()
                        }
                        _ => String::new(),
                    },
                    ..Assessment::default()
                };
                for unknown in &assessment.unknowns {
                    message.unknowns.push(KeyValue {
                        key: unknown.id.to_string(),
                        value: unknown.summary.clone(),
                    });
                }
                for requirement in &assessment.evidence_requirements {
                    message.evidence_requirements.push(KeyValue {
                        key: requirement.id.to_string(),
                        value: requirement.description.clone(),
                    });
                }
                for effect in &assessment.known_effects.persistent {
                    message.known_persistent_effects.push(encode_effect(effect));
                }
                message.data_impact = encode_data_impact(&assessment.known_effects.data_impact);
                self.ok(
                    request,
                    MaterializePlanResponse::Assessment(message).encode(),
                )
            }
            Ok(PlanMaterialization::Executable(envelope)) => {
                let mut plan = ExecutablePlan {
                    plan_id: envelope.plan_id.to_string(),
                    plan_sha256: envelope.plan_digest.to_hex(),
                    provider_execution_plan_sha256: envelope
                        .provider_execution_plan_digest
                        .to_hex(),
                    public_projection_sha256: envelope.public_projection_digest.to_hex(),
                    expires_at_epoch_ms: envelope.expires_at_epoch_ms,
                    execution_purpose: envelope.execution_purpose.as_str().to_string(),
                    ..ExecutablePlan::default()
                };
                for step in &envelope.public_steps {
                    plan.public_steps.push(encode_step(step));
                }
                for effect in &envelope.effect_set.persistent {
                    plan.persistent_effects.push(encode_effect(effect));
                }
                for effect in &envelope.effect_set.transient {
                    plan.transient_effects.push(encode_transient(effect));
                }
                plan.data_impact = encode_data_impact(&envelope.effect_set.data_impact);
                self.ok(request, MaterializePlanResponse::Plan(plan).encode())
            }
            Err(error) => self.refuse(
                request,
                Status::Refused,
                "MATERIALIZATION_REFUSED",
                &error.to_string(),
            ),
        }
    }
}

/// Every blocker, in one message.
///
/// All of them rather than the first: an operator fixing one at a time should
/// not have to discover the second by trying again.
fn blocker_list(blockers: &[arkforge_engine::ExecutionBlocker]) -> String {
    blockers
        .iter()
        .map(|blocker| format!("{}: {blocker}", blocker.code()))
        .collect::<Vec<_>>()
        .join("; ")
}

fn admission_facts(
    envelope: &arkforge_core::plan::FlashPlanEnvelope,
    observation: DeviceObservation,
    transport_session_digest: Sha256Digest,
) -> Result<AdmissionFacts, String> {
    Ok(AdmissionFacts {
        observation,
        transport_session_digest,
        provider_facts_digest: digest_canonical(Domain::ProviderFacts, &envelope.provider)
            .map_err(|error| error.to_string())?,
        toolchain_facts_digest: digest_canonical(Domain::ToolchainFacts, &envelope.toolchain)
            .map_err(|error| error.to_string())?,
        artifact_facts_digest: digest_canonical(Domain::ArtifactFacts, &envelope.artifact)
            .map_err(|error| error.to_string())?,
    })
}

/// Whether a completed native dispatch establishes a sealed mode transition
/// and therefore authorizes one exact rebind for the next admission.
///
/// A declared transition without semantic success proves nothing, and missing
/// mode facts are not inferred. Same-mode steps retain their open continuity
/// session.
fn successful_dispatch_requires_rebind(
    disposition: ActionDisposition,
    expected_before: Option<&DeviceMode>,
    expected_after: Option<&DeviceMode>,
) -> bool {
    disposition == ActionDisposition::SemanticSuccess
        && matches!(
            (expected_before, expected_after),
            (Some(before), Some(after)) if before != after
        )
}

fn encode_job_summary(job: &crate::jobs::Job) -> JobSummary {
    JobSummary {
        job_id: job.job_id().to_string(),
        plan_id: job.plan_id().to_string(),
        plan_sha256: job.plan_digest().as_bytes().to_vec(),
        state: job.state().as_str().to_string(),
        terminal: job.state().is_terminal() || job.stopped().is_some(),
        current_step_id: job.current_step_id(),
        completed_steps: job.completed_steps() as u64,
        total_steps: job.total_steps() as u64,
        last_sequence: job.last_sequence(),
        stopped_reason: job.stopped().map(ToString::to_string).unwrap_or_default(),
    }
}

fn next_expected_mode(
    stored: &StoredPlan,
    job: Option<&crate::jobs::Job>,
) -> Result<Option<arkforge_core::DeviceMode>, String> {
    let job = job.ok_or_else(|| "the job no longer exists".to_string())?;
    Ok(job.expected_mode(&stored.envelope))
}

/// Chooses the provider by the artifact formats the *profile* declares.
///
/// The daemon does not decide that a device belongs to a vendor; the profile
/// says which formats apply, and a provider that handles one of them takes it.
/// A profile whose formats nobody handles gets a refusal, not a default.
fn provider_for<'a>(
    profile: &DeviceProfile,
    rockchip: &'a RockchipProvider,
    unisoc: &'a UnisocProvider,
) -> Option<&'a dyn FlashProvider> {
    if profile
        .artifact_formats
        .iter()
        .any(|format| format.as_str() == dayu200::FORMAT_ID)
    {
        return Some(rockchip);
    }
    if profile
        .artifact_formats
        .iter()
        .any(|format| format.as_str() == pac::FORMAT_ID)
    {
        return Some(unisoc);
    }
    None
}

/// Reads a container with the parser its framing indicates.
///
/// gzip's magic is a fact of the container, not a guess about the device, so
/// sniffing it is legitimate. Anything else falls to the research observer,
/// which claims nothing about what it read.
fn inspect_container<R: Read>(mut source: R) -> Result<ArtifactManifest, String> {
    let mut magic = [0u8; 2];
    let mut filled = 0usize;
    while filled < magic.len() {
        match source.read(&mut magic[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(error) => return Err(error.to_string()),
        }
    }
    let head = magic[..filled].to_vec();
    let rejoined = std::io::Read::chain(std::io::Cursor::new(head), source);

    if filled == 2 && magic[0] == 0x1f && magic[1] == 0x8b {
        dayu200::inspect(rejoined).map_err(|error| error.to_string())
    } else {
        pac::inspect(rejoined)
            .map(|(manifest, _)| manifest)
            .map_err(|error| error.to_string())
    }
}

fn driver_facts_digest() -> Sha256Digest {
    arkforge_core::digest::sha256(b"arkforge/driver-facts/none-measured")
}

fn evidence_set_digest() -> Sha256Digest {
    arkforge_core::digest::sha256(b"AD-003,AD-005,AD-006")
}

/// The research backend the Unisoc provider dispatches through — which is to
/// say, no backend at all.
fn research_toolchain_identity() -> ToolchainIdentity {
    ToolchainIdentity {
        id: OpaqueId::new("research-inspect").expect("literal identifier"),
        kind: ToolchainKind::Replay,
        version: Version::new(0, 1, 0),
        backend_digest: arkforge_core::digest::sha256(b"arkforge/research-inspect"),
        upstream_ref: None,
    }
}

/// The in-process RockUSB backend compiled into this `arkforged` build.
pub fn native_toolchain_identity(backend_digest: Sha256Digest) -> ToolchainIdentity {
    ToolchainIdentity {
        id: OpaqueId::new("arkforged-native-rockusb").expect("literal identifier"),
        kind: ToolchainKind::NativeProtocol,
        version: Version::new(0, 1, 0),
        backend_digest,
        upstream_ref: option_env!("ARKFORGE_SOURCE_REVISION").map(str::to_string),
    }
}

/// Identity used only for assessments built before the daemon binds its own
/// executable. It can never pass execution readiness because no dispatcher is
/// bound; the digest exists only to keep the assessment's maturity key exact.
fn unbound_native_toolchain_identity() -> ToolchainIdentity {
    native_toolchain_identity(arkforge_core::digest::sha256(
        b"arkforge/native-rockusb/unbound",
    ))
}

fn replay_toolchain_identity() -> ToolchainIdentity {
    ToolchainIdentity {
        id: OpaqueId::new("transcript-replay").expect("literal identifier"),
        kind: ToolchainKind::Replay,
        version: Version::new(1, 0, 0),
        backend_digest: arkforge_core::digest::sha256(b"arkforge/transcript-replay"),
        upstream_ref: None,
    }
}

struct ImportPayload {
    expected_size_bytes: u64,
    expected_sha256: Option<String>,
}

fn decode_import_request(payload: &[u8]) -> Result<ImportPayload, String> {
    let mut expected_size_bytes = 0u64;
    let mut expected_sha256 = None;
    let mut reader = arkforge_ipc::wire::Reader::new(payload);
    loop {
        match reader.next_field() {
            Ok(Some((field, value))) => match field {
                1 => expected_size_bytes = value.as_u64().map_err(|e| e.to_string())?,
                2 => {
                    expected_sha256 = Some(value.as_str(2).map_err(|e| e.to_string())?.to_string())
                }
                _ => {}
            },
            Ok(None) => break,
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(ImportPayload {
        expected_size_bytes,
        expected_sha256,
    })
}

fn first_string_field(payload: &[u8], wanted: u32) -> Option<String> {
    let mut reader = arkforge_ipc::wire::Reader::new(payload);
    while let Ok(Some((field, value))) = reader.next_field() {
        if field == wanted {
            return value.as_str(field).ok().map(str::to_string);
        }
    }
    None
}

fn first_u64_field(payload: &[u8], wanted: u32) -> Option<u64> {
    let mut reader = arkforge_ipc::wire::Reader::new(payload);
    while let Ok(Some((field, value))) = reader.next_field() {
        if field == wanted {
            return value.as_u64().ok();
        }
    }
    None
}

fn first_bytes_field(payload: &[u8], wanted: u32) -> Option<Vec<u8>> {
    let mut reader = arkforge_ipc::wire::Reader::new(payload);
    while let Ok(Some((field, value))) = reader.next_field() {
        if field == wanted {
            return value.as_bytes().ok().map(ToOwned::to_owned);
        }
    }
    None
}

fn digest_from_bytes(bytes: &[u8]) -> Option<Sha256Digest> {
    if bytes.len() != 32 {
        return None;
    }
    let mut digest = [0u8; 32];
    digest.copy_from_slice(bytes);
    Some(Sha256Digest::from_bytes(digest))
}

fn encode_manifest(manifest: &ArtifactManifest) -> InspectArtifactResponse {
    let mut response = InspectArtifactResponse {
        format_id: manifest.format.id.to_string(),
        content_sha256: manifest.content_digest.to_hex(),
        size_bytes: manifest.size_bytes,
        confidence: manifest.confidence.as_str().to_string(),
        manifest_sha256: manifest
            .digest()
            .map(|digest| digest.to_hex())
            .unwrap_or_default(),
        unclassified_members: manifest.unclassified_members.clone(),
        ..InspectArtifactResponse::default()
    };
    for member in &manifest.members {
        response.members.push(ArchiveMember {
            path: member.path.clone(),
            size_bytes: member.size_bytes,
            sha256: member.sha256.to_hex(),
            role: member.role.as_str().to_string(),
        });
    }
    if let Some(table) = &manifest.partition_table {
        for entry in &table.entries {
            response.partitions.push(PartitionEntry {
                index: entry.index,
                name: entry.name.clone(),
                offset_sectors: entry.offset_sectors,
                size_sectors: entry.size_sectors,
                attribute: entry
                    .attribute
                    .map(|attribute| attribute.as_str().to_string())
                    .unwrap_or_default(),
                grammar_branch: entry.grammar_branch.as_str().to_string(),
            });
        }
    }
    for (key, value) in &manifest.build_facts {
        response.build_facts.push(KeyValue {
            key: key.to_string(),
            value: value.clone(),
        });
    }
    for unknown in &manifest.execution_relevant_unknowns {
        response.execution_relevant_unknowns.push(KeyValue {
            key: unknown.id.to_string(),
            value: unknown.summary.clone(),
        });
    }
    response
}

fn encode_observation(observation: &DeviceObservation) -> Vec<u8> {
    let mut out = Vec::new();
    arkforge_ipc::wire::write_string(&mut out, 1, observation.observation_id.as_str());
    arkforge_ipc::wire::write_uint64(&mut out, 2, observation.observed_at_epoch_ms);
    arkforge_ipc::wire::write_string(&mut out, 3, observation.mode.as_str());
    arkforge_ipc::wire::write_string(&mut out, 4, &observation.topology_digest.to_hex());
    arkforge_ipc::wire::write_string(&mut out, 5, &observation.descriptor_digest.to_hex());
    arkforge_ipc::wire::write_string(&mut out, 6, observation.identity_strength.as_str());
    arkforge_ipc::wire::write_bool(&mut out, 7, observation.malformed_descriptor);
    for fact in &observation.protocol_identity {
        arkforge_ipc::wire::write_message(
            &mut out,
            8,
            &KeyValue {
                key: fact.key.to_string(),
                value: fact.value.clone(),
            }
            .encode(),
        );
    }
    out
}

fn encode_step(step: &arkforge_core::PublicFlashStep) -> PublicStep {
    PublicStep {
        step_id: step.step_id.to_string(),
        kind: step.kind.as_str().to_string(),
        effect: step.effect.as_str().to_string(),
        cancellation: step.cancellation.as_str().to_string(),
        binding: step.binding.as_str().to_string(),
        semantic_target: match &step.semantic_target {
            Some(arkforge_core::SemanticTarget::Partition(id)) => format!("partition:{id}"),
            Some(arkforge_core::SemanticTarget::RawRegion(id)) => format!("region:{id}"),
            Some(arkforge_core::SemanticTarget::BootMetadata(field)) => {
                format!("bootMetadata:{}", field.as_str())
            }
            Some(arkforge_core::SemanticTarget::Device) => "device".to_string(),
            None => String::new(),
        },
        content_sha256: step
            .content_digest
            .map(|digest| digest.to_hex())
            .unwrap_or_default(),
        expected_mode_before: step
            .expected_mode_before
            .as_ref()
            .map(|mode| mode.to_string())
            .unwrap_or_default(),
        expected_mode_after: step
            .expected_mode_after
            .as_ref()
            .map(|mode| mode.to_string())
            .unwrap_or_default(),
        private_action_sha256: step.private_action_digest.to_hex(),
    }
}

fn encode_effect(effect: &PersistentEffect) -> Effect {
    match effect {
        PersistentEffect::WritePartition {
            partition,
            range,
            content,
        } => Effect {
            kind: "writePartition".into(),
            target: partition.to_string(),
            range_start: range.start,
            range_length: range.length,
            content_sha256: content.to_hex(),
        },
        PersistentEffect::ErasePartition { partition, range } => Effect {
            kind: "erasePartition".into(),
            target: partition.to_string(),
            range_start: range.start,
            range_length: range.length,
            content_sha256: String::new(),
        },
        PersistentEffect::WriteRawRegion {
            region,
            range,
            content,
        } => Effect {
            kind: "writeRawRegion".into(),
            target: region.to_string(),
            range_start: range.start,
            range_length: range.length,
            content_sha256: content.to_hex(),
        },
        PersistentEffect::ReplacePartitionTable { layout_digest } => Effect {
            kind: "replacePartitionTable".into(),
            target: String::new(),
            range_start: 0,
            range_length: 0,
            content_sha256: layout_digest.to_hex(),
        },
        PersistentEffect::ChangeBootMetadata { field, .. } => Effect {
            kind: "changeBootMetadata".into(),
            target: field.as_str().to_string(),
            range_start: 0,
            range_length: 0,
            content_sha256: String::new(),
        },
    }
}

fn encode_transient(effect: &TransientEffect) -> Effect {
    match effect {
        TransientEffect::EnterMode { from, to } => Effect {
            kind: "enterMode".into(),
            target: format!("{from}->{to}"),
            ..Effect::default()
        },
        TransientEffect::Reboot { target_mode } => Effect {
            kind: "reboot".into(),
            target: target_mode.to_string(),
            ..Effect::default()
        },
        TransientEffect::LoadEphemeralAgent { stage, content, .. } => Effect {
            kind: "loadEphemeralAgent".into(),
            target: stage.as_str().to_string(),
            content_sha256: content.to_hex(),
            ..Effect::default()
        },
        TransientEffect::UsbDetachReattach { expectation_digest } => Effect {
            kind: "usbDetachReattach".into(),
            content_sha256: expectation_digest.to_hex(),
            ..Effect::default()
        },
    }
}

fn encode_data_impact(impact: &arkforge_core::DataImpact) -> Vec<KeyValue> {
    vec![
        KeyValue {
            key: "userdata".into(),
            value: impact.userdata.as_str().to_string(),
        },
        KeyValue {
            key: "calibration".into(),
            value: impact.calibration.as_str().to_string(),
        },
        KeyValue {
            key: "nonVolatileConfig".into(),
            value: impact.non_volatile_config.as_str().to_string(),
        },
        KeyValue {
            key: "secureStorage".into(),
            value: impact.secure_storage.as_str().to_string(),
        },
    ]
}

fn completeness_name(completeness: EffectSetCompleteness) -> &'static str {
    match completeness {
        EffectSetCompleteness::Bounded => "bounded",
        EffectSetCompleteness::Unbounded => "unbounded",
    }
}

fn persistent_effect_name(effect: &PersistentEffect) -> String {
    match effect {
        PersistentEffect::WritePartition { partition, .. }
        | PersistentEffect::ErasePartition { partition, .. } => {
            format!("partition:{partition}")
        }
        PersistentEffect::WriteRawRegion { region, .. } => format!("rawRegion:{region}"),
        PersistentEffect::ReplacePartitionTable { .. } => "partitionTable".into(),
        PersistentEffect::ChangeBootMetadata { field, .. } => {
            format!("bootMetadata:{}", field.as_str())
        }
    }
}

fn recovery_blocker_code(blocker: &RecoveryBlocker) -> &'static str {
    match blocker {
        RecoveryBlocker::EffectsUnbounded => "effectsUnbounded",
        RecoveryBlocker::NoPublishedCoverage { .. } => "noPublishedCoverage",
        RecoveryBlocker::EffectOutsideCoverage { .. } => "effectOutsideCoverage",
        RecoveryBlocker::NothingToRecover => "nothingToRecover",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arkforge_core::identity::{MaturityKey, MaturityState};

    const DAYU200_PROFILE: &str = include_str!("../../../profiles/dayu200.yaml");

    fn native_binding_maturity(campaign: Option<&str>) -> MaturityState {
        let root = std::env::temp_dir().join(format!(
            "arkforged-native-maturity-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let profile = arkforge_core::profile::load(DAYU200_PROFILE).unwrap();
        let mut service = Service::new(
            &root,
            vec![profile],
            Vec::new(),
            Clock::Fixed(1_754_380_800_000),
            campaign,
        )
        .unwrap();
        let backend_digest = arkforge_core::digest::sha256(b"one exact arkforged build");
        service.bind_native_dispatcher(BoundToolchain {
            id: OpaqueId::new("arkforged-native-rockusb").unwrap(),
            backend_digest,
        });

        let toolchain = service.bound_rockchip_toolchain_identity();
        assert_eq!(toolchain.kind, ToolchainKind::NativeProtocol);
        assert_eq!(toolchain.backend_digest, backend_digest);
        assert_eq!(
            service
                .readiness
                .dispatcher
                .as_ref()
                .unwrap()
                .backend_digest,
            backend_digest
        );

        let profile = service.profiles.values().next().unwrap();
        let state = service.maturity.lookup(&MaturityKey {
            provider: service.rockchip.identity().clone(),
            profile: profile.identity().unwrap(),
            artifact_format: service.rockchip.descriptor().artifact_formats[0].clone(),
            toolchain,
            host_platform: HostPlatform::current(),
            driver_facts_digest: driver_facts_digest(),
            evidence_set_digest: evidence_set_digest(),
        });
        drop(service);
        let _ = std::fs::remove_dir_all(root);
        state
    }

    #[test]
    fn native_binding_without_a_campaign_is_hardware_gated() {
        assert!(matches!(
            native_binding_maturity(None),
            MaturityState::HardwareGated { .. }
        ));
    }

    #[test]
    fn afa_ac_7_publishes_the_exact_native_build_as_a_hardware_campaign() {
        assert_eq!(
            native_binding_maturity(Some("AFA-AC-7")),
            MaturityState::HardwareCampaign {
                campaign: "AFA-AC-7".into()
            }
        );
    }

    #[test]
    fn only_semantically_confirmed_native_mode_changes_open_an_exact_rebind() {
        let loader = DeviceMode::new("rockusb-loader").unwrap();
        let normal = DeviceMode::new("hdc-normal").unwrap();

        assert!(successful_dispatch_requires_rebind(
            ActionDisposition::SemanticSuccess,
            Some(&loader),
            Some(&normal),
        ));
        assert!(!successful_dispatch_requires_rebind(
            ActionDisposition::SemanticSuccess,
            Some(&loader),
            Some(&loader),
        ));
        assert!(!successful_dispatch_requires_rebind(
            ActionDisposition::OutcomeUnknown,
            Some(&loader),
            Some(&normal),
        ));
        assert!(!successful_dispatch_requires_rebind(
            ActionDisposition::SemanticSuccess,
            None,
            Some(&normal),
        ));
    }
}
