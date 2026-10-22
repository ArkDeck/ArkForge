use super::*;

pub(super) fn finish_queued_cancellation(
    job: &mut Job,
    now_epoch_ms: u64,
    checkpoint: Sha256Digest,
) -> Result<(), JobError> {
    conclude_cancellation(
        job,
        now_epoch_ms,
        checkpoint,
        "cancelled after the in-flight action reached a durable safe boundary",
    )
}

pub(super) fn conclude_cancellation(
    job: &mut Job,
    now_epoch_ms: u64,
    _prior: Sha256Digest,
    reason: &str,
) -> Result<(), JobError> {
    let step_id = job.current_step_id();
    let event_sequence = job.next_sequence();
    let digest = job.journal.append(
        JournalRecordKind::OutcomeClassified,
        now_epoch_ms,
        1,
        id(&job.job_id)?,
        vec![
            (id("outcome")?, "cancelledSafe".into()),
            (id("reason")?, reason.into()),
            (id("eventSequence")?, event_sequence.to_string()),
        ],
    )?;
    job.move_to(JobState::CancelledSafe)?;
    job.pending = None;
    job.stopped = Some(JobStop::RefusedByAuthority {
        step_id,
        reason: reason.into(),
    });
    job.publish(
        JobEventKind::OutcomeClassified,
        now_epoch_ms,
        digest,
        |event| {
            event.facts.push(KeyValue {
                key: "outcome".into(),
                value: "cancelledSafe".into(),
            });
            event.facts.push(KeyValue {
                key: "reason".into(),
                value: reason.into(),
            });
        },
    );
    Ok(())
}

/// Moves to the next step, or concludes.
pub(super) fn advance(
    job: &mut Job,
    envelope: &FlashPlanEnvelope,
    now_epoch_ms: u64,
    _checkpoint: Sha256Digest,
) -> Result<(), JobError> {
    let requires_rebind = envelope
        .public_steps
        .get(job.step_index)
        .is_some_and(
            |step| match (&step.expected_mode_before, &step.expected_mode_after) {
                (Some(before), Some(after)) => before != after,
                _ => false,
            },
        );
    job.step_index += 1;
    if job.step_index >= envelope.public_steps.len() {
        let reason = "every step completed";
        let event_sequence = job.next_sequence();
        let digest = job.journal.append(
            JournalRecordKind::OutcomeClassified,
            now_epoch_ms,
            1,
            id(&job.job_id)?,
            vec![
                (id("outcome")?, "succeeded".into()),
                (id("reason")?, reason.into()),
                (id("eventSequence")?, event_sequence.to_string()),
            ],
        )?;
        job.move_to(JobState::Postflight)?;
        job.move_to(JobState::Succeeded)?;
        job.stopped = Some(JobStop::Completed);
        job.publish(
            JobEventKind::OutcomeClassified,
            now_epoch_ms,
            digest,
            |event| {
                event.facts.push(KeyValue {
                    key: "outcome".into(),
                    value: "succeeded".into(),
                });
                event.facts.push(KeyValue {
                    key: "reason".into(),
                    value: reason.into(),
                });
            },
        );
        return Ok(());
    }
    if requires_rebind {
        job.move_to(JobState::RebindWait)?;
    } else {
        job.move_to(JobState::Preflight)?;
    }
    Ok(())
}

/// Publishes the admission the job's current step needs.
pub(super) fn request_admission(
    job: &mut Job,
    envelope: &FlashPlanEnvelope,
    private_plan: &StoredProviderPlan,
    admission_facts: &AdmissionFacts,
    now_epoch_ms: u64,
) -> Result<(), JobError> {
    let step = envelope
        .public_steps
        .get(job.step_index)
        .ok_or(JobError::PlanHasNoSteps)?;
    let primary = private_plan
        .actions
        .iter()
        .find(|action| {
            action.step_id == step.step_id && action.role == PrivateActionRole::PrimaryEffect
        })
        .ok_or_else(|| JobError::StepHasNoAction(step.step_id.to_string()))?;

    if let Some(expected_mode) = &step.expected_mode_before
        && expected_mode != &admission_facts.observation.mode
    {
        return Err(JobError::ObservedModeMismatch {
            expected: expected_mode.as_str().to_string(),
            found: admission_facts.observation.mode.as_str().to_string(),
        });
    }
    let freshness = admission_facts.authority_snapshot()?;
    let (serial_evidence_kind, serial_sha256) = match admission_facts.observation.serial_evidence {
        SerialEvidence::Absent => ("absent", Vec::new()),
        SerialEvidence::Descriptor { digest } => ("descriptor", digest.as_bytes().to_vec()),
        SerialEvidence::ProtocolUnique { digest } => ("protocolUnique", digest.as_bytes().to_vec()),
    };
    let snapshot = StepAdmissionSnapshot {
        job_id: job.job_id.clone(),
        plan_id: job.plan_id.clone(),
        plan_sha256: job.plan_digest.as_bytes().to_vec(),
        step_id: step.step_id.to_string(),
        attempt_id: format!("ATTEMPT-{}", job.step_index + 1),
        public_step_sha256: step
            .digest()
            .map_err(|error| JobError::Core(error.to_string()))?
            .as_bytes()
            .to_vec(),
        private_action_sha256: primary
            .digest()
            .map_err(|error| JobError::Core(error.to_string()))?
            .as_bytes()
            .to_vec(),
        effect_set_sha256: envelope
            .effect_set
            .digest()
            .map_err(|error| JobError::Core(error.to_string()))?
            .as_bytes()
            .to_vec(),
        admitted_device_facts_sha256: freshness.device_facts_digest.as_bytes().to_vec(),
        observed_mode: admission_facts.observation.mode.as_str().to_string(),
        observed_at_epoch_ms: admission_facts.observation.observed_at_epoch_ms,
        snapshot_lifetime_ms: SNAPSHOT_LIFETIME_MS,
        request_id: format!("{}-{}", job.job_id, job.step_index + 1),
        topology_sha256: admission_facts
            .observation
            .topology_digest
            .as_bytes()
            .to_vec(),
        descriptor_sha256: admission_facts
            .observation
            .descriptor_digest
            .as_bytes()
            .to_vec(),
        serial_sha256,
        serial_evidence_kind: serial_evidence_kind.to_string(),
        protocol_identity: admission_facts
            .observation
            .protocol_identity
            .iter()
            .map(|fact| KeyValue {
                key: fact.key.to_string(),
                value: fact.value.clone(),
            })
            .collect(),
        identity_strength: admission_facts
            .observation
            .identity_strength
            .as_str()
            .to_string(),
        malformed_descriptor: admission_facts.observation.malformed_descriptor,
        transport_session_sha256: admission_facts.transport_session_digest.as_bytes().to_vec(),
    };

    let digest = job.journal.append(
        JournalRecordKind::StepAdmissionRequested,
        now_epoch_ms,
        1,
        id(&snapshot.step_id)?,
        vec![(id(fact::STEP_ID)?, snapshot.step_id.clone())],
    )?;
    job.pending = Some(Pending::Admission {
        request_id: snapshot.request_id.clone(),
        snapshot: Box::new(snapshot.clone()),
        freshness,
    });
    job.publish(
        JobEventKind::StepAdmissionRequested,
        now_epoch_ms,
        digest,
        |event| event.admission = Some(snapshot),
    );
    Ok(())
}

/// A step's private actions, in the order they must run.
///
/// Read-only sub-actions first, then the single primary effect. The projection
/// validator already guarantees there is exactly one primary; this only puts it
/// last, because a sub-action exists to establish something the primary needs.
pub(super) fn ordered_actions(
    private_plan: &StoredProviderPlan,
    step_id: &str,
) -> Result<Vec<PrivateActionRecord>, JobError> {
    let mut sub: Vec<PrivateActionRecord> = Vec::new();
    let mut primary: Option<PrivateActionRecord> = None;
    for action in &private_plan.actions {
        if action.step_id.as_str() != step_id {
            continue;
        }
        match action.role {
            PrivateActionRole::PrimaryEffect => primary = Some(action.clone()),
            PrivateActionRole::ReadOnlyTransportSubAction => sub.push(action.clone()),
        }
    }
    let primary = primary.ok_or_else(|| JobError::StepHasNoAction(step_id.to_string()))?;
    sub.push(primary);
    Ok(sub)
}

/// Whether this step belongs to the authority's control port, and what it must
/// confirm.
///
/// Read from the plan's own private action rather than from a table kept here:
/// the provider already writes `via: managed-device-control-port` into the
/// action body, and a second copy of that mapping in the daemon is a second
/// copy that can drift.
pub(super) type ManagedControlSpec = (ManagedControlAction, Vec<(String, String)>);

pub(super) fn managed_control_for(
    private_plan: &StoredProviderPlan,
    step_id: &str,
) -> Result<Option<ManagedControlSpec>, JobError> {
    let Some(action) = private_plan
        .actions
        .iter()
        .find(|action| action.step_id.as_str() == step_id)
    else {
        return Err(JobError::StepHasNoAction(step_id.to_string()));
    };
    match StoredAction::decode(action) {
        Ok(StoredAction::ManagedControl {
            control_action,
            expect,
        }) => {
            let action = match control_action.as_str() {
                "enter-updater" => ManagedControlAction::EnterUpdater,
                "reboot-to-normal" => ManagedControlAction::RebootToNormal,
                "read-product-facts" => ManagedControlAction::ReadProductFacts,
                "read-build-facts" => ManagedControlAction::ReadBuildFacts,
                other => return Err(JobError::UnknownControlAction(other.to_string())),
            };
            Ok(Some((action, expect)))
        }
        Ok(_) => Ok(None),
        // An action this daemon cannot decode is not a control action, and
        // guessing that it is would hand it to the wrong side.
        Err(_) => Ok(None),
    }
}

pub(super) fn private_action_digest(
    snapshot: &StepAdmissionSnapshot,
) -> Result<Sha256Digest, JobError> {
    digest_from(&snapshot.private_action_sha256).ok_or(JobError::TagNotADigest)
}

/// The defined evidence digest of an accepted control receipt: SHA-256 over
/// `key=value\n` lines with the keys in byte order.
///
/// Both sides compute this — the authority when it builds an accepted receipt,
/// this daemon before taking one — so the facts and their evidence cannot
/// drift apart. Every fact key the port publishes is ASCII and unique, so byte
/// order is total and the lines cannot collide.
pub fn canonical_facts_digest(facts: &[KeyValue]) -> Sha256Digest {
    let mut ordered: Vec<(&str, &str)> = facts
        .iter()
        .map(|fact| (fact.key.as_str(), fact.value.as_str()))
        .collect();
    ordered.sort();
    let mut bytes = Vec::new();
    for (key, value) in ordered {
        bytes.extend_from_slice(key.as_bytes());
        bytes.push(b'=');
        bytes.extend_from_slice(value.as_bytes());
        bytes.push(b'\n');
    }
    sha256(&bytes)
}

pub(super) fn missing_expected_control_facts(
    expected: &[KeyValue],
    observed: &[KeyValue],
) -> Vec<String> {
    expected
        .iter()
        .filter(|expected| !observed.contains(expected))
        .map(|expected| expected.key.clone())
        .collect()
}

pub(super) fn validate_receipt_fact_keys(facts: &[KeyValue]) -> Result<(), JobError> {
    let mut seen = std::collections::BTreeSet::new();
    for fact in facts {
        OpaqueId::new(&fact.key).map_err(|_| JobError::InvalidReceiptFactKey(fact.key.clone()))?;
        if !seen.insert(fact.key.as_str()) {
            return Err(JobError::DuplicateReceiptFactKey(fact.key.clone()));
        }
    }
    Ok(())
}
