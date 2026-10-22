use super::*;
use arkforge_engine::{PlanStore, StoredPlan};
use arkforge_ipc::messages::Request;
use arkforge_ipc::recovery::SupersedingRecoveryAssessment;
use arkforge_ipc::{Api, SessionKind, Status, wire};
use arkforged::{Clock, Service};

const AFTER_TIMEOUT: u64 = NOW + 120_003;

fn expired_control(root: &TempRoot, fixture: &Fixture) -> String {
    let mut registry = JobRegistry::open(root.0.join("jobs")).unwrap();
    let job = registry
        .start(
            &fixture.envelope,
            &fixture.private_plan,
            ControllerSessionId::new("SESSION-1").unwrap(),
            &admission_facts(fixture, "hdc-normal", NOW),
            NOW,
        )
        .unwrap();
    let snapshot = pending_snapshot(&mut registry, fixture, &job, NOW);
    let (permit, tag, epoch) = mint(&snapshot, "PERMIT-CLOSED-CONTROL", NOW + 60_000);
    registry
        .submit_permit(
            &job,
            &snapshot.request_id,
            Some((permit, tag, epoch)),
            "",
            &secret(),
            &fixture.envelope,
            &fixture.private_plan,
            &fixture.profile,
            Some(current_facts(&snapshot, NOW + 1)),
            NOW + 1,
        )
        .unwrap();
    assert_eq!(
        registry.expire_stale_controls(AFTER_TIMEOUT),
        vec![job.clone()]
    );
    assert!(registry.take_pending_dispatch().is_none());
    job
}

fn candidate(fixture: &Fixture) -> StoredPlan {
    let mut envelope = fixture.envelope.clone();
    envelope.plan_id = PlanId::new("PLAN-INDEPENDENT-RECOVERY").unwrap();
    envelope.execution_purpose = ExecutionPurpose::SupersedingRecovery;
    envelope.created_at_epoch_ms = AFTER_TIMEOUT;
    envelope.expires_at_epoch_ms = AFTER_TIMEOUT + 3_600_000;
    envelope.plan_digest = envelope.recompute_digest().unwrap();
    StoredPlan {
        envelope,
        private_plan: fixture.private_plan.clone(),
    }
}

fn assess(
    root: &TempRoot,
    profile: &DeviceProfile,
    job: &str,
    candidate: Option<&StoredPlan>,
    now: u64,
) -> SupersedingRecoveryAssessment {
    let mut service = Service::new(
        &root.0,
        vec![profile.clone()],
        Vec::new(),
        Clock::Fixed(now),
        None,
    )
    .unwrap();
    let mut payload = Vec::new();
    wire::write_string(&mut payload, 1, job);
    if let Some(candidate) = candidate {
        wire::write_string(&mut payload, 2, candidate.envelope.plan_id.as_str());
        wire::write_string(&mut payload, 3, &candidate.envelope.plan_digest.to_hex());
    }
    let response = service.handle(
        SessionKind::Controller,
        &Request {
            request_id: "REQ-INDEPENDENT-RECOVERY".into(),
            api: Api::PlanSupersedingRecovery,
            payload,
        },
        None,
    );
    assert_eq!(response.status, Status::Ok);
    assert_eq!(
        service.jobs().job(job).unwrap().state(),
        JobState::OutcomeUnknown
    );
    assert!(service.take_pending_dispatch().is_none());
    assert!(service.take_pending_preparation().is_none());
    assert!(service.take_pending_reconcile().is_none());
    SupersedingRecoveryAssessment::decode(&response.payload).unwrap()
}

fn persist(root: &TempRoot, candidate: &StoredPlan) {
    PlanStore::open(arkforged::plan_storage::FilePlanStorage::new(
        root.0.join("plans"),
    ))
    .unwrap()
    .insert(candidate.clone())
    .unwrap();
}

fn failed_postflight(root: &TempRoot, fixture: &Fixture) -> String {
    let mut registry = JobRegistry::new(root.0.join("jobs"));
    let port = ScriptedPort::default();
    let mut dispatcher =
        arkforged::dispatch::Dispatcher::new(root.0.join("store"), root.0.join("work"), &port);
    stage_archive_into(&root.0.join("store"));
    let job = registry
        .start(
            &fixture.envelope,
            &fixture.private_plan,
            ControllerSessionId::new("SESSION-1").unwrap(),
            &admission_facts(fixture, "hdc-normal", NOW),
            NOW,
        )
        .unwrap();
    let receipts = walk_to_completion(&mut registry, &mut dispatcher, fixture, &job, false);
    assert_eq!(
        registry.job(&job).unwrap().state(),
        JobState::OutcomeUnknown
    );
    assert_eq!(receipts.len() + 1, fixture.envelope.public_steps.len());
    assert_eq!(port.writes().len(), 9);
    job
}

#[test]
fn failed_read_only_postflight_can_be_covered_only_by_a_distinct_complete_plan() {
    let root = TempRoot::new("read-only-postflight-recovery");
    let fixture = plan_fixture();
    let job = failed_postflight(&root, &fixture);
    let original = StoredPlan {
        envelope: fixture.envelope.clone(),
        private_plan: fixture.private_plan.clone(),
    };
    let replacement = candidate(&fixture);
    persist(&root, &replacement);
    let journal = root.0.join("jobs").join(format!("{job}.journal"));
    let original_bytes = std::fs::read(&journal).unwrap();
    let later = AFTER_TIMEOUT + 10_000;

    // A public label or managed-control name cannot replace the stored private
    // action. Nor can an unexpired old read be overtaken by a new epoch.
    assert!(!assess(&root, &fixture.profile, &job, Some(&replacement), later).eligible);
    persist(&root, &original);
    assert!(!assess(&root, &fixture.profile, &job, Some(&replacement), NOW + 500).eligible);
    assert!(!assess(&root, &fixture.profile, &job, Some(&original), later).eligible);

    for _ in 0..2 {
        let initial = assess(&root, &fixture.profile, &job, None, later);
        assert_eq!(initial.blocker_code, "managedControlRecoveryNeedsPlan");
        assert!(initial.source.as_ref().unwrap().closed_control.is_none());
        let result = assess(&root, &fixture.profile, &job, Some(&replacement), later);
        assert!(
            result.eligible,
            "{}: {}",
            result.blocker_code, result.blocker_reason
        );
        assert_eq!(result.source, initial.source);
        assert_eq!(
            result.covered_effects.len(),
            fixture.profile.allowed_targets.len()
        );
        assert_eq!(std::fs::read(&journal).unwrap(), original_bytes);
    }
}

#[test]
fn read_only_tail_does_not_hide_another_unknown_or_a_cancellation() {
    for cancellation in [false, true] {
        let root = TempRoot::new(if cancellation {
            "postflight-recovery-cancel"
        } else {
            "postflight-recovery-other-intent"
        });
        let fixture = plan_fixture();
        let job = failed_postflight(&root, &fixture);
        persist(
            &root,
            &StoredPlan {
                envelope: fixture.envelope.clone(),
                private_plan: fixture.private_plan.clone(),
            },
        );
        let replacement = candidate(&fixture);
        persist(&root, &replacement);
        let path = root.0.join("jobs").join(format!("{job}.journal"));
        let (mut journal, _) = DurableJournal::open(&path).unwrap();
        let facts = if cancellation {
            Vec::new()
        } else {
            [
                ("jobId", job.as_str()),
                ("planId", fixture.envelope.plan_id.as_str()),
                ("stepId", "STEP-OTHER"),
                ("permitId", "PERMIT-OTHER"),
                ("attemptId", "ATTEMPT-OTHER"),
            ]
            .into_iter()
            .map(|(key, value)| (OpaqueId::new(key).unwrap(), value.to_string()))
            .collect()
        };
        journal
            .append(
                if cancellation {
                    JournalRecordKind::CancellationRequested
                } else {
                    JournalRecordKind::StepIntentRecorded
                },
                AFTER_TIMEOUT + 1,
                1,
                OpaqueId::new(&job).unwrap(),
                facts,
            )
            .unwrap();
        drop(journal);
        let bytes = std::fs::read(&path).unwrap();
        let result = assess(
            &root,
            &fixture.profile,
            &job,
            Some(&replacement),
            AFTER_TIMEOUT + 10_000,
        );
        assert!(!result.eligible);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn expired_first_control_proof_survives_restart_without_recreating_the_original_plan() {
    let root = TempRoot::new("independent-closed-control");
    let fixture = plan_fixture();
    let job = expired_control(&root, &fixture);
    let journal = root.0.join("jobs").join(format!("{job}.journal"));
    let original_bytes = std::fs::read(&journal).unwrap();
    let initial = assess(&root, &fixture.profile, &job, None, AFTER_TIMEOUT);
    assert!(!initial.eligible);
    assert_eq!(initial.blocker_code, "managedControlRecoveryNeedsPlan");
    let source = initial.source.as_ref().unwrap();
    assert_eq!(source.plan_id, fixture.envelope.plan_id.as_str());
    assert_eq!(source.plan_sha256, fixture.envelope.plan_digest.to_hex());
    let control = source.closed_control.as_ref().unwrap();
    assert_eq!(control.action, ManagedControlAction::EnterUpdater);
    assert_eq!(
        control.step_id,
        fixture.envelope.public_steps[0].step_id.as_str()
    );
    assert_eq!(control.permit_id, "PERMIT-CLOSED-CONTROL");
    assert_eq!(control.dispatched_at_epoch_ms, NOW + 1);
    assert_eq!(control.deadline_epoch_ms, NOW + 120_001);
    let replacement = candidate(&fixture);
    persist(&root, &replacement);
    for _ in 0..2 {
        let assessed = assess(
            &root,
            &fixture.profile,
            &job,
            Some(&replacement),
            AFTER_TIMEOUT,
        );
        assert!(
            assessed.eligible,
            "{}: {}",
            assessed.blocker_code, assessed.blocker_reason
        );
        assert_eq!(assessed.source, initial.source);
        assert_eq!(
            assessed.candidate_artifact_sha256,
            fixture.envelope.artifact.content_digest.to_hex()
        );
        assert_eq!(
            assessed.covered_effects.len(),
            fixture.profile.allowed_targets.len()
        );
        assert_eq!(std::fs::read(&journal).unwrap(), original_bytes);
        let store = PlanStore::open(arkforged::plan_storage::FilePlanStorage::new(
            root.0.join("plans"),
        ))
        .unwrap();
        assert_eq!(store.len(), 1);
        assert!(
            store
                .get(&fixture.envelope.plan_id, fixture.envelope.plan_digest)
                .is_err()
        );
    }
}

#[test]
fn closed_control_does_not_qualify_before_its_deadline_or_with_pending_cancellation() {
    let root = TempRoot::new("closed-control-cancel");
    let fixture = plan_fixture();
    let job = expired_control(&root, &fixture);
    let early = assess(&root, &fixture.profile, &job, None, NOW + 5);
    assert!(!early.eligible);
    assert!(early.source.unwrap().closed_control.is_none());
    let path = root.0.join("jobs").join(format!("{job}.journal"));
    let (mut journal, _) = DurableJournal::open(&path).unwrap();
    journal
        .append(
            JournalRecordKind::CancellationRequested,
            AFTER_TIMEOUT + 1,
            1,
            OpaqueId::new(&job).unwrap(),
            Vec::new(),
        )
        .unwrap();
    drop(journal);
    let cancelled = assess(&root, &fixture.profile, &job, None, AFTER_TIMEOUT + 2);
    assert!(!cancelled.eligible);
    assert!(cancelled.source.unwrap().closed_control.is_none());
}

#[test]
fn primary_plan_expired_replacement_and_changed_profile_are_ineligible() {
    let root = TempRoot::new("closed-control-candidate-refusals");
    let fixture = plan_fixture();
    let job = expired_control(&root, &fixture);
    let primary = StoredPlan {
        envelope: fixture.envelope.clone(),
        private_plan: fixture.private_plan.clone(),
    };
    persist(&root, &primary);
    assert!(!assess(&root, &fixture.profile, &job, Some(&primary), AFTER_TIMEOUT).eligible);
    let replacement = candidate(&fixture);
    persist(&root, &replacement);
    assert!(
        !assess(
            &root,
            &fixture.profile,
            &job,
            Some(&replacement),
            replacement.envelope.expires_at_epoch_ms
        )
        .eligible
    );
    let mut changed = fixture.profile.clone();
    changed.recovery.supports_complete_overwrite = false;
    assert!(!assess(&root, &changed, &job, Some(&replacement), AFTER_TIMEOUT).eligible);
}

#[test]
fn a_second_unresolved_action_prevents_the_closed_control_exception() {
    let root = TempRoot::new("closed-control-second-intent");
    let fixture = plan_fixture();
    let job = expired_control(&root, &fixture);
    let path = root.0.join("jobs").join(format!("{job}.journal"));
    let (mut journal, _) = DurableJournal::open(path).unwrap();
    let facts = [
        ("jobId", job.as_str()),
        ("planId", fixture.envelope.plan_id.as_str()),
        ("stepId", "STEP-OTHER"),
        ("permitId", "PERMIT-OTHER"),
        ("attemptId", "ATTEMPT-2"),
    ]
    .into_iter()
    .map(|(key, value)| (OpaqueId::new(key).unwrap(), value.to_string()))
    .collect();
    journal
        .append(
            JournalRecordKind::StepIntentRecorded,
            AFTER_TIMEOUT + 1,
            1,
            OpaqueId::new("STEP-OTHER").unwrap(),
            facts,
        )
        .unwrap();
    drop(journal);
    let result = assess(&root, &fixture.profile, &job, None, AFTER_TIMEOUT + 2);
    assert!(!result.eligible);
    assert!(result.source.unwrap().closed_control.is_none());
    assert_eq!(result.blocker_code, "effectsUnbounded");
}

#[test]
fn incomplete_overwrite_or_missing_verification_reset_or_postflight_is_refused() {
    use arkforge_core::projection::validate_projection;
    use arkforge_core::step::FlashStepKind;
    for (index, remove) in [
        FlashStepKind::WriteTarget,
        FlashStepKind::VerifyTarget,
        FlashStepKind::Reboot,
        FlashStepKind::PostflightProbe,
    ]
    .into_iter()
    .enumerate()
    {
        let root = TempRoot::new(&format!("incomplete-recovery-{index}"));
        let fixture = plan_fixture();
        let job = expired_control(&root, &fixture);
        let mut replacement = candidate(&fixture);
        let removed = replacement
            .envelope
            .public_steps
            .iter()
            .find(|step| step.kind == remove)
            .unwrap()
            .clone();
        replacement
            .envelope
            .public_steps
            .retain(|step| step.step_id != removed.step_id);
        replacement
            .private_plan
            .actions
            .retain(|action| action.step_id != removed.step_id);
        if remove == FlashStepKind::WriteTarget {
            replacement.envelope.effect_set.persistent.retain(|effect| {
                removed
                    .semantic_target
                    .as_ref()
                    .is_none_or(|target| match target {
                        arkforge_core::step::SemanticTarget::Partition(partition) => {
                            effect.partition() != Some(partition)
                        }
                        _ => true,
                    })
            });
        }
        if remove == FlashStepKind::Reboot {
            replacement
                .envelope
                .effect_set
                .transient
                .retain(|effect| !matches!(effect, arkforge_core::TransientEffect::Reboot { .. }));
        }
        let projection = validate_projection(
            &replacement.envelope.public_steps,
            &replacement.private_plan,
            &replacement.envelope.effect_set,
        )
        .unwrap();
        replacement.envelope.per_action_digests = projection.per_action;
        replacement.envelope.provider_execution_plan_digest =
            projection.provider_execution_plan_digest;
        replacement.envelope.public_projection_digest = projection.public_projection_digest;
        replacement.envelope.plan_digest = replacement.envelope.recompute_digest().unwrap();
        persist(&root, &replacement);
        let result = assess(
            &root,
            &fixture.profile,
            &job,
            Some(&replacement),
            AFTER_TIMEOUT,
        );
        assert!(!result.eligible, "missing {remove:?} must not qualify");
        assert_eq!(result.blocker_code, "recoveryCandidateInvalid");
    }
}
