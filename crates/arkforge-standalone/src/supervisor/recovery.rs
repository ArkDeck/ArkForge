//! Runtime-owned, single-use complete-overwrite recovery admission.
//!
//! The old job is only read. None of these records is an old permit, a new
//! receipt for the old job, or permission to replay its plan. A crash before
//! the new job correlation leaves a spent reservation with zero eligible
//! permits. This deliberately bounded path admits one replacement epoch.

use super::*;
use arkforge_core::effect::DeviceMode;
use arkforge_core::step::{
    BindingRequirement, CancellationPolicy, FlashStepKind, PublicFlashStep, SemanticTarget,
    WorkflowEffect,
};
use arkforge_ipc::messages::{JobSummary, ManagedControlAction};
use arkforge_ipc::recovery::{RecoverySourceEvidence, SupersedingRecoveryAssessment};
use std::io::Read;

const MAX_RECORD_BYTES: u64 = 1024 * 1024;
const RECOVERY_BUDGET_MS: u64 = 4 * 60 * 60 * 1000;

fn blocked(reason: impl std::fmt::Display) -> StandaloneError {
    StandaloneError::new("RECOVERY_PROOF_REQUIRED", reason.to_string(), 3, false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Proof {
    assessment: SupersedingRecoveryAssessment,
    source_authority: Vec<u8>,
    source_lineage: Vec<u8>,
    source_permit: Vec<u8>,
    replacement_authority: Vec<u8>,
    issued_at: u64,
    expires_at: u64,
}

impl Proof {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_message(&mut out, 1, &self.assessment.encode());
        wire::write_bytes(&mut out, 2, &self.source_authority);
        wire::write_bytes(&mut out, 3, &self.source_lineage);
        wire::write_bytes(&mut out, 4, &self.source_permit);
        wire::write_bytes(&mut out, 5, &self.replacement_authority);
        wire::write_uint64(&mut out, 6, self.issued_at);
        wire::write_uint64(&mut out, 7, self.expires_at);
        out
    }

    fn decode(input: &[u8]) -> Result<Self, StandaloneError> {
        let mut out = Self {
            assessment: Default::default(),
            source_authority: Vec::new(),
            source_lineage: Vec::new(),
            source_permit: Vec::new(),
            replacement_authority: Vec::new(),
            issued_at: 0,
            expires_at: 0,
        };
        let mut reader = wire::Reader::new(input);
        while let Some((field, value)) = reader.next_field().map_err(blocked)? {
            match field {
                1 => {
                    out.assessment =
                        SupersedingRecoveryAssessment::decode(value.as_bytes().map_err(blocked)?)
                            .map_err(blocked)?
                }
                2 => out.source_authority = value.as_bytes().map_err(blocked)?.to_vec(),
                3 => out.source_lineage = value.as_bytes().map_err(blocked)?.to_vec(),
                4 => out.source_permit = value.as_bytes().map_err(blocked)?.to_vec(),
                5 => out.replacement_authority = value.as_bytes().map_err(blocked)?.to_vec(),
                6 => out.issued_at = value.as_u64().map_err(blocked)?,
                7 => out.expires_at = value.as_u64().map_err(blocked)?,
                _ => return Err(blocked("Unrecognized recovery proof field.")),
            }
        }
        if out.encode() != input
            || !out.assessment.eligible
            || out.assessment.source.is_none()
            || out.source_authority.len() != 32
            || out.source_lineage.len() != 32
            || out.replacement_authority.len() != 32
            || out.issued_at == 0
            || out.expires_at <= out.issued_at
            || out.expires_at - out.issued_at > RECOVERY_BUDGET_MS
        {
            return Err(blocked("Incomplete or noncanonical recovery proof."));
        }
        Ok(out)
    }
}

fn path(runtime: &Path, id: &str, extension: &str) -> PathBuf {
    runtime.join("authority").join("recovery").join(format!(
        "{}.{}",
        sha256(id.as_bytes()),
        extension
    ))
}

fn require_plain(path: &Path, directory: bool) -> Result<(), StandaloneError> {
    let metadata = std::fs::symlink_metadata(path).map_err(blocked)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(blocked("Recovery storage refuses reparse points."));
        }
    }
    if metadata.file_type().is_symlink()
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        return Err(blocked(
            "Recovery storage requires plain files and directories.",
        ));
    }
    Ok(())
}

fn read(path: &Path) -> Result<Vec<u8>, StandaloneError> {
    require_plain(path, false)?;
    let file = std::fs::File::open(path).map_err(blocked)?;
    if file.metadata().map_err(blocked)?.len() > MAX_RECORD_BYTES {
        return Err(blocked("Recovery record exceeds size bound."));
    }
    let mut bytes = Vec::new();
    file.take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(blocked)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(blocked("Recovery record grew beyond size bound."));
    }
    Ok(bytes)
}

/// Atomic publication without overwriting a reservation, correlation or proof.
fn persist(path: &Path, bytes: &[u8]) -> Result<(), StandaloneError> {
    let root = path
        .parent()
        .ok_or_else(|| blocked("Missing recovery storage root."))?;
    std::fs::create_dir_all(root).map_err(blocked)?;
    require_plain(root, true)?;
    protect_path(root, true).map_err(blocked)?;
    let mut nonce = [0; 32];
    fill_random(&mut nonce).map_err(blocked)?;
    let temporary = root.join(format!("{}.pending", sha256(&nonce)));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).map_err(blocked)?;
    let result = (|| {
        protect_path(&temporary, false).map_err(blocked)?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(blocked)?;
        std::fs::hard_link(&temporary, path).map_err(|_| {
            blocked("This recovery record already exists or could not be committed.")
        })?;
        sync_directory(root).map_err(blocked)
    })();
    drop(file);
    let _ = std::fs::remove_file(&temporary);
    result
}

fn lineage_digest(lineage: &ActiveTargetLineage) -> Vec<u8> {
    let mut out = Vec::new();
    wire::write_string(&mut out, 1, &lineage.plan_id);
    wire::write_string(&mut out, 2, &lineage.current_device_id);
    wire::write_bytes(&mut out, 3, &lineage.current_stable_identity_sha256);
    wire::write_string(&mut out, 4, &lineage.topology_sha256);
    wire::write_uint64(&mut out, 5, lineage.revision);
    sha256(&out).as_bytes().to_vec()
}

fn assessment(
    controller: &mut ControllerClient,
    job: &str,
) -> Result<SupersedingRecoveryAssessment, StandaloneError> {
    SupersedingRecoveryAssessment::decode(&controller.plan_superseding_recovery(job)?)
        .map_err(blocked)
}

fn source_records(
    runtime: &Path,
    job: &str,
    source: &RecoverySourceEvidence,
) -> Result<(AuthorityPlanRecord, ActiveTargetLineage, Vec<u8>), StandaloneError> {
    let old = load_authority_plan(runtime, &source.plan_id)?;
    let lineage = load_job_lineage(runtime, job)?;
    if old.plan.plan_sha256 != source.plan_sha256
        || lineage.plan_id != source.plan_id
        || lineage.topology_sha256 != old.topology_sha256
        || source.journal_sequence == 0
        || Sha256Digest::parse_hex(&source.journal_head_sha256).is_err()
    {
        return Err(blocked(
            "The original authority, plan and verified journal do not correlate.",
        ));
    }
    let permit_hash = if source.closed_control.is_some() {
        if lineage.revision != 1
            || lineage.current_device_id != old.device_id
            || lineage.current_stable_identity_sha256 != old.stable_identity_sha256
        {
            return Err(blocked(
                "The first-control proof conflicts with the original target lineage.",
            ));
        }
        source_control_permit(runtime, job, source, &old)?
    } else {
        Vec::new()
    };
    Ok((old, lineage, permit_hash))
}

fn source_control_permit(
    runtime: &Path,
    job: &str,
    source: &RecoverySourceEvidence,
    old: &AuthorityPlanRecord,
) -> Result<Vec<u8>, StandaloneError> {
    let control = source
        .closed_control
        .as_ref()
        .ok_or_else(|| blocked("Missing closed control evidence."))?;
    if control.action != ManagedControlAction::EnterUpdater {
        return Err(blocked("Unsupported uncertain control action."));
    }
    let request = control
        .request_id
        .strip_suffix("-control")
        .ok_or_else(|| blocked("The old control request has no exact admission correlation."))?;
    let epoch = control
        .controller_session_id
        .strip_prefix("CLI-SESSION-")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| blocked("The old control has no standalone authority session."))?;
    let bytes = read(
        &runtime
            .join("authority")
            .join("permits")
            .join(epoch.to_string())
            .join(format!("{}.permit", sha256(request.as_bytes()))),
    )?;
    let persisted = decode_persisted_permit(&StepAdmissionSnapshot::default(), epoch, &bytes)?;
    let permit = StepPermit::from_canonical_bytes(&persisted.permit_cbor).map_err(blocked)?;
    validate_control_permit(job, source, old, &permit)?;
    // The journal proves prior acceptance. We only correlate its exact body;
    // never resurrect an old pairing secret or submit this spent permit.
    Ok(sha256(&bytes).as_bytes().to_vec())
}

fn validate_control_permit(
    job: &str,
    source: &RecoverySourceEvidence,
    old: &AuthorityPlanRecord,
    permit: &StepPermit,
) -> Result<(), StandaloneError> {
    let control = source
        .closed_control
        .as_ref()
        .ok_or_else(|| blocked("Missing control proof."))?;
    let first = old
        .plan
        .public_steps
        .first()
        .ok_or_else(|| blocked("Missing original public entry step."))?;
    if first.step_id != control.step_id
        || first.kind != "ensureMode"
        || first.effect != "mutating"
        || first.semantic_target != "device"
        || !first.content_sha256.is_empty()
        || first.expected_mode_before.is_empty()
        || first.expected_mode_after.is_empty()
        || first.expected_mode_before == first.expected_mode_after
    {
        return Err(blocked(
            "The old public step is not the exact closed mode transition.",
        ));
    }
    let public = PublicFlashStep {
        step_id: StepId::new(&first.step_id).map_err(blocked)?,
        kind: FlashStepKind::EnsureMode,
        effect: WorkflowEffect::Mutating,
        cancellation: CancellationPolicy::parse(&first.cancellation)
            .ok_or_else(|| blocked("Invalid old cancellation policy."))?,
        binding: BindingRequirement::parse(&first.binding)
            .ok_or_else(|| blocked("Invalid old binding requirement."))?,
        semantic_target: Some(SemanticTarget::Device),
        content_digest: None,
        expected_mode_before: Some(DeviceMode::new(&first.expected_mode_before).map_err(blocked)?),
        expected_mode_after: Some(DeviceMode::new(&first.expected_mode_after).map_err(blocked)?),
        private_action_digest: Sha256Digest::parse_hex(&first.private_action_sha256)
            .map_err(blocked)?,
    };
    if permit.job_id.as_str() != job
        || permit.plan_id.as_str() != source.plan_id
        || permit.plan_digest.to_hex() != source.plan_sha256
        || permit.permit_id.as_str() != control.permit_id
        || permit.step_id.as_str() != control.step_id
        || permit.attempt_id.as_str() != control.attempt_id
        || permit.controller_session_id.as_str() != control.controller_session_id
        || permit.authority_namespace.as_str() != "arkforge.cli"
        || permit.authority_binding.authority_namespace.as_str() != "arkforge.cli"
        || permit.authority_binding.binding_id.as_str() != old.binding_id
        || permit.authority_binding.binding_revision != 1
        || permit.authority_binding.stable_identity_digest.as_bytes()
            != old.stable_identity_sha256.as_slice()
        || permit.public_step_digest != public.digest().map_err(blocked)?
        || permit.private_action_digest != public.private_action_digest
        || !permit.single_use
        || permit.issued_at_epoch_ms > control.dispatched_at_epoch_ms
        || permit.expires_at_epoch_ms <= control.dispatched_at_epoch_ms
        || control.deadline_epoch_ms <= control.dispatched_at_epoch_ms
    {
        return Err(blocked(
            "The spent permit does not bind the exact journaled control and original identity.",
        ));
    }
    Ok(())
}

fn fresh_identity(
    public: &mut PublicClient,
    old: &AuthorityPlanRecord,
    lineage: &ActiveTargetLineage,
    profile: &str,
    device: &str,
) -> Result<(), StandaloneError> {
    if profile != old.profile_id || device != lineage.current_device_id {
        return Err(blocked(
            "Recovery must use the original profile and exact current target lineage.",
        ));
    }
    let observations = public.device_list()?;
    let mut matches = observations
        .iter()
        .filter(|value| value.observation_id == device);
    let observation = matches
        .next()
        .ok_or_else(|| blocked("The recovery target is absent."))?;
    if matches.next().is_some() {
        return Err(blocked("The recovery target is ambiguous."));
    }
    let probe = public.device_probe(device, profile)?;
    validate_observations(lineage, observation, &probe)
}

fn validate_observations(
    lineage: &ActiveTargetLineage,
    observation: &DeviceObservationView,
    probe: &DeviceProbeView,
) -> Result<(), StandaloneError> {
    for value in [observation, &probe.observation] {
        if value.identity_strength != "serialAndTopology"
            || value.serial_sha256.is_empty()
            || value.descriptor_sha256.is_empty()
            || value.topology_sha256 != lineage.topology_sha256
            || value.malformed_descriptor
        {
            return Err(blocked(
                "Recovery requires fresh, strong serial, descriptor and topology identity.",
            ));
        }
    }
    if stable_identity_digest(observation, probe).as_bytes()
        != lineage.current_stable_identity_sha256.as_slice()
    {
        return Err(blocked(
            "Fresh device facts differ from the original durable target lineage.",
        ));
    }
    Ok(())
}

pub(super) fn require_source(
    runtime: &Path,
    public: &mut PublicClient,
    controller: &mut ControllerClient,
    job: &str,
    profile: &str,
    device: &str,
) -> Result<(), StandaloneError> {
    let assessment = assessment(controller, job)?;
    if !assessment.eligible && assessment.blocker_code != "managedControlRecoveryNeedsPlan" {
        return Err(blocked(assessment.blocker_reason));
    }
    let source = assessment
        .source
        .as_ref()
        .ok_or_else(|| blocked("No verified source journal."))?;
    let (old, lineage, _) = source_records(runtime, job, source)?;
    require_lane(runtime, public, controller, job, "")?;
    if path(runtime, job, "reserved")
        .try_exists()
        .map_err(blocked)?
    {
        return Err(blocked("The source recovery epoch is already reserved."));
    }
    fresh_identity(public, &old, &lineage, profile, device)
}

pub(super) fn authorize(
    runtime: &Path,
    public: &mut PublicClient,
    controller: &mut ControllerClient,
    plan: &ExecutablePlan,
) -> Result<(), StandaloneError> {
    let new = load_authority_plan(runtime, &plan.plan_id)?;
    let assessment = controller.assess_recovery_candidate(
        &new.supersedes_job_id,
        &plan.plan_id,
        &plan.plan_sha256,
    )?;
    if !assessment.eligible {
        return Err(blocked(assessment.blocker_reason));
    }
    let source = assessment
        .source
        .as_ref()
        .ok_or_else(|| blocked("Missing source evidence."))?;
    let (old, lineage, permit_hash) = source_records(runtime, &new.supersedes_job_id, source)?;
    validate_replacement(&old, &lineage, &new, &assessment)?;
    fresh_identity(public, &old, &lineage, &new.profile_id, &new.device_id)?;
    let now = arkforged::rescue::now_epoch_ms()?;
    let proof = Proof {
        assessment,
        source_authority: sha256(&encode_authority_plan(&old)).as_bytes().to_vec(),
        source_lineage: lineage_digest(&lineage),
        source_permit: permit_hash,
        replacement_authority: sha256(&encode_authority_plan(&new)).as_bytes().to_vec(),
        issued_at: now,
        expires_at: now
            .saturating_add(RECOVERY_BUDGET_MS)
            .min(plan.expires_at_epoch_ms),
    };
    Proof::decode(&proof.encode())?;
    persist(&path(runtime, &plan.plan_id, "proof"), &proof.encode())
}

fn validate_replacement(
    old: &AuthorityPlanRecord,
    lineage: &ActiveTargetLineage,
    new: &AuthorityPlanRecord,
    assessment: &SupersedingRecoveryAssessment,
) -> Result<(), StandaloneError> {
    if new.plan.execution_purpose != "supersedingRecovery"
        || new.supersedes_job_id != assessment.job_id
        || new.plan.plan_id != assessment.candidate_plan_id
        || new.plan.plan_sha256 != assessment.candidate_plan_sha256
        || new.plan.plan_id == old.plan.plan_id
        || new.plan.plan_sha256 == old.plan.plan_sha256
        || new.profile_id != old.profile_id
        || new.topology_sha256 != old.topology_sha256
        || new.device_id != lineage.current_device_id
        || new.stable_identity_sha256 != lineage.current_stable_identity_sha256
        || new.plan.persistent_effects.is_empty()
        || new.plan.persistent_effects != old.plan.persistent_effects
        || Sha256Digest::parse_hex(&assessment.candidate_profile_sha256).is_err()
        || Sha256Digest::parse_hex(&assessment.candidate_artifact_sha256).is_err()
        || Sha256Digest::parse_hex(&assessment.recovery_contract_sha256).is_err()
    {
        return Err(blocked(
            "Recovery does not reproduce the exact target, immutable content and complete original write set.",
        ));
    }
    Ok(())
}

fn validate_proof(
    runtime: &Path,
    controller: &mut ControllerClient,
    record: &AuthorityPlanRecord,
    now: u64,
) -> Result<Proof, StandaloneError> {
    let proof = Proof::decode(&read(&path(runtime, &record.plan.plan_id, "proof"))?)?;
    if now < proof.issued_at || now >= proof.expires_at {
        return Err(blocked("Recovery budget or plan has expired."));
    }
    let assessment = controller.assess_recovery_candidate(
        &record.supersedes_job_id,
        &record.plan.plan_id,
        &record.plan.plan_sha256,
    )?;
    if assessment != proof.assessment || !assessment.eligible {
        return Err(blocked(
            "Recovery source, coverage or immutable candidate facts changed.",
        ));
    }
    validate_source_seals(runtime, record, &proof)?;
    Ok(proof)
}

fn validate_source_seals(
    runtime: &Path,
    record: &AuthorityPlanRecord,
    proof: &Proof,
) -> Result<(), StandaloneError> {
    let source = proof
        .assessment
        .source
        .as_ref()
        .ok_or_else(|| blocked("Missing source proof."))?;
    let (old, lineage, permit_hash) = source_records(runtime, &record.supersedes_job_id, source)?;
    validate_replacement(&old, &lineage, record, &proof.assessment)?;
    if proof.source_authority != sha256(&encode_authority_plan(&old)).as_bytes()
        || proof.source_lineage != lineage_digest(&lineage)
        || proof.source_permit != permit_hash
        || proof.replacement_authority != sha256(&encode_authority_plan(record)).as_bytes()
    {
        return Err(blocked("Durable recovery authority facts changed."));
    }
    Ok(())
}

pub(super) fn before_start(
    runtime: &Path,
    public: &mut PublicClient,
    controller: &mut ControllerClient,
    record: &AuthorityPlanRecord,
    now: u64,
) -> Result<(), StandaloneError> {
    for job in public.job_list()? {
        if matches!(job.state.as_str(), "outcomeUnknown" | "recoveryAssessable")
            && job.plan_id == record.plan.plan_id
        {
            return Err(blocked(
                "An unknown job's original plan can never be started again, even after independent recovery.",
            ));
        }
    }
    require_lane(runtime, public, controller, &record.supersedes_job_id, "")?;
    if record.plan.execution_purpose == "primaryFlash" && record.supersedes_job_id.is_empty() {
        return Ok(());
    }
    let proof = validate_proof(runtime, controller, record, now)?;
    let (old, lineage, _) = source_records(
        runtime,
        &record.supersedes_job_id,
        proof
            .assessment
            .source
            .as_ref()
            .ok_or_else(|| blocked("Missing source proof."))?,
    )?;
    fresh_identity(
        public,
        &old,
        &lineage,
        &record.profile_id,
        &record.device_id,
    )?;
    // One replacement per source, including after crashes or failed starts.
    persist(
        &path(runtime, &record.supersedes_job_id, "reserved"),
        &proof.encode(),
    )
}

pub(super) fn correlate(
    runtime: &Path,
    record: &AuthorityPlanRecord,
    job: &str,
) -> Result<(), StandaloneError> {
    if record.supersedes_job_id.is_empty() {
        return Ok(());
    }
    let proof = read(&path(runtime, &record.plan.plan_id, "proof"))?;
    if read(&path(runtime, &record.supersedes_job_id, "reserved"))? != proof {
        return Err(blocked(
            "Recovery reservation differs from its sealed proof.",
        ));
    }
    persist(
        &path(runtime, &record.supersedes_job_id, "job"),
        job.as_bytes(),
    )
}

pub(super) fn before_effect(
    runtime: &Path,
    public: &mut PublicClient,
    controller: &mut ControllerClient,
    record: &AuthorityPlanRecord,
    job_id: &str,
) -> Result<(), StandaloneError> {
    require_lane(
        runtime,
        public,
        controller,
        &record.supersedes_job_id,
        job_id,
    )?;
    if record.plan.execution_purpose == "primaryFlash" && record.supersedes_job_id.is_empty() {
        return Ok(());
    }
    let proof = validate_proof(
        runtime,
        controller,
        record,
        arkforged::rescue::now_epoch_ms()?,
    )?;
    if read(&path(runtime, &record.supersedes_job_id, "reserved"))? != proof.encode()
        || read(&path(runtime, &record.supersedes_job_id, "job"))? != job_id.as_bytes()
    {
        return Err(blocked(
            "No single-use recovery reservation is correlated to this exact job.",
        ));
    }
    Ok(())
}

fn require_lane(
    runtime: &Path,
    public: &mut PublicClient,
    controller: &mut ControllerClient,
    source: &str,
    active: &str,
) -> Result<(), StandaloneError> {
    for job in public.job_list()? {
        if !job.terminal && job.job_id != active {
            return Err(blocked("Another job holds the serial execution lane."));
        }
        if matches!(job.state.as_str(), "outcomeUnknown" | "recoveryAssessable")
            && job.job_id != source
            && !completed_relation(runtime, public, controller, &job.job_id)?
        {
            return Err(blocked(
                "An outstanding unknown intent is not covered by this recovery proof.",
            ));
        }
        if job.state == "confirmedFailed"
            && public.job_events(&job.job_id, 0)?.iter().any(|event| {
                event
                    .receipt
                    .as_ref()
                    .is_some_and(|receipt| receipt.disposition == "confirmedPartialEffect")
            })
        {
            return Err(blocked(
                "A separate confirmed partial effect is not covered by this recovery proof.",
            ));
        }
    }
    Ok(())
}

fn completion_bytes(
    runtime: &Path,
    public: &mut PublicClient,
    controller: &mut ControllerClient,
    source_job: &str,
) -> Result<Vec<u8>, StandaloneError> {
    let proof_bytes = read(&path(runtime, source_job, "reserved"))?;
    let proof = Proof::decode(&proof_bytes)?;
    if proof.assessment.job_id != source_job {
        return Err(blocked("Recovery source correlation differs."));
    }
    let job_bytes = read(&path(runtime, source_job, "job"))?;
    let job_id = std::str::from_utf8(&job_bytes).map_err(blocked)?;
    JobId::new(job_id).map_err(blocked)?;
    let record = load_authority_plan(runtime, &proof.assessment.candidate_plan_id)?;
    validate_source_seals(runtime, &record, &proof)?;
    if assessment(controller, source_job)?.source != proof.assessment.source {
        return Err(blocked("The original unknown journal changed."));
    }
    let job = public.job_show(job_id)?;
    if job.job_id != job_id {
        return Err(blocked(
            "The recovery job response does not match its durable correlation.",
        ));
    }
    let events = public.job_events(job_id, 0)?;
    let receipts = events
        .iter()
        .filter_map(|event| event.receipt.as_ref())
        .collect::<Vec<_>>();
    completion_record(&record, &job, &proof_bytes, &receipts)
}

fn completion_record(
    record: &AuthorityPlanRecord,
    job: &JobSummary,
    proof_bytes: &[u8],
    receipts: &[&arkforge_ipc::messages::ActionReceiptSummary],
) -> Result<Vec<u8>, StandaloneError> {
    validate_terminal(record, job)?;
    let mut out = Vec::new();
    wire::write_string(&mut out, 1, "SupersedingRecoveryEpoch");
    wire::write_bytes(&mut out, 2, sha256(proof_bytes).as_bytes());
    wire::write_string(&mut out, 3, &record.supersedes_job_id);
    wire::write_string(&mut out, 4, &job.job_id);
    wire::write_message(&mut out, 5, &job.encode());
    for step in &record.plan.public_steps {
        if !matches!(
            step.kind.as_str(),
            "ensureMode" | "writeTarget" | "verifyTarget" | "reboot" | "postflightProbe"
        ) {
            continue;
        }
        let matching = receipts
            .iter()
            .filter(|receipt| receipt.step_id == step.step_id)
            .collect::<Vec<_>>();
        let [receipt] = matching.as_slice() else {
            return Err(blocked(
                "Recovery is missing a unique semantic receipt for a required step.",
            ));
        };
        if receipt.job_id != job.job_id
            || receipt.plan_id != record.plan.plan_id
            || receipt.disposition != "semanticSuccess"
            || !receipt.failure_classification.is_empty()
            || (step.kind == "verifyTarget"
                && !(receipt.verification_outcome == "verified"
                    || (receipt.verification_outcome == "typedSkip"
                        && !receipt.typed_skip_reason.is_empty())))
            || (step.kind == "postflightProbe" && receipt.facts.is_empty())
        {
            return Err(blocked(
                "Recovery verification, reset or postflight did not semantically succeed.",
            ));
        }
        receipt.receipt_digest().map_err(blocked)?;
        wire::write_message(&mut out, 6, &receipt.encode());
    }
    Ok(out)
}

fn validate_terminal(
    record: &AuthorityPlanRecord,
    job: &JobSummary,
) -> Result<(), StandaloneError> {
    if !job.terminal
        || job.state != "succeeded"
        || job.plan_id != record.plan.plan_id
        || digest_bytes(&job.plan_sha256, "recovery plan")?.to_hex() != record.plan.plan_sha256
        || job.total_steps != record.plan.public_steps.len() as u64
        || job.completed_steps != job.total_steps
    {
        return Err(blocked(
            "The independent recovery has not completed every sealed step.",
        ));
    }
    Ok(())
}

fn completed_relation(
    runtime: &Path,
    public: &mut PublicClient,
    controller: &mut ControllerClient,
    source: &str,
) -> Result<bool, StandaloneError> {
    let target = path(runtime, source, "complete");
    if !target.try_exists().map_err(blocked)? {
        return Ok(false);
    }
    Ok(read(&target)? == completion_bytes(runtime, public, controller, source)?)
}

pub(super) fn finalize(runtime: &Path, public: &mut PublicClient) -> Result<(), StandaloneError> {
    let mut controller = ControllerClient::connect(runtime)?;
    for job in public.job_list()? {
        if job.state != "outcomeUnknown" {
            continue;
        }
        let target = path(runtime, &job.job_id, "complete");
        if target.try_exists().map_err(blocked)?
            || !path(runtime, &job.job_id, "job")
                .try_exists()
                .map_err(blocked)?
        {
            continue;
        }
        // A failed/unfinished recovery never releases the lane. Its blocker is
        // reported by admission; finalization does not hide unrelated jobs.
        if let Ok(bytes) = completion_bytes(runtime, public, &mut controller, &job.job_id) {
            persist(&target, &bytes)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;
