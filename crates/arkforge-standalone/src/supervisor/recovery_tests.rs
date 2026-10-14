use super::*;
use arkforge_ipc::messages::{Effect, PublicStep};
use arkforge_ipc::recovery::ClosedControlEvidence;

struct Fixture {
    root: PathBuf,
    old: AuthorityPlanRecord,
    lineage: ActiveTargetLineage,
    source: RecoverySourceEvidence,
    permit: StepPermit,
    observation: DeviceObservationView,
    probe: DeviceProbeView,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("arkforge-recovery-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let observation = DeviceObservationView {
            observation_id: "OBS-ONE".into(),
            mode: "hdc-normal".into(),
            observed_at_epoch_ms: 100,
            topology_sha256: sha256(b"port").to_hex(),
            descriptor_sha256: sha256(b"descriptor").to_hex(),
            serial_sha256: sha256(b"serial").to_hex(),
            serial_evidence_kind: "usbDescriptor".into(),
            identity_strength: "serialAndTopology".into(),
            malformed_descriptor: false,
            protocol_identity: Vec::new(),
        };
        let probe = DeviceProbeView {
            observation: observation.clone(),
            protocol_facts: Vec::new(),
            profile_id: "profile@1.0.0".into(),
            facts_sha256: sha256(b"probe").to_hex(),
        };
        let stable = stable_identity_digest(&observation, &probe);
        let public = PublicFlashStep {
            step_id: StepId::new("STEP-1").unwrap(),
            kind: FlashStepKind::EnsureMode,
            effect: WorkflowEffect::Mutating,
            cancellation: CancellationPolicy::CancellableAtBoundary,
            binding: BindingRequirement::ExactBoundTargetWithModeLineage,
            semantic_target: Some(SemanticTarget::Device),
            content_digest: None,
            expected_mode_before: Some(DeviceMode::new("hdc-normal").unwrap()),
            expected_mode_after: Some(DeviceMode::new("rockusb-loader").unwrap()),
            private_action_digest: sha256(b"old immutable private action"),
        };
        let old = AuthorityPlanRecord {
            plan: ExecutablePlan {
                plan_id: "PLAN-OLD".into(),
                plan_sha256: sha256(b"old plan").to_hex(),
                public_steps: vec![PublicStep {
                    step_id: "STEP-1".into(),
                    kind: "ensureMode".into(),
                    effect: "mutating".into(),
                    cancellation: public.cancellation.as_str().into(),
                    binding: public.binding.as_str().into(),
                    semantic_target: "device".into(),
                    expected_mode_before: "hdc-normal".into(),
                    expected_mode_after: "rockusb-loader".into(),
                    private_action_sha256: public.private_action_digest.to_hex(),
                    ..Default::default()
                }],
                persistent_effects: vec![Effect {
                    kind: "writePartition".into(),
                    target: "system".into(),
                    range_start: 4096,
                    range_length: 8192,
                    content_sha256: sha256(b"system").to_hex(),
                }],
                ..Default::default()
            },
            binding_id: "BIND-OLD".into(),
            stable_identity_sha256: stable.as_bytes().to_vec(),
            device_id: observation.observation_id.clone(),
            profile_id: probe.profile_id.clone(),
            supersedes_job_id: String::new(),
            topology_sha256: observation.topology_sha256.clone(),
            toolchain_id: "old-toolchain".into(),
        };
        let lineage = ActiveTargetLineage {
            plan_id: old.plan.plan_id.clone(),
            current_device_id: old.device_id.clone(),
            current_stable_identity_sha256: old.stable_identity_sha256.clone(),
            topology_sha256: old.topology_sha256.clone(),
            revision: 1,
        };
        let snapshot = StepAdmissionSnapshot {
            job_id: "JOB-OLD".into(),
            plan_id: old.plan.plan_id.clone(),
            plan_sha256: Sha256Digest::parse_hex(&old.plan.plan_sha256)
                .unwrap()
                .as_bytes()
                .to_vec(),
            step_id: "STEP-1".into(),
            attempt_id: "ATTEMPT-1".into(),
            public_step_sha256: public.digest().unwrap().as_bytes().to_vec(),
            private_action_sha256: public.private_action_digest.as_bytes().to_vec(),
            effect_set_sha256: sha256(b"effect set").as_bytes().to_vec(),
            request_id: "JOB-OLD-1".into(),
            ..Default::default()
        };
        let submission = load_or_create_permit(
            &root,
            5,
            &ControllerPairingSecret::new(PairingEpoch(5), b"fixture only".to_vec()),
            &snapshot,
            &old,
            sha256(b"admitted facts"),
            100,
            200,
        )
        .unwrap();
        let permit = StepPermit::from_canonical_bytes(&submission.permit_cbor).unwrap();
        let source = RecoverySourceEvidence {
            plan_id: old.plan.plan_id.clone(),
            plan_sha256: old.plan.plan_sha256.clone(),
            journal_head_sha256: sha256(b"verified journal tail").to_hex(),
            journal_sequence: 7,
            closed_control: Some(ClosedControlEvidence {
                step_id: "STEP-1".into(),
                permit_id: permit.permit_id.to_string(),
                request_id: "JOB-OLD-1-control".into(),
                action: ManagedControlAction::EnterUpdater,
                dispatch_record_sha256: sha256(b"dispatch record").to_hex(),
                deadline_epoch_ms: 120_101,
                dispatched_at_epoch_ms: 101,
                attempt_id: "ATTEMPT-1".into(),
                controller_session_id: "CLI-SESSION-5".into(),
            }),
        };
        persist_authority_plan(&root, &old).unwrap();
        persist_job_lineage(&root, "JOB-OLD", &lineage).unwrap();
        Self {
            root,
            old,
            lineage,
            source,
            permit,
            observation,
            probe,
        }
    }

    fn replacement(&self) -> (AuthorityPlanRecord, Proof) {
        let mut new = self.old.clone();
        new.plan.plan_id = "PLAN-NEW".into();
        new.plan.plan_sha256 = sha256(b"distinct replacement plan").to_hex();
        new.plan.execution_purpose = "supersedingRecovery".into();
        new.supersedes_job_id = "JOB-OLD".into();
        new.toolchain_id = "new-toolchain".into();
        let proof = Proof {
            assessment: SupersedingRecoveryAssessment {
                job_id: "JOB-OLD".into(),
                eligible: true,
                source: Some(self.source.clone()),
                candidate_plan_id: new.plan.plan_id.clone(),
                candidate_plan_sha256: new.plan.plan_sha256.clone(),
                candidate_artifact_sha256: sha256(b"immutable archive").to_hex(),
                candidate_profile_sha256: sha256(b"profile").to_hex(),
                recovery_contract_sha256: sha256(b"coverage").to_hex(),
                ..Default::default()
            },
            source_authority: sha256(&encode_authority_plan(&self.old))
                .as_bytes()
                .to_vec(),
            source_lineage: lineage_digest(&self.lineage),
            source_permit: source_control_permit(&self.root, "JOB-OLD", &self.source, &self.old)
                .unwrap(),
            replacement_authority: sha256(&encode_authority_plan(&new)).as_bytes().to_vec(),
            issued_at: 130_000,
            expires_at: 230_000,
        };
        (new, proof)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn old_control_requires_the_exact_spent_permit_public_step_and_durable_lineage() {
    let fixture = Fixture::new("source-permit");
    let (_, _, hash) = source_records(&fixture.root, "JOB-OLD", &fixture.source).unwrap();
    assert_eq!(hash.len(), 32);
    let changes: &[fn(&mut StepPermit)] = &[
        |permit| permit.job_id = JobId::new("JOB-OTHER").unwrap(),
        |permit| permit.plan_digest = sha256(b"different plan"),
        |permit| permit.private_action_digest = sha256(b"different action"),
        |permit| permit.public_step_digest = sha256(b"different semantics"),
        |permit| permit.authority_binding.stable_identity_digest = sha256(b"different board"),
        |permit| permit.controller_session_id = ControllerSessionId::new("CLI-SESSION-6").unwrap(),
        |permit| permit.single_use = false,
        |permit| permit.expires_at_epoch_ms = 101,
    ];
    for change in changes {
        let mut permit = fixture.permit.clone();
        change(&mut permit);
        assert!(
            validate_control_permit("JOB-OLD", &fixture.source, &fixture.old, &permit).is_err()
        );
    }
    let mut source = fixture.source.clone();
    source.closed_control.as_mut().unwrap().request_id = "JOB-OTHER-1-control".into();
    assert!(source_records(&fixture.root, "JOB-OLD", &source).is_err());
}

#[test]
fn fresh_identity_rejects_drift_weak_identity_and_changed_probe_facts() {
    let fixture = Fixture::new("identity");
    validate_observations(&fixture.lineage, &fixture.observation, &fixture.probe).unwrap();
    let mut observed = fixture.observation.clone();
    observed.observed_at_epoch_ms += 500;
    validate_observations(&fixture.lineage, &observed, &fixture.probe).unwrap();
    for change in [
        |value: &mut DeviceObservationView| value.serial_sha256 = sha256(b"other serial").to_hex(),
        |value: &mut DeviceObservationView| value.topology_sha256 = sha256(b"other port").to_hex(),
        |value: &mut DeviceObservationView| value.identity_strength = "topologyOnly".into(),
        |value: &mut DeviceObservationView| value.malformed_descriptor = true,
    ] {
        let mut observed = fixture.observation.clone();
        change(&mut observed);
        assert!(validate_observations(&fixture.lineage, &observed, &fixture.probe).is_err());
        let mut probe = fixture.probe.clone();
        change(&mut probe.observation);
        assert!(validate_observations(&fixture.lineage, &fixture.observation, &probe).is_err());
    }
    let mut probe = fixture.probe.clone();
    probe.protocol_facts.push(arkforge_ipc::messages::KeyValue {
        key: "state".into(),
        value: "changed".into(),
    });
    assert!(validate_observations(&fixture.lineage, &fixture.observation, &probe).is_err());
}

#[test]
fn replacement_cannot_change_content_or_identity_and_proof_has_a_hard_budget() {
    let fixture = Fixture::new("replacement");
    let (new, proof) = fixture.replacement();
    validate_replacement(&fixture.old, &fixture.lineage, &new, &proof.assessment).unwrap();
    assert_eq!(Proof::decode(&proof.encode()).unwrap(), proof);
    validate_source_seals(&fixture.root, &new, &proof).unwrap();
    let mut changed = new.clone();
    changed.plan.persistent_effects[0].content_sha256 = sha256(b"other firmware").to_hex();
    assert!(
        validate_replacement(&fixture.old, &fixture.lineage, &changed, &proof.assessment).is_err()
    );
    let mut changed = new.clone();
    changed.stable_identity_sha256 = sha256(b"other board").as_bytes().to_vec();
    assert!(
        validate_replacement(&fixture.old, &fixture.lineage, &changed, &proof.assessment).is_err()
    );
    let mut changed = proof.clone();
    changed.expires_at = changed.issued_at + RECOVERY_BUDGET_MS + 1;
    assert!(Proof::decode(&changed.encode()).is_err());
    let mut changed = proof.clone();
    changed.source_permit.clear();
    assert!(validate_source_seals(&fixture.root, &new, &changed).is_err());
}

#[test]
fn reservation_is_single_use_and_a_crash_before_correlation_cannot_authorize_a_job() {
    let fixture = Fixture::new("reservation");
    let (new, proof) = fixture.replacement();
    persist(
        &path(&fixture.root, &new.plan.plan_id, "proof"),
        &proof.encode(),
    )
    .unwrap();
    assert!(correlate(&fixture.root, &new, "JOB-NEW").is_err());
    let reserved = path(&fixture.root, "JOB-OLD", "reserved");
    persist(&reserved, &proof.encode()).unwrap();
    assert!(persist(&reserved, &proof.encode()).is_err());
    assert!(!path(&fixture.root, "JOB-OLD", "job").exists());
    assert_eq!(read(&reserved).unwrap(), proof.encode());
    correlate(&fixture.root, &new, "JOB-NEW").unwrap();
    assert!(correlate(&fixture.root, &new, "JOB-OTHER").is_err());
    assert_eq!(
        read(&path(&fixture.root, "JOB-OLD", "job")).unwrap(),
        b"JOB-NEW"
    );
    assert!(!path(&fixture.root, "JOB-OLD", "complete").exists());
}

#[test]
fn successful_status_without_all_steps_or_wrong_plan_never_completes_recovery() {
    let fixture = Fixture::new("terminal");
    let (new, _) = fixture.replacement();
    let mut job = JobSummary {
        job_id: "JOB-NEW".into(),
        plan_id: new.plan.plan_id.clone(),
        plan_sha256: Sha256Digest::parse_hex(&new.plan.plan_sha256)
            .unwrap()
            .as_bytes()
            .to_vec(),
        state: "succeeded".into(),
        terminal: true,
        total_steps: 1,
        completed_steps: 1,
        ..Default::default()
    };
    validate_terminal(&new, &job).unwrap();
    job.completed_steps = 0;
    assert!(validate_terminal(&new, &job).is_err());
    job.completed_steps = 1;
    job.state = "outcomeUnknown".into();
    assert!(validate_terminal(&new, &job).is_err());
    job.state = "succeeded".into();
    job.plan_id = "PLAN-OTHER".into();
    assert!(validate_terminal(&new, &job).is_err());
}

#[test]
fn completion_requires_each_write_verification_reset_and_postflight_receipt() {
    use arkforge_ipc::messages::{ActionReceiptSummary, KeyValue};
    let fixture = Fixture::new("receipts");
    let (mut record, proof) = fixture.replacement();
    for (index, kind) in ["writeTarget", "verifyTarget", "reboot", "postflightProbe"]
        .into_iter()
        .enumerate()
    {
        record.plan.public_steps.push(PublicStep {
            step_id: format!("STEP-{}", index + 2),
            kind: kind.into(),
            ..Default::default()
        });
    }
    let job = JobSummary {
        job_id: "JOB-NEW".into(),
        plan_id: record.plan.plan_id.clone(),
        plan_sha256: Sha256Digest::parse_hex(&record.plan.plan_sha256)
            .unwrap()
            .as_bytes()
            .to_vec(),
        state: "succeeded".into(),
        terminal: true,
        total_steps: 5,
        completed_steps: 5,
        ..Default::default()
    };
    let receipts = record
        .plan
        .public_steps
        .iter()
        .enumerate()
        .map(|(index, step)| ActionReceiptSummary {
            job_id: job.job_id.clone(),
            plan_id: job.plan_id.clone(),
            step_id: step.step_id.clone(),
            attempt_id: "ATTEMPT-NEW".into(),
            permit_id: format!("PERMIT-NEW-{index}"),
            disposition: "semanticSuccess".into(),
            evidence_sha256: sha256(step.step_id.as_bytes()).as_bytes().to_vec(),
            verification_outcome: if step.kind == "verifyTarget" {
                "typedSkip"
            } else {
                ""
            }
            .into(),
            typed_skip_reason: if step.kind == "verifyTarget" {
                "lbaReadbackUnsupported"
            } else {
                ""
            }
            .into(),
            facts: if step.kind == "postflightProbe" {
                vec![KeyValue {
                    key: "build".into(),
                    value: "fixture-build".into(),
                }]
            } else {
                Vec::new()
            },
            ..Default::default()
        })
        .collect::<Vec<_>>();
    let complete = |receipts: &[ActionReceiptSummary]| {
        completion_record(
            &record,
            &job,
            &proof.encode(),
            &receipts.iter().collect::<Vec<_>>(),
        )
    };
    assert!(complete(&receipts).is_ok());
    for index in 0..receipts.len() {
        let mut missing = receipts.clone();
        missing.remove(index);
        assert!(complete(&missing).is_err());
    }
    let mut changed = receipts.clone();
    changed[2].verification_outcome = "failed".into();
    assert!(complete(&changed).is_err());
    let mut changed = receipts.clone();
    changed[2].typed_skip_reason.clear();
    assert!(complete(&changed).is_err());
    let mut changed = receipts.clone();
    changed[4].facts.clear();
    assert!(complete(&changed).is_err());
    let mut changed = receipts.clone();
    changed[3].disposition = "outcomeUnknown".into();
    assert!(complete(&changed).is_err());
    let mut changed = receipts.clone();
    changed.push(receipts[0].clone());
    assert!(complete(&changed).is_err());
}
