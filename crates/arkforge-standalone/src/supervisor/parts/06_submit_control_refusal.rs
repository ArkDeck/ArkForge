use super::*;

pub(super) fn submit_control_refusal(
    controller: &mut ControllerClient,
    request: &arkforge_ipc::messages::ManagedControlRequest,
    reason: &str,
) -> Result<(), StandaloneError> {
    let receipt = SubmitManagedControlReceiptRequest {
        job_id: request.job_id.clone(),
        request_id: request.request_id.clone(),
        action: request.action,
        accepted: false,
        facts: Vec::new(),
        evidence_sha256: sha256(reason.as_bytes()).as_bytes().to_vec(),
        failure_reason: reason.to_string(),
    };
    submit_control_receipt(controller, &receipt)
}

pub(super) fn answer_admission(
    runtime_dir: &Path,
    epoch: u64,
    pairing: &ControllerPairingSecret,
    public: &mut PublicClient,
    controller: &mut ControllerClient,
    snapshot: &StepAdmissionSnapshot,
    target_lineages: &mut BTreeMap<String, ActiveTargetLineage>,
) -> Result<(), StandaloneError> {
    let record = load_authority_plan(runtime_dir, &snapshot.plan_id)?;
    if let Err(error) =
        recovery::before_effect(runtime_dir, public, controller, &record, &snapshot.job_id)
    {
        return submit_permit_refusal(controller, snapshot, epoch, &error.message);
    }
    let lineage = match target_lineages.get(&snapshot.job_id) {
        Some(lineage) => lineage.clone(),
        None => {
            let loaded = load_job_lineage(runtime_dir, &snapshot.job_id)?;
            target_lineages.insert(snapshot.job_id.clone(), loaded.clone());
            loaded
        }
    };
    if lineage.plan_id != snapshot.plan_id || lineage.topology_sha256 != record.topology_sha256 {
        return submit_permit_refusal(
            controller,
            snapshot,
            epoch,
            "The durable target lineage does not belong to this sealed plan.",
        );
    }
    let admitted_device = recompute_admitted_device_digest(snapshot)?;
    if admitted_device.as_bytes() != snapshot.admitted_device_facts_sha256.as_slice() {
        return submit_permit_refusal(
            controller,
            snapshot,
            epoch,
            "The admission device digest does not match the raw typed identity snapshot.",
        );
    }
    let now = arkforged::rescue::now_epoch_ms()?;
    let expires = snapshot
        .observed_at_epoch_ms
        .saturating_add(snapshot.snapshot_lifetime_ms);
    if now >= expires {
        return submit_permit_refusal(
            controller,
            snapshot,
            epoch,
            "The admission snapshot expired before the authority could permit it.",
        );
    }
    let Some(lineage) =
        resolve_lineage_for_admission(runtime_dir, public, snapshot, &record, &lineage)?
    else {
        return submit_permit_refusal(
            controller,
            snapshot,
            epoch,
            "The exact target lineage does not match one unique current observation.",
        );
    };
    target_lineages.insert(snapshot.job_id.clone(), lineage.clone());
    let current_stable =
        current_stable_identity(public, &lineage.current_device_id, &record.profile_id)?;
    if current_stable.as_bytes() != lineage.current_stable_identity_sha256.as_slice() {
        return submit_permit_refusal(
            controller,
            snapshot,
            epoch,
            "The exact target binding no longer matches the current observation and probe facts.",
        );
    }
    let submission = load_or_create_permit(
        runtime_dir,
        epoch,
        pairing,
        snapshot,
        &record,
        admitted_device,
        now,
        expires,
    )?;
    let outcome = controller.submit_permit(&submission)?;
    if !outcome.accepted {
        return Err(StandaloneError::new(
            outcome.rejection_code,
            outcome.rejection_message,
            3,
            false,
        ));
    }
    Ok(())
}

pub(super) fn resolve_lineage_for_admission(
    runtime_dir: &Path,
    public: &mut PublicClient,
    snapshot: &StepAdmissionSnapshot,
    record: &AuthorityPlanRecord,
    lineage: &ActiveTargetLineage,
) -> Result<Option<ActiveTargetLineage>, StandaloneError> {
    let observations = public.device_list()?;
    if let Some(current) = observations
        .iter()
        .find(|observation| observation.observation_id == lineage.current_device_id)
    {
        return Ok(observation_matches_admission(current, snapshot).then(|| lineage.clone()));
    }

    // A transport-dispatched reset does not return a managed-control rebound
    // observation to the supervisor. Advance the authority's durable lineage
    // only across the exact mode edge sealed immediately before this step and
    // only to the unique live observation that reproduces every typed identity
    // field in the daemon's admission snapshot.
    if !prior_reboot_authorizes_rebind(&record.plan, snapshot) {
        return Ok(None);
    }
    let mut matches = observations
        .iter()
        .filter(|observation| observation_matches_admission(observation, snapshot));
    let Some(rebound) = matches.next() else {
        return Ok(None);
    };
    if matches.next().is_some() {
        return Ok(None);
    }
    let probe = public.device_probe(&rebound.observation_id, &record.profile_id)?;
    if !observation_matches_admission(&probe.observation, snapshot) {
        return Ok(None);
    }
    let advanced = ActiveTargetLineage {
        plan_id: lineage.plan_id.clone(),
        current_device_id: rebound.observation_id.clone(),
        current_stable_identity_sha256: stable_identity_digest(rebound, &probe).as_bytes().to_vec(),
        topology_sha256: lineage.topology_sha256.clone(),
        revision: lineage.revision.saturating_add(1),
    };
    persist_job_lineage(runtime_dir, &snapshot.job_id, &advanced)?;
    Ok(Some(advanced))
}

pub(super) fn prior_reboot_authorizes_rebind(
    plan: &ExecutablePlan,
    snapshot: &StepAdmissionSnapshot,
) -> bool {
    let Some(index) = plan
        .public_steps
        .iter()
        .position(|step| step.step_id == snapshot.step_id)
    else {
        return false;
    };
    let Some(previous) = index
        .checked_sub(1)
        .and_then(|previous| plan.public_steps.get(previous))
    else {
        return false;
    };
    let current = &plan.public_steps[index];
    previous.kind == "reboot"
        && !previous.expected_mode_before.is_empty()
        && previous.expected_mode_before != previous.expected_mode_after
        && previous.expected_mode_after == snapshot.observed_mode
        && current.expected_mode_before == snapshot.observed_mode
        && current.expected_mode_after == snapshot.observed_mode
        && digest_bytes(&snapshot.private_action_sha256, "private action digest")
            .is_ok_and(|digest| digest.to_hex() == current.private_action_sha256)
}

pub(super) fn observation_matches_admission(
    observation: &DeviceObservationView,
    snapshot: &StepAdmissionSnapshot,
) -> bool {
    observation.mode == snapshot.observed_mode
        && digest_bytes(&snapshot.topology_sha256, "topology digest")
            .is_ok_and(|digest| digest.to_hex() == observation.topology_sha256)
        && digest_bytes(&snapshot.descriptor_sha256, "descriptor digest")
            .is_ok_and(|digest| digest.to_hex() == observation.descriptor_sha256)
        && digest_bytes(&snapshot.serial_sha256, "serial digest")
            .is_ok_and(|digest| digest.to_hex() == observation.serial_sha256)
        && observation.serial_evidence_kind == snapshot.serial_evidence_kind
        && observation.protocol_identity == snapshot.protocol_identity
        && observation.identity_strength == snapshot.identity_strength
        && observation.malformed_descriptor == snapshot.malformed_descriptor
}

pub(super) fn submit_permit_refusal(
    controller: &mut ControllerClient,
    snapshot: &StepAdmissionSnapshot,
    epoch: u64,
    reason: &str,
) -> Result<(), StandaloneError> {
    let outcome = controller.submit_permit(&SubmitStepPermitRequest {
        job_id: snapshot.job_id.clone(),
        request_id: snapshot.request_id.clone(),
        permit_cbor: Vec::new(),
        integrity_tag: Vec::new(),
        pairing_epoch: epoch,
        refusal: reason.to_string(),
    })?;
    if !outcome.accepted {
        return Err(StandaloneError::new(
            outcome.rejection_code,
            outcome.rejection_message,
            3,
            false,
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn load_or_create_permit(
    runtime_dir: &Path,
    epoch: u64,
    pairing: &ControllerPairingSecret,
    snapshot: &StepAdmissionSnapshot,
    record: &AuthorityPlanRecord,
    admitted_device: Sha256Digest,
    issued_at: u64,
    expires_at: u64,
) -> Result<SubmitStepPermitRequest, StandaloneError> {
    let root = runtime_dir
        .join("authority")
        .join("permits")
        .join(epoch.to_string());
    std::fs::create_dir_all(&root).map_err(|error| internal("create the permit journal", error))?;
    protect_path(&root, true).map_err(|error| internal("protect the permit journal", error))?;
    let path = root.join(format!("{}.permit", sha256(snapshot.request_id.as_bytes())));
    if path.exists() {
        let persisted =
            std::fs::read(&path).map_err(|error| internal("read a durable permit", error))?;
        return decode_persisted_permit(snapshot, epoch, &persisted);
    }

    let plan_digest = digest_bytes(&snapshot.plan_sha256, "plan digest")?;
    if plan_digest.to_hex() != record.plan.plan_sha256 {
        return Err(permit_invalid(
            "admission plan digest differs from authority plan",
        ));
    }
    let permit_id = PermitId::new(format!(
        "CLI-PERMIT-{}",
        &sha256(format!("{epoch}:{}", snapshot.request_id).as_bytes()).to_hex()[..24]
    ))
    .map_err(permit_invalid)?;
    let binding = AuthorityBindingRef {
        authority_namespace: AuthorityNamespace::new("arkforge.cli").map_err(permit_invalid)?,
        binding_id: OpaqueId::new(&record.binding_id).map_err(permit_invalid)?,
        binding_revision: 1,
        stable_identity_digest: digest_bytes(
            &record.stable_identity_sha256,
            "stable identity digest",
        )?,
    };
    let permit = StepPermit {
        permit_id,
        authority_namespace: AuthorityNamespace::new("arkforge.cli").map_err(permit_invalid)?,
        controller_session_id: ControllerSessionId::new(format!("CLI-SESSION-{epoch}"))
            .map_err(permit_invalid)?,
        job_id: JobId::new(&snapshot.job_id).map_err(permit_invalid)?,
        plan_id: PlanId::new(&snapshot.plan_id).map_err(permit_invalid)?,
        plan_digest,
        step_id: StepId::new(&snapshot.step_id).map_err(permit_invalid)?,
        attempt_id: AttemptId::new(&snapshot.attempt_id).map_err(permit_invalid)?,
        public_step_digest: digest_bytes(&snapshot.public_step_sha256, "public step digest")?,
        private_action_digest: digest_bytes(
            &snapshot.private_action_sha256,
            "private action digest",
        )?,
        effect_set_digest: digest_bytes(&snapshot.effect_set_sha256, "effect set digest")?,
        authority_binding: binding,
        admitted_device_facts_digest: admitted_device,
        issued_at_epoch_ms: issued_at,
        expires_at_epoch_ms: expires_at,
        single_use: true,
        integrity_tag: PermitIntegrityTag {
            epoch: PairingEpoch(epoch),
            tag: Sha256Digest::from_bytes([0; 32]),
        },
    };
    let permit_cbor = permit.signing_body().map_err(permit_invalid)?;
    let tag = mint_integrity_tag(&permit, pairing).map_err(permit_invalid)?;
    let submission = SubmitStepPermitRequest {
        job_id: snapshot.job_id.clone(),
        request_id: snapshot.request_id.clone(),
        permit_cbor,
        integrity_tag: tag.tag.as_bytes().to_vec(),
        pairing_epoch: epoch,
        refusal: String::new(),
    };
    let persisted = encode_persisted_permit(&submission);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| internal("create a durable permit", error))?;
    protect_path(&path, false).map_err(|error| internal("protect a durable permit", error))?;
    file.write_all(&persisted)
        .and_then(|_| file.sync_all())
        .map_err(|error| internal("commit a durable permit before submission", error))?;
    Ok(submission)
}

pub(super) fn encode_persisted_permit(submission: &SubmitStepPermitRequest) -> Vec<u8> {
    let mut out = Vec::new();
    wire::write_bytes(&mut out, 1, &submission.permit_cbor);
    wire::write_bytes(&mut out, 2, &submission.integrity_tag);
    wire::write_uint64(&mut out, 3, submission.pairing_epoch);
    out
}

pub(super) fn decode_persisted_permit(
    snapshot: &StepAdmissionSnapshot,
    epoch: u64,
    input: &[u8],
) -> Result<SubmitStepPermitRequest, StandaloneError> {
    let mut permit_cbor = Vec::new();
    let mut integrity_tag = Vec::new();
    let mut stored_epoch = 0;
    let mut reader = wire::Reader::new(input);
    while let Some((field, value)) = reader
        .next_field()
        .map_err(|error| internal("decode a durable permit", error))?
    {
        match field {
            1 => permit_cbor = value.as_bytes().map_err(status_decode)?.to_vec(),
            2 => integrity_tag = value.as_bytes().map_err(status_decode)?.to_vec(),
            3 => stored_epoch = value.as_u64().map_err(status_decode)?,
            _ => {}
        }
    }
    if permit_cbor.is_empty() || integrity_tag.len() != 32 || stored_epoch != epoch {
        return Err(StandaloneError::new(
            "PERMIT_JOURNAL_INVALID",
            "A durable permit is incomplete or belongs to another pairing epoch.",
            10,
            false,
        ));
    }
    Ok(SubmitStepPermitRequest {
        job_id: snapshot.job_id.clone(),
        request_id: snapshot.request_id.clone(),
        permit_cbor,
        integrity_tag,
        pairing_epoch: epoch,
        refusal: String::new(),
    })
}

pub(super) fn current_stable_identity(
    public: &mut PublicClient,
    device: &str,
    profile: &str,
) -> Result<Sha256Digest, StandaloneError> {
    let observations = public.device_list()?;
    let observation = observations
        .iter()
        .find(|candidate| candidate.observation_id == device)
        .ok_or_else(|| {
            StandaloneError::new(
                "OBSERVATION_NOT_FOUND",
                format!("No current observation exactly matches {device}."),
                5,
                true,
            )
        })?;
    let probe = public.device_probe(device, profile)?;
    Ok(stable_identity_digest(observation, &probe))
}

pub(super) fn stable_identity_digest(
    observation: &DeviceObservationView,
    probe: &DeviceProbeView,
) -> Sha256Digest {
    let mut stable = Vec::new();
    stable.extend_from_slice(b"arkforge.cli-stable-identity/v2");
    append_stable_observation(&mut stable, "discovery", observation);
    append_stable_observation(&mut stable, "same-handle-probe", &probe.observation);
    append_stable_field(&mut stable, "probe.profile", probe.profile_id.as_bytes());
    for fact in &probe.protocol_facts {
        append_stable_field(&mut stable, "probe.fact.key", fact.key.as_bytes());
        append_stable_field(&mut stable, "probe.fact.value", fact.value.as_bytes());
    }
    sha256(&stable)
}
