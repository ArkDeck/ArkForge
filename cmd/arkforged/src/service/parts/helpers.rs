use super::*;

pub(super) fn profile_key(id: &OpaqueId, version: Version) -> String {
    format!("{id}@{version}")
}

/// Every blocker, in one message.
///
/// All of them rather than the first: an operator fixing one at a time should
/// not have to discover the second by trying again.
pub(super) fn blocker_list(blockers: &[arkforge_engine::ExecutionBlocker]) -> String {
    blockers
        .iter()
        .map(|blocker| format!("{}: {blocker}", blocker.code()))
        .collect::<Vec<_>>()
        .join("; ")
}

pub(super) fn admission_facts(
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
pub(super) fn successful_dispatch_requires_rebind(
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

pub(super) fn encode_job_summary(job: &crate::jobs::Job) -> JobSummary {
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

pub(super) fn next_expected_mode(
    stored: &StoredPlan,
    job: Option<&crate::jobs::Job>,
) -> Result<Option<DeviceMode>, String> {
    let job = job.ok_or_else(|| "the job no longer exists".to_string())?;
    Ok(job.expected_mode(&stored.envelope))
}

/// Chooses the provider by the artifact formats the *profile* declares.
///
/// The daemon does not decide that a device belongs to a vendor; the profile
/// says which formats apply, and a provider that handles one of them takes it.
/// A profile whose formats nobody handles gets a refusal, not a default.
pub(super) fn provider_for<'a>(
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

pub(super) fn driver_facts_digest() -> Sha256Digest {
    sha256(b"arkforge/driver-facts/none-measured")
}

pub(super) fn evidence_set_digest() -> Sha256Digest {
    sha256(b"AD-003,AD-005,AD-006")
}

/// The research backend the Unisoc provider dispatches through — which is to
/// say, no backend at all.
pub(super) fn research_toolchain_identity() -> ToolchainIdentity {
    ToolchainIdentity {
        id: OpaqueId::new("research-inspect").expect("literal identifier"),
        kind: ToolchainKind::Replay,
        version: Version::new(0, 1, 0),
        backend_digest: sha256(b"arkforge/research-inspect"),
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
pub(super) fn unbound_native_toolchain_identity() -> ToolchainIdentity {
    native_toolchain_identity(sha256(b"arkforge/native-rockusb/unbound"))
}

pub(super) fn replay_toolchain_identity() -> ToolchainIdentity {
    ToolchainIdentity {
        id: OpaqueId::new("transcript-replay").expect("literal identifier"),
        kind: ToolchainKind::Replay,
        version: Version::new(1, 0, 0),
        backend_digest: sha256(b"arkforge/transcript-replay"),
        upstream_ref: None,
    }
}

pub(super) struct ImportPayload {
    pub(super) expected_size_bytes: u64,
    pub(super) expected_sha256: Option<String>,
}

pub(super) fn decode_import_request(payload: &[u8]) -> Result<ImportPayload, String> {
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

pub(super) fn first_string_field(payload: &[u8], wanted: u32) -> Option<String> {
    let mut reader = arkforge_ipc::wire::Reader::new(payload);
    while let Ok(Some((field, value))) = reader.next_field() {
        if field == wanted {
            return value.as_str(field).ok().map(str::to_string);
        }
    }
    None
}

pub(super) fn first_u64_field(payload: &[u8], wanted: u32) -> Option<u64> {
    let mut reader = arkforge_ipc::wire::Reader::new(payload);
    while let Ok(Some((field, value))) = reader.next_field() {
        if field == wanted {
            return value.as_u64().ok();
        }
    }
    None
}

pub(super) fn first_bytes_field(payload: &[u8], wanted: u32) -> Option<Vec<u8>> {
    let mut reader = arkforge_ipc::wire::Reader::new(payload);
    while let Ok(Some((field, value))) = reader.next_field() {
        if field == wanted {
            return value.as_bytes().ok().map(ToOwned::to_owned);
        }
    }
    None
}

pub(super) fn digest_from_bytes(bytes: &[u8]) -> Option<Sha256Digest> {
    if bytes.len() != 32 {
        return None;
    }
    let mut digest = [0u8; 32];
    digest.copy_from_slice(bytes);
    Some(Sha256Digest::from_bytes(digest))
}

pub(super) fn encode_observation(observation: &DeviceObservation) -> Vec<u8> {
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
    arkforge_ipc::wire::write_string(
        &mut out,
        9,
        &observation
            .serial_evidence
            .digest()
            .map(|digest| digest.to_hex())
            .unwrap_or_default(),
    );
    arkforge_ipc::wire::write_string(
        &mut out,
        10,
        match observation.serial_evidence {
            arkforge_transport::SerialEvidence::Absent => "absent",
            arkforge_transport::SerialEvidence::Descriptor { .. } => "descriptor",
            arkforge_transport::SerialEvidence::ProtocolUnique { .. } => "protocolUnique",
        },
    );
    out
}

pub(super) fn encode_step(step: &arkforge_core::PublicFlashStep) -> PublicStep {
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

pub(super) fn encode_effect(effect: &PersistentEffect) -> Effect {
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

pub(super) fn recovered_managed_control_possible_effects(
    journal: &arkforge_engine::journal::Journal,
) -> Option<PossibleEffectSet> {
    let ledger = PermitLedger::from_journal(journal);
    let unresolved = ledger.unresolved();
    if unresolved.is_empty() {
        return None;
    }
    for permit_id in unresolved {
        let action = journal.records().iter().rev().find_map(|record| {
            if record.kind != JournalRecordKind::PermitConsuming
                || journal_fact(&record.facts, "permitId") != Some(permit_id.as_str())
            {
                return None;
            }
            journal_fact(&record.facts, "controlAction")
        })?;
        if !ManagedControlAction::ALL
            .iter()
            .any(|candidate| candidate.as_str() == action)
        {
            return None;
        }
    }
    Some(PossibleEffectSet {
        effects: arkforge_core::EffectSet::read_only(),
        completeness: EffectSetCompleteness::Bounded,
        source_action_ids: Vec::new(),
    })
}

pub(super) fn journal_fact<'a>(facts: &'a [(OpaqueId, String)], key: &str) -> Option<&'a str> {
    facts
        .iter()
        .find(|(candidate, _)| candidate.as_str() == key)
        .map(|(_, value)| value.as_str())
}

pub(super) fn encode_transient(effect: &TransientEffect) -> Effect {
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

pub(super) fn encode_data_impact(impact: &arkforge_core::DataImpact) -> Vec<KeyValue> {
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

pub(super) fn completeness_name(completeness: EffectSetCompleteness) -> &'static str {
    match completeness {
        EffectSetCompleteness::Bounded => "bounded",
        EffectSetCompleteness::Unbounded => "unbounded",
    }
}

pub(super) fn persistent_effect_name(effect: &PersistentEffect) -> String {
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

pub(super) fn recovery_blocker_code(blocker: &RecoveryBlocker) -> &'static str {
    match blocker {
        RecoveryBlocker::EffectsUnbounded => "effectsUnbounded",
        RecoveryBlocker::NoPublishedCoverage { .. } => "noPublishedCoverage",
        RecoveryBlocker::EffectOutsideCoverage { .. } => "effectOutsideCoverage",
        RecoveryBlocker::NothingToRecover => "nothingToRecover",
    }
}
