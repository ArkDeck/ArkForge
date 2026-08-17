//! The daemon's request handler, independent of any socket.
//!
//! architecture.md 15.3. Keeping dispatch transport-agnostic is what lets the
//! API-surface tests assert "the public socket cannot start execution" and
//! "startExecution is unavailable" without opening a socket — the properties
//! belong to the service, not to the plumbing.

use arkforge_artifact::cas::{CasQuota, ContentAddressedStore};
use arkforge_artifact::manifest::ArtifactManifest;
use arkforge_artifact::{dayu200, pac};
use arkforge_core::identity::{HostPlatform, ToolchainIdentity, ToolchainKind, Version};
use arkforge_core::ids::{OpaqueId, PlanId};
use arkforge_core::plan::PlanMaterialization;
use arkforge_core::profile::DeviceProfile;
use arkforge_core::{AuthorityBindingRef, AuthorityNamespace, PersistentEffect, Sha256Digest, TransientEffect};
use crate::jobs::JobRegistry;
use arkforge_authority_api::{ControllerPairingSecret, StepPermit};
use arkforge_engine::{BoundToolchain, Engine, ExecutionReadiness, StoredPlan};
use arkforge_ipc::messages::{
    ArchiveMember, Assessment, Effect, ErrorBody, ExecutablePlan, InspectArtifactResponse,
    KeyValue, MaterializePlanResponse, PartitionEntry, PublicStep, Request, Response,
    SubmissionOutcome, SubmitManagedControlReceiptRequest, SubmitStepPermitRequest,
    WatchJobRequest,
};
use arkforge_ipc::{Api, SessionKind, Status};
use arkforge_provider::rockchip::{publish_dayu200_maturity, RockchipProvider};
use arkforge_provider::unisoc::{publish_af_v3_maturity, UnisocProvider};
use arkforge_provider::{
    FlashIntent, FlashProvider, MaterializeRequest, MaturityRegistry, ProbeContext,
};
use arkforge_transport::replay::TranscriptTransport;
use arkforge_transport::usb::UsbTransport;
use arkforge_transport::{transcript, DeviceObservation, DeviceTransport, TypedDiscoveryFilter};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

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
    now_epoch_ms: u64,
    jobs: JobRegistry,
    /// The secret the authority handed this daemon at startup. Held here and
    /// nowhere else; there is no getter.
    pairing: Option<ControllerPairingSecret>,
    /// What this daemon can do, as standing facts. Kept beside the secret
    /// rather than derived from it, because pairing is only half of it.
    readiness: ExecutionReadiness,
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
        now_epoch_ms: u64,
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
                for toolchain in [fixed_tool_identity(), replay_toolchain_identity()] {
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
            now_epoch_ms,
            jobs: JobRegistry::new(store_root.join("jobs")),
            pairing: None,
            readiness: ExecutionReadiness::default(),
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

    /// Binds the fixed tool a dispatcher will run.
    ///
    /// Identity, not a path. What the rest of the daemon needs to know is
    /// whether these are the bytes a plan's maturity was published against;
    /// which file they came from is the host's business.
    pub fn bind_dispatcher(&mut self, toolchain: BoundToolchain) {
        self.readiness.dispatcher = Some(toolchain);
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

    /// Records what a dispatcher observed.
    pub fn complete_dispatch(
        &mut self,
        job_id: &str,
        outcome: crate::jobs::DispatchOutcome,
    ) -> Result<(), String> {
        let stored = self
            .stored_plan_for_job(job_id)
            .ok_or_else(|| format!("no job {job_id}"))?;
        self.jobs
            .complete_dispatch(
                job_id,
                outcome,
                &stored.envelope,
                &stored.private_plan,
                self.now_epoch_ms,
            )
            .map_err(|error| error.to_string())
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
            Api::CancelJob => self.cancel_job(request),
            Api::SubmitStepPermit => self.submit_step_permit(request),
            Api::SubmitManagedControlReceipt => self.submit_control_receipt(request),
            Api::ReconcileJob | Api::PlanSupersedingRecovery | Api::GetRecoveryGuide => self
                .refuse(
                    request,
                    Status::Unavailable,
                    "RECONCILE_SURFACE_UNAVAILABLE",
                    "reconcile and superseding recovery need read-only device observation of a \
                     job that dispatched; this build has the assessment half only \
                     (arkforge_engine::superseding)",
                ),
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
                return self.refuse(request, Status::InvalidArgument, "MALFORMED_REQUEST", &message)
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
                    )
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
                )
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
                )
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
                        )
                    }
                };
                match inspect_container(object) {
                    Ok(manifest) => {
                        self.manifests.insert(artifact_id.clone(), manifest.clone());
                        manifest
                    }
                    Err(error) => {
                        return self.refuse(
                            request,
                            Status::Refused,
                            "ARTIFACT_REJECTED",
                            &error,
                        )
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
            let observations = match transport.discover(&filter, self.now_epoch_ms) {
                Ok(observations) => observations,
                Err(error) => {
                    return self.refuse(
                        request,
                        Status::Internal,
                        "DISCOVERY_FAILED",
                        &error.to_string(),
                    )
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
            let Ok(observations) = transport.discover(&TypedDiscoveryFilter::default(), self.now_epoch_ms)
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

        let readiness = self.readiness.clone();
        let stored = match self.engine.start_execution(&plan_id, expected, &readiness) {
            Ok(stored) => stored.clone(),
            Err(arkforge_engine::EngineError::ExecutionDisabled(blockers)) => {
                let code = blockers
                    .first()
                    .map(|blocker| blocker.code())
                    .unwrap_or("EXECUTION_DISABLED");
                return self.refuse(
                    request,
                    Status::Unavailable,
                    code,
                    &blocker_list(&blockers),
                );
            }
            Err(error) => {
                return self.refuse(
                    request,
                    Status::NotFound,
                    "PLAN_NOT_STARTABLE",
                    &error.to_string(),
                )
            }
        };

        match self
            .jobs
            .start(&stored.envelope, &stored.private_plan, self.now_epoch_ms)
        {
            Ok(job_id) => {
                let mut payload = Vec::new();
                arkforge_ipc::wire::write_string(&mut payload, 1, &job_id);
                self.ok(request, payload)
            }
            Err(error) => self.refuse(
                request,
                Status::Internal,
                error.code(),
                &error.to_string(),
            ),
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

    fn cancel_job(&mut self, request: &Request) -> Response {
        let job_id = first_string_field(&request.payload, 1).unwrap_or_default();
        match self.jobs.cancel(&job_id, self.now_epoch_ms) {
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
            Err(error) => self.refuse(
                request,
                Status::Refused,
                error.code(),
                &error.to_string(),
            ),
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
                    )
                }
            }
        };

        let Some(profile) = self.profiles.get(stored.envelope.profile.id.as_str()).cloned() else {
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
        let outcome = match self.jobs.submit_permit(
            &submission.job_id,
            &submission.request_id,
            permit,
            &submission.refusal,
            &secret,
            &stored.envelope,
            &stored.private_plan,
            &profile,
            self.now_epoch_ms,
        ) {
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
        let outcome = match self.jobs.submit_control_receipt(
            &receipt,
            &stored.envelope,
            &stored.private_plan,
            self.now_epoch_ms,
        ) {
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

        let mut probe = None;
        for transport in &self.transports {
            let Ok(observations) = transport.discover(&TypedDiscoveryFilter::default(), self.now_epoch_ms)
            else {
                continue;
            };
            if let Some(observation) = observations
                .iter()
                .find(|candidate| candidate.observation_id.as_str() == observation_id)
            {
                probe = provider
                    .probe(&ProbeContext {
                        transport: transport.as_ref(),
                        observation,
                        profile: &profile,
                    })
                    .ok();
                break;
            }
        }
        let Some(probe) = probe else {
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
            intent: FlashIntent::FullRestore,
            artifact: &manifest,
            artifact_id: OpaqueId::new(&artifact_id[..32]).unwrap_or_else(|_| {
                OpaqueId::new("ART-UNNAMED").expect("literal identifier")
            }),
            profile: &profile,
            probe: &probe,
            authority_binding: AuthorityBindingRef {
                // A read-only materialization is not bound to an authority
                // target: nothing here can be executed, and inventing a binding
                // would put a target identity into an audit record that no
                // authority issued.
                authority_namespace: AuthorityNamespace::new("unbound").expect("literal"),
                binding_id: OpaqueId::new("UNBOUND").expect("literal identifier"),
                binding_revision: 0,
                stable_identity_digest: probe.facts_digest,
            },
            toolchain: if profile
                .artifact_formats
                .iter()
                .any(|format| format.as_str() == pac::FORMAT_ID)
            {
                research_toolchain_identity()
            } else {
                fixed_tool_identity()
            },
            host_platform: HostPlatform::current(),
            driver_facts_digest: driver_facts_digest(),
            evidence_set_digest: evidence_set_digest(),
            created_at_epoch_ms: self.now_epoch_ms,
            plan_lifetime_ms: 3_600_000,
        };

        let materialized = match provider.materialize_with_private_plan(&materialize, &self.maturity)
        {
            Ok(materialized) => materialized,
            Err(error) => {
                return self.refuse(
                    request,
                    Status::Refused,
                    "MATERIALIZATION_REFUSED",
                    &error.to_string(),
                )
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

/// The pinned rkdeveloptool ArkDeck ships
/// (`RockchipFlashProfile.pinnedToolchainFingerprint`).
pub fn fixed_tool_identity() -> ToolchainIdentity {
    ToolchainIdentity {
        id: OpaqueId::new("rkdeveloptool-fixed").expect("literal identifier"),
        kind: ToolchainKind::FixedTool,
        version: Version::new(1, 32, 0),
        backend_digest: Sha256Digest::parse_hex(
            "038a8a0ea26ef7eb77451789f310c0c9fbeaf43a78af1d6146e02311a9c23611",
        )
        .expect("pinned literal digest"),
        // ArkDeck records this alongside the hash. Two builds of this commit
        // exist on the reference host with different digests, which is why the
        // digest stays the discriminator and this is only provenance (AD-010).
        upstream_ref: Some("rkdeveloptool@304f073752fd25c854e1bcf05d8e7f925b1f4e14".into()),
    }
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
