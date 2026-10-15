//! Evidence for a distinct recovery; never recreates an old private action.

use crate::jobs::Job;
use arkforge_core::digest::{Domain, digest_canonical};
use arkforge_core::ids::OpaqueId;
use arkforge_core::plan::ExecutionPurpose;
use arkforge_core::profile::DeviceProfile;
use arkforge_core::step::{FlashStepKind, SemanticTarget, WorkflowEffect};
use arkforge_core::{PersistentEffect, Sha256Digest};
use arkforge_engine::journal::{JournalRecord, JournalRecordKind};
use arkforge_engine::recovery::PermitLedger;
use arkforge_engine::{JobState, StoredPlan};
use arkforge_ipc::messages::ManagedControlAction;
use arkforge_ipc::recovery::{ClosedControlEvidence, RecoverySourceEvidence};

fn fact<'a>(record: &'a JournalRecord, name: &str) -> Option<&'a str> {
    record
        .facts
        .iter()
        .find(|(key, _)| key.as_str() == name)
        .map(|(_, value)| value.as_str())
}

pub(crate) fn source(job: &Job, now: u64) -> Option<RecoverySourceEvidence> {
    if job.state() != JobState::OutcomeUnknown {
        return None;
    }
    let tail = job.journal().records().last()?;
    Some(RecoverySourceEvidence {
        plan_id: job.plan_id().to_string(),
        plan_sha256: job.plan_digest().to_hex(),
        journal_head_sha256: tail.record_digest.to_hex(),
        journal_sequence: tail.sequence,
        closed_control: first_closed_control(job, now),
    })
}

/// A failed final property read has no persistent effect, but it still leaves
/// a durable unknown intent. Only the exact stored public/private plan and a
/// fully correlated, expired read-only tail can establish this case. This is
/// permission to assess a distinct complete overwrite, never to repeat the read
/// or turn the old job into a success.
pub(crate) fn read_only_postflight_tail(job: &Job, stored: &StoredPlan, now: u64) -> Option<()> {
    use JournalRecordKind::*;
    use arkforge_provider::rockchip_execute::StoredAction;

    let plan = &stored.envelope;
    let step = plan.public_steps.last()?;
    if job.state() != JobState::OutcomeUnknown
        || job.plan_id() != plan.plan_id.as_str()
        || job.plan_digest() != plan.plan_digest
        || step.kind != FlashStepKind::PostflightProbe
        || step.effect != WorkflowEffect::ReadOnly
        || job.completed_steps().checked_add(1)? != plan.public_steps.len()
    {
        return None;
    }
    let actions = stored
        .private_plan
        .actions
        .iter()
        .filter(|action| action.step_id == step.step_id)
        .collect::<Vec<_>>();
    let [action] = actions.as_slice() else {
        return None;
    };
    if action.effect_class != WorkflowEffect::ReadOnly {
        return None;
    }
    let StoredAction::ManagedControl {
        control_action,
        expect,
    } = StoredAction::decode(action).ok()?
    else {
        return None;
    };
    if control_action != ManagedControlAction::ReadBuildFacts.as_str() || expect.is_empty() {
        return None;
    }
    let records = job.journal().records();
    let ledger = PermitLedger::from_journal(job.journal());
    let unresolved = ledger.unresolved();
    let [permit] = unresolved.as_slice() else {
        return None;
    };
    let one = |kind| {
        let mut matching = records.iter().filter(|record| {
            record.kind == kind && fact(record, "permitId") == Some(permit.as_str())
        });
        let record = matching.next()?;
        matching.next().is_none().then_some(record)
    };
    let accepted = one(StepPermitAccepted)?;
    let intent = one(StepIntentRecorded)?;
    let consuming = one(PermitConsuming)?;
    let dispatch = one(ExternalDispatchStarted)?;
    if !(accepted.sequence < intent.sequence
        && intent.sequence < consuming.sequence
        && consuming.sequence < dispatch.sequence)
    {
        return None;
    }
    let attempt = fact(dispatch, "attemptId")?;
    for record in [accepted, intent, consuming, dispatch] {
        if fact(record, "jobId") != Some(job.job_id())
            || fact(record, "planId") != Some(job.plan_id())
            || fact(record, "stepId") != Some(step.step_id.as_str())
            || fact(record, "attemptId") != Some(attempt)
            || record.subject.as_str() != step.step_id.as_str()
        {
            return None;
        }
    }
    for name in [
        "controlAction",
        "controlRequestId",
        "controlDeadlineEpochMs",
    ] {
        if fact(consuming, name) != fact(dispatch, name) {
            return None;
        }
    }
    let request = fact(dispatch, "controlRequestId")?;
    OpaqueId::new(request).ok()?;
    OpaqueId::new(attempt).ok()?;
    let deadline: u64 = fact(dispatch, "controlDeadlineEpochMs")?.parse().ok()?;
    if fact(dispatch, "controlAction") != Some(control_action.as_str())
        || deadline <= dispatch.at_epoch_ms
        || now < deadline
    {
        return None;
    }
    let receipts = job
        .events_from(0)
        .into_iter()
        .filter_map(|event| event.receipt)
        .collect::<Vec<_>>();
    for prior in &plan.public_steps[..plan.public_steps.len() - 1] {
        let matching = receipts
            .iter()
            .filter(|receipt| receipt.step_id == prior.step_id.as_str())
            .collect::<Vec<_>>();
        let [receipt] = matching.as_slice() else {
            return None;
        };
        if receipt.job_id != job.job_id()
            || receipt.plan_id != job.plan_id()
            || receipt.disposition != "semanticSuccess"
            || !receipt.failure_classification.is_empty()
        {
            return None;
        }
        let mut checkpoints = records.iter().filter(|record| {
            record.kind == StepCheckpointed
                && fact(record, "stepId") == Some(prior.step_id.as_str())
        });
        let checkpoint = checkpoints.next()?;
        if checkpoints.next().is_some()
            || checkpoint.sequence >= accepted.sequence
            || fact(checkpoint, "jobId") != Some(job.job_id())
            || fact(checkpoint, "planId") != Some(job.plan_id())
            || fact(checkpoint, "permitId") != Some(receipt.permit_id.as_str())
            || fact(checkpoint, "attemptId") != Some(receipt.attempt_id.as_str())
        {
            return None;
        }
    }
    Some(())
}

/// This narrow legacy proof requires exactly one fully correlated control
/// dispatch, before any checkpoint or other effect. A missing private *write*
/// plan never qualifies. The control request id is the existing external-action
/// identity, not an invented provider action id or a reconstructed receipt.
fn first_closed_control(job: &Job, now: u64) -> Option<ClosedControlEvidence> {
    use JournalRecordKind::*;
    let records = job.journal().records();
    let ledger = PermitLedger::from_journal(job.journal());
    if ledger.iter().count() != 1 || ledger.unresolved().len() != 1 {
        return None;
    }
    if records.iter().any(|record| {
        matches!(
            record.kind,
            CancellationRequested
                | SemanticReceiptRecorded
                | PermitConsumed
                | StepCheckpointed
                | RebindObserved
                | PostflightRecorded
        )
    }) {
        return None;
    }
    let one = |kind| {
        let mut matching = records.iter().filter(|record| record.kind == kind);
        let record = matching.next()?;
        matching.next().is_none().then_some(record)
    };
    let created = one(JobCreated)?;
    let requested = one(StepAdmissionRequested)?;
    let accepted = one(StepPermitAccepted)?;
    let intent = one(StepIntentRecorded)?;
    let consuming = one(PermitConsuming)?;
    let dispatch = one(ExternalDispatchStarted)?;
    if !(created.sequence < requested.sequence
        && requested.sequence < accepted.sequence
        && accepted.sequence < intent.sequence
        && intent.sequence < consuming.sequence
        && consuming.sequence < dispatch.sequence)
        || fact(created, "jobId") != Some(job.job_id())
        || fact(created, "planId") != Some(job.plan_id())
        || fact(created, "planDigest") != Some(job.plan_digest().to_hex().as_str())
    {
        return None;
    }
    let step = fact(dispatch, "stepId")?;
    let permit = fact(dispatch, "permitId")?;
    let attempt = fact(dispatch, "attemptId")?;
    let request = fact(dispatch, "controlRequestId")?;
    if [step, permit, attempt, request]
        .into_iter()
        .any(|value| OpaqueId::new(value).is_err())
        || ledger.unresolved()[0].as_str() != permit
        || fact(requested, "stepId") != Some(step)
        || fact(dispatch, "controlAction") != Some("enter-updater")
    {
        return None;
    }
    for record in [accepted, intent, consuming, dispatch] {
        if fact(record, "jobId") != Some(job.job_id())
            || fact(record, "planId") != Some(job.plan_id())
            || fact(record, "stepId") != Some(step)
            || fact(record, "permitId") != Some(permit)
            || fact(record, "attemptId") != Some(attempt)
            || record.subject.as_str() != step
        {
            return None;
        }
    }
    for name in [
        "controlAction",
        "controlRequestId",
        "controlDeadlineEpochMs",
    ] {
        if fact(consuming, name) != fact(dispatch, name) {
            return None;
        }
    }
    let deadline = fact(dispatch, "controlDeadlineEpochMs")?.parse().ok()?;
    if deadline <= dispatch.at_epoch_ms || now < deadline {
        return None;
    }
    Some(ClosedControlEvidence {
        step_id: step.to_string(),
        permit_id: permit.to_string(),
        request_id: request.to_string(),
        action: ManagedControlAction::EnterUpdater,
        dispatch_record_sha256: dispatch.record_digest.to_hex(),
        deadline_epoch_ms: deadline,
        dispatched_at_epoch_ms: dispatch.at_epoch_ms,
        attempt_id: attempt.to_string(),
        controller_session_id: fact(created, "controllerSessionId")?.to_string(),
    })
}

/// Validate the already sealed replacement against the *published* profile.
/// This is deliberately stricter than a coverage-name intersection: every
/// writable target needs a content-bound write and subsequent verification,
/// followed by reset, exact rebind and artifact-derived postflight facts.
pub(crate) fn validate_candidate(
    candidate: &StoredPlan,
    profile: &DeviceProfile,
    source: &RecoverySourceEvidence,
    possible: &[PersistentEffect],
    now: u64,
) -> Result<(), &'static str> {
    let plan = &candidate.envelope;
    if plan.execution_purpose != ExecutionPurpose::SupersedingRecovery
        || plan.plan_id.as_str() == source.plan_id
        || plan.plan_digest.to_hex() == source.plan_sha256
        || now < plan.created_at_epoch_ms
        || now >= plan.expires_at_epoch_ms
    {
        return Err("the replacement must be a fresh, distinct supersedingRecovery plan");
    }
    if profile.identity().ok().as_ref() != Some(&plan.profile)
        || !profile.recovery.supports_complete_overwrite
        || !profile.recovery.unsupported_states.is_empty()
        || plan.provider.id.as_str() != arkforge_provider::rockchip::PROVIDER_ID
        || profile.allowed_targets.is_empty()
    {
        return Err("the exact published profile does not provide this recovery recipe");
    }
    let contract = plan
        .recovery_contract
        .as_ref()
        .ok_or("missing recovery contract")?;
    if contract.id.as_str() != "arkforge.rockchip.complete-overwrite"
        || contract.version != profile.recovery.version
        || digest_canonical(Domain::RecoveryCoverage, &profile.recovery).ok()
            != Some(contract.digest)
    {
        return Err("the sealed recovery contract does not match published coverage");
    }
    if plan.effect_set.persistent.len() != profile.allowed_targets.len()
        || plan
            .public_steps
            .iter()
            .filter(|step| step.kind == FlashStepKind::WriteTarget)
            .count()
            != profile.allowed_targets.len()
        || plan
            .public_steps
            .iter()
            .filter(|step| step.kind == FlashStepKind::VerifyTarget)
            .count()
            != profile.allowed_targets.len()
    {
        return Err("the replacement does not cover the complete published target set");
    }
    let block_size = profile
        .storage
        .logical_block_size
        .ok_or("unknown block size")? as u64;
    let mut last_verification = 0;
    for target in &profile.allowed_targets {
        if !profile
            .recovery
            .covered_effects
            .iter()
            .any(|name| name.as_str() == target.partition.as_str())
        {
            return Err("a writable target is outside the published recovery coverage");
        }
        let effects = plan
            .effect_set
            .persistent
            .iter()
            .filter(|effect| effect.partition() == Some(&target.partition))
            .collect::<Vec<_>>();
        let [PersistentEffect::WritePartition { range, content, .. }] = effects.as_slice() else {
            return Err("each recovery target requires exactly one content-bound partition write");
        };
        if target.offset_sectors.checked_mul(block_size) != Some(range.start) || range.length == 0 {
            return Err("the recovery write does not match the published partition offset");
        }
        let matching = |kind| {
            plan.public_steps
                .iter()
                .enumerate()
                .filter(|(_, step)| {
                    step.kind == kind
                        && step.semantic_target
                            == Some(SemanticTarget::Partition(target.partition.clone()))
                        && step.content_digest == Some(*content)
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>()
        };
        let writes = matching(FlashStepKind::WriteTarget);
        let verifies = matching(FlashStepKind::VerifyTarget);
        if writes.len() != 1 || verifies.len() != 1 || writes[0] >= verifies[0] {
            return Err("every recovery write requires its own later content-bound verification");
        }
        last_verification = last_verification.max(verifies[0]);
    }
    for old in possible {
        let covered = plan.effect_set.persistent.iter().any(|new| {
            old.partition().is_some()
                && new.partition() == old.partition()
                && match (old.range(), new.range()) {
                    (Some(old), Some(new)) => {
                        new.start <= old.start
                            && new
                                .start
                                .checked_add(new.length)
                                .zip(old.start.checked_add(old.length))
                                .is_some_and(|(new_end, old_end)| new_end >= old_end)
                    }
                    _ => false,
                }
        });
        if !covered {
            return Err("the replacement does not cover every possible persistent byte range");
        }
    }
    let steps = &plan.public_steps;
    let Some(postflight) = steps.last() else {
        return Err("missing postflight");
    };
    let Some(reboot) = steps.get(steps.len().saturating_sub(2)) else {
        return Err("missing reboot");
    };
    if postflight.kind != FlashStepKind::PostflightProbe
        || reboot.kind != FlashStepKind::Reboot
        || last_verification >= steps.len().saturating_sub(2)
        || !plan.postflight.require_exact_target_lineage
        || plan.postflight.require_mode.is_none()
        || plan.postflight.required_runtime_facts.is_empty()
        || reboot.expected_mode_after != plan.postflight.require_mode
        || postflight.expected_mode_before != plan.postflight.require_mode
    {
        return Err("recovery requires reset, exact rebind and complete runtime postflight");
    }
    if let Some(control) = &source.closed_control {
        let first = steps.first().ok_or("missing recovery entry step")?;
        if control.action != ManagedControlAction::EnterUpdater
            || first.kind != FlashStepKind::EnsureMode
            || first.effect < WorkflowEffect::Mutating
            || first.expected_mode_before != plan.postflight.require_mode
            || !profile.mode_transitions.iter().any(|transition| {
                transition.action.as_str() == control.action.as_str()
                    && Some(&transition.from) == first.expected_mode_before.as_ref()
                    && Some(&transition.to) == first.expected_mode_after.as_ref()
                    && transition.rebind.require_disconnect
            })
        {
            return Err(
                "the replacement does not cover the original closed mode-control semantics",
            );
        }
    }
    Ok(())
}

pub(crate) fn parse_digest(text: &str) -> Option<Sha256Digest> {
    Sha256Digest::parse_hex(text).ok()
}
