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
            serial_evidence_kind: "descriptor".into(),
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
            additional_sources: Vec::new(),
            budget: None,
            identity_migration: None,
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
    validate_replacement(
        &fixture.old,
        &fixture.lineage,
        &new,
        &proof.assessment,
        None,
    )
    .unwrap();
    assert_eq!(Proof::decode(&proof.encode()).unwrap(), proof);
    validate_source_seals(&fixture.root, &new, &proof).unwrap();
    let mut changed = new.clone();
    changed.plan.persistent_effects[0].content_sha256 = sha256(b"other firmware").to_hex();
    assert!(
        validate_replacement(
            &fixture.old,
            &fixture.lineage,
            &changed,
            &proof.assessment,
            None
        )
        .is_err()
    );
    let mut changed = new.clone();
    changed.stable_identity_sha256 = sha256(b"other board").as_bytes().to_vec();
    assert!(
        validate_replacement(
            &fixture.old,
            &fixture.lineage,
            &changed,
            &proof.assessment,
            None
        )
        .is_err()
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
    use arkforge_core::CanonicalCbor;
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
                vec![
                    KeyValue {
                        key: "build".into(),
                        value: "fixture-build".into(),
                    },
                    KeyValue {
                        key: "mode".into(),
                        value: "hdc-normal".into(),
                    },
                ]
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
    let original = complete(&receipts).unwrap();
    let persisted = path(&fixture.root, "JOB-OLD", "complete");
    persist(&persisted, &original).unwrap();
    let replayed = receipts
        .iter()
        .map(|receipt| {
            ActionReceiptSummary::from_canonical_bytes(&receipt.to_canonical_bytes().unwrap())
                .unwrap()
        })
        .collect::<Vec<_>>();
    let after_restart = complete(&replayed).unwrap();
    assert_ne!(
        original, after_restart,
        "canonical map replay changes order"
    );
    assert_eq!(
        normalize_completion_receipts(&read(&persisted).unwrap()).unwrap(),
        normalize_completion_receipts(&after_restart).unwrap()
    );
    assert_eq!(read(&persisted).unwrap(), original);
    let mut changed = replayed.clone();
    changed[4].facts[0].value = "different actual property".into();
    assert_ne!(
        normalize_completion_receipts(&original).unwrap(),
        normalize_completion_receipts(&complete(&changed).unwrap()).unwrap()
    );
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

#[test]
fn completion_normalization_preserves_every_receipt_and_envelope_value() {
    use arkforge_ipc::messages::{ActionReceiptSummary, KeyValue};
    let receipt = ActionReceiptSummary {
        job_id: "JOB-NEW".into(),
        plan_id: "PLAN-NEW".into(),
        step_id: "STEP-NEW".into(),
        action_id: "ACTION-NEW".into(),
        attempt_id: "ATTEMPT-NEW".into(),
        permit_id: "PERMIT-NEW".into(),
        disposition: "semanticSuccess".into(),
        evidence_sha256: sha256(b"evidence").as_bytes().to_vec(),
        verification_outcome: "verified".into(),
        verification_strength: "fullHash".into(),
        verified_range_start: 4096,
        verified_range_length: 8192,
        facts: vec![KeyValue {
            key: "mode".into(),
            value: "normal".into(),
        }],
        ..Default::default()
    };
    let envelope = |body: &[u8]| {
        let mut out = Vec::new();
        for field in 1..=5 {
            wire::write_message(&mut out, field, &[field as u8]);
        }
        wire::write_message(&mut out, 6, body);
        out
    };
    let original = envelope(&receipt.encode());
    let normalized = normalize_completion_receipts(&original).unwrap();
    for change in [
        |r: &mut ActionReceiptSummary| r.job_id.push_str("-OTHER"),
        |r: &mut ActionReceiptSummary| r.plan_id.push_str("-OTHER"),
        |r: &mut ActionReceiptSummary| r.step_id.push_str("-OTHER"),
        |r: &mut ActionReceiptSummary| r.action_id.push_str("-OTHER"),
        |r: &mut ActionReceiptSummary| r.attempt_id.push_str("-OTHER"),
        |r: &mut ActionReceiptSummary| r.permit_id.push_str("-OTHER"),
        |r: &mut ActionReceiptSummary| r.disposition = "outcomeUnknown".into(),
        |r: &mut ActionReceiptSummary| r.evidence_sha256[0] ^= 1,
        |r: &mut ActionReceiptSummary| {
            r.verification_outcome = "typedSkip".into();
            r.verification_strength.clear();
        },
        |r: &mut ActionReceiptSummary| r.verification_strength = "sampledRanges".into(),
        |r: &mut ActionReceiptSummary| r.verified_range_start += 1,
        |r: &mut ActionReceiptSummary| r.verified_range_length += 1,
        |r: &mut ActionReceiptSummary| r.typed_skip_reason = "reason".into(),
        |r: &mut ActionReceiptSummary| r.failure_classification = "failure".into(),
        |r: &mut ActionReceiptSummary| r.facts[0].key = "other".into(),
        |r: &mut ActionReceiptSummary| r.facts[0].value = "loader".into(),
    ] {
        let mut changed = receipt.clone();
        change(&mut changed);
        assert_ne!(
            normalized,
            normalize_completion_receipts(&envelope(&changed.encode())).unwrap()
        );
    }
    for selected in 1..=6 {
        let mut reader = wire::Reader::new(&original);
        let mut changed = Vec::new();
        while let Some((field, value)) = reader.next_field().unwrap() {
            let bytes = value.as_bytes().unwrap();
            wire::write_message(
                &mut changed,
                field,
                if field == selected && field != 6 {
                    b"changed"
                } else {
                    bytes
                },
            );
            if field == selected {
                wire::write_message(&mut changed, field, bytes);
            }
        }
        assert_ne!(normalized, normalize_completion_receipts(&changed).unwrap());
    }
    let mut duplicate_facts = receipt.clone();
    duplicate_facts.facts.push(receipt.facts[0].clone());
    assert!(normalize_completion_receipts(&envelope(&duplicate_facts.encode())).is_err());
    for field in [1, 99] {
        let mut changed = receipt.encode();
        wire::write_string(&mut changed, field, "unexpected-or-duplicate");
        assert!(normalize_completion_receipts(&envelope(&changed)).is_err());
    }
    let mut unknown = original.clone();
    wire::write_string(&mut unknown, 99, "unknown completion field");
    assert!(normalize_completion_receipts(&unknown).is_err());
    assert!(normalize_completion_receipts(&original[..original.len() - 1]).is_err());
}

fn with_additional_source(fixture: &Fixture, proof: &Proof) -> Proof {
    let mut second = proof.clone();
    second.assessment.job_id = "JOB-SECOND".into();
    let source = second.assessment.source.as_mut().unwrap();
    source.closed_control = None;
    source.plan_id = "PLAN-SECOND".into();
    source.plan_sha256 = sha256(b"second unknown plan").to_hex();
    source.journal_head_sha256 = sha256(b"second journal").to_hex();
    let mut old = fixture.old.clone();
    old.plan.plan_id = source.plan_id.clone();
    old.plan.plan_sha256 = source.plan_sha256.clone();
    let mut lineage = fixture.lineage.clone();
    lineage.plan_id = source.plan_id.clone();
    persist_authority_plan(&fixture.root, &old).unwrap();
    persist_job_lineage(&fixture.root, "JOB-SECOND", &lineage).unwrap();
    second.source_authority = sha256(&encode_authority_plan(&old)).as_bytes().to_vec();
    second.source_lineage = lineage_digest(&lineage);
    second.source_permit.clear();
    let mut set = proof.clone();
    set.additional_sources.push(second);
    set
}

#[test]
fn complete_set_proof_seals_every_source_and_keeps_legacy_proof_bytes() {
    let fixture = Fixture::new("set-seals");
    let (new, old_proof) = fixture.replacement();
    let old_bytes = old_proof.encode();
    assert_eq!(Proof::decode(&old_bytes).unwrap().encode(), old_bytes);
    let proof = with_additional_source(&fixture, &old_proof);
    let decoded = Proof::decode(&proof.encode()).unwrap();
    assert_eq!(decoded.sources().count(), 2);
    for source in decoded.sources() {
        validate_source_seals(&fixture.root, &new, source).unwrap();
    }
    let mut changed = decoded.additional_sources[0].clone();
    changed.source_lineage = sha256(b"changed source lineage").as_bytes().to_vec();
    assert!(validate_source_seals(&fixture.root, &new, &changed).is_err());
    changed = decoded.additional_sources[0].clone();
    changed.assessment.source.as_mut().unwrap().plan_sha256 =
        sha256(b"different historical plan").to_hex();
    assert!(validate_source_seals(&fixture.root, &new, &changed).is_err());
    assert_eq!(old_proof.encode(), old_bytes);
}

#[test]
fn source_set_refuses_omissions_additions_duplicates_nesting_and_budget_drift() {
    let fixture = Fixture::new("set-format");
    let (_, legacy) = fixture.replacement();
    let proof = with_additional_source(&fixture, &legacy);
    let expected = vec!["JOB-OLD".into(), "JOB-SECOND".into()];
    validate_source_set(&expected, &expected).unwrap();
    assert!(validate_source_set(&expected, &expected[..1]).is_err());
    assert!(
        validate_source_set(
            &[
                "JOB-NEW-UNKNOWN".into(),
                "JOB-OLD".into(),
                "JOB-SECOND".into()
            ],
            &expected
        )
        .is_err()
    );
    assert!(validate_source_set(&[], &expected).is_err());
    for change in [
        |source: &mut Proof| source.assessment.job_id = "JOB-OLD".into(),
        |source: &mut Proof| source.expires_at += 1,
        |source: &mut Proof| source.issued_at -= 1,
        |source: &mut Proof| source.assessment.candidate_plan_id = "PLAN-OTHER".into(),
        |source: &mut Proof| {
            source.assessment.candidate_artifact_sha256 = sha256(b"different image").to_hex()
        },
        |source: &mut Proof| {
            source.assessment.recovery_contract_sha256 = sha256(b"wider coverage").to_hex()
        },
        |source: &mut Proof| {
            source.replacement_authority = sha256(b"different authority").as_bytes().to_vec()
        },
    ] {
        let mut changed = proof.clone();
        change(&mut changed.additional_sources[0]);
        assert!(Proof::decode(&changed.encode()).is_err());
    }
    let mut nested = proof.clone();
    nested.additional_sources[0]
        .additional_sources
        .push(legacy.clone());
    assert!(Proof::decode(&nested.encode()).is_err());
    let mut duplicate = proof.clone();
    duplicate
        .additional_sources
        .push(proof.additional_sources[0].clone());
    assert!(Proof::decode(&duplicate.encode()).is_err());
    let mut oversized = legacy.clone();
    for index in 0..MAX_RECOVERY_SOURCES {
        let mut source = legacy.clone();
        source.assessment.job_id = format!("JOB-SOURCE-{index:03}");
        oversized.additional_sources.push(source);
    }
    assert!(Proof::decode(&oversized.encode()).is_err());
}

#[test]
fn complete_set_reserves_and_correlates_all_sources_exactly_once() {
    let fixture = Fixture::new("set-reservation");
    let (new, legacy) = fixture.replacement();
    let proof = with_additional_source(&fixture, &legacy);
    persist(
        &path(&fixture.root, &new.plan.plan_id, "proof"),
        &proof.encode(),
    )
    .unwrap();
    reserve_sources(&fixture.root, &proof).unwrap();
    assert!(validate_reservations(&fixture.root, &proof, "JOB-NEW").is_err());
    correlate(&fixture.root, &new, "JOB-NEW").unwrap();
    validate_reservations(
        &fixture.root,
        &Proof::decode(&proof.encode()).unwrap(),
        "JOB-NEW",
    )
    .unwrap();
    assert!(validate_reservations(&fixture.root, &proof, "JOB-DIFFERENT").is_err());
    assert!(reserve_sources(&fixture.root, &proof).is_err());
    assert!(correlate(&fixture.root, &new, "JOB-DIFFERENT").is_err());
    for source in proof.sources() {
        assert_eq!(
            read(&path(&fixture.root, &source.assessment.job_id, "reserved")).unwrap(),
            proof.encode()
        );
        assert_eq!(
            read(&path(&fixture.root, &source.assessment.job_id, "job")).unwrap(),
            b"JOB-NEW"
        );
        assert!(!path(&fixture.root, &source.assessment.job_id, "complete").exists());
    }
}

#[test]
fn historical_spent_budget_blocks_a_new_source_without_overwriting_any_record() {
    let fixture = Fixture::new("set-spent");
    let (new, legacy) = fixture.replacement();
    let proof = with_additional_source(&fixture, &legacy);
    let reserved = path(&fixture.root, "JOB-OLD", "reserved");
    persist(&reserved, &legacy.encode()).unwrap();
    persist(&path(&fixture.root, "JOB-OLD", "job"), b"JOB-SECOND").unwrap();
    let original = read(&reserved).unwrap();
    let pending = vec!["JOB-OLD".into(), "JOB-SECOND".into()];
    for _restart in 0..2 {
        let error = require_unspent(&fixture.root, &pending).unwrap_err();
        assert_eq!(error.code, "RECOVERY_PROOF_REQUIRED");
        assert!(error.message.contains("budget exhausted for [JOB-OLD]"));
        assert!(
            error
                .message
                .contains("complete outstanding set is [JOB-OLD, JOB-SECOND]")
        );
        assert!(reserve_sources(&fixture.root, &proof).is_err());
        assert_eq!(read(&reserved).unwrap(), original);
        assert_eq!(
            read(&path(&fixture.root, "JOB-OLD", "job")).unwrap(),
            b"JOB-SECOND"
        );
        assert!(!path(&fixture.root, "JOB-SECOND", "reserved").exists());
        assert!(!path(&fixture.root, &new.plan.plan_id, "proof").exists());
    }
}

#[test]
fn partial_set_publication_is_spent_and_never_authorizes_an_orphan() {
    let fixture = Fixture::new("set-partial");
    let (new, legacy) = fixture.replacement();
    let proof = with_additional_source(&fixture, &legacy);
    persist(
        &path(&fixture.root, &new.plan.plan_id, "proof"),
        &proof.encode(),
    )
    .unwrap();
    persist(&path(&fixture.root, "JOB-OLD", "reserved"), &proof.encode()).unwrap();
    assert!(correlate(&fixture.root, &new, "JOB-ORPHAN").is_err());
    assert!(!path(&fixture.root, "JOB-OLD", "job").exists());
    assert!(validate_reservations(&fixture.root, &proof, "JOB-ORPHAN").is_err());
    assert!(reserve_sources(&fixture.root, &proof).is_err());
    persist(
        &path(&fixture.root, "JOB-SECOND", "reserved"),
        &proof.encode(),
    )
    .unwrap();
    persist(&path(&fixture.root, "JOB-OLD", "job"), b"JOB-ORPHAN").unwrap();
    assert!(validate_reservations(&fixture.root, &proof, "JOB-ORPHAN").is_err());
    assert!(!path(&fixture.root, "JOB-SECOND", "job").exists());
}

fn grant(fixture: &Fixture, name: &str) -> (AuthorityPlanRecord, Proof) {
    let (mut record, mut proof) = fixture.replacement();
    record.plan.plan_id = format!("PLAN-{name}");
    record.plan.plan_sha256 = sha256(name.as_bytes()).to_hex();
    proof.assessment.candidate_plan_id = record.plan.plan_id.clone();
    proof.assessment.candidate_plan_sha256 = record.plan.plan_sha256.clone();
    proof.replacement_authority = sha256(&encode_authority_plan(&record)).as_bytes().to_vec();
    proof.budget = Some(budget::next(&fixture.root).unwrap());
    persist(
        &path(&fixture.root, &record.plan.plan_id, "proof"),
        &proof.encode(),
    )
    .unwrap();
    (record, proof)
}

#[test]
fn independent_grant_preserves_spent_legacy_records_and_requires_exact_new_correlation() {
    let fixture = Fixture::new("independent-grant");
    let (old_replacement, old_proof) = fixture.replacement();
    persist(
        &path(&fixture.root, &old_replacement.plan.plan_id, "proof"),
        &old_proof.encode(),
    )
    .unwrap();
    reserve_sources(&fixture.root, &old_proof).unwrap();
    correlate(&fixture.root, &old_replacement, "JOB-FAILED-REPLACEMENT").unwrap();
    let before = ["reserved", "job"].map(|ext| read(&path(&fixture.root, "JOB-OLD", ext)).unwrap());
    let (record, proof) = grant(&fixture, "INDEPENDENT");
    assert_eq!(proof.budget.as_ref().unwrap().ordinal, 2);
    assert_eq!(Proof::decode(&proof.encode()).unwrap(), proof);
    assert!(validate_reservations(&fixture.root, &proof, "JOB-NEW").is_err());
    reserve_sources(&fixture.root, &proof).unwrap();
    assert!(validate_reservations(&fixture.root, &proof, "JOB-NEW").is_err());
    correlate(&fixture.root, &record, "JOB-NEW").unwrap();
    validate_reservations(&fixture.root, &proof, "JOB-NEW").unwrap();
    assert!(validate_reservations(&fixture.root, &proof, "JOB-OLD").is_err());
    assert!(correlate(&fixture.root, &record, "JOB-OTHER").is_err());
    assert!(reserve_sources(&fixture.root, &proof).is_err());
    assert_eq!(budget::next(&fixture.root).unwrap().ordinal, 3);
    assert_eq!(
        before,
        ["reserved", "job"].map(|ext| read(&path(&fixture.root, "JOB-OLD", ext)).unwrap())
    );
    assert!(!path(&fixture.root, "JOB-OLD", "complete").exists());
}

#[test]
fn competing_plans_share_one_ordinal_and_orphaned_consumption_is_not_refunded() {
    let fixture = Fixture::new("grant-race");
    let (_, first) = grant(&fixture, "RACE-A");
    let (_, second) = grant(&fixture, "RACE-B");
    assert_eq!(first.budget, second.budget);
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            reserve_sources(&fixture.root, &first)
        });
        let b = scope.spawn(|| {
            barrier.wait();
            reserve_sources(&fixture.root, &second)
        });
        [a.join().unwrap(), b.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(budget::grants(&fixture.root).unwrap().len(), 1);
    for _restart in 0..2 {
        assert_eq!(budget::next(&fixture.root).unwrap().ordinal, 2);
        assert!(validate_reservations(&fixture.root, &first, "JOB-ORPHAN").is_err());
        assert!(validate_reservations(&fixture.root, &second, "JOB-ORPHAN").is_err());
    }
}

#[test]
fn all_consumed_epochs_count_toward_a_fixed_limit_even_without_jobs_or_after_expiry() {
    let fixture = Fixture::new("grant-limit");
    for ordinal in 1..=budget::MAX_EPOCHS {
        let (_, proof) = grant(&fixture, &format!("LIMIT-{ordinal}"));
        assert_eq!(proof.budget.as_ref().unwrap().ordinal, ordinal);
        reserve_sources(&fixture.root, &proof).unwrap();
    }
    assert_eq!(budget::grants(&fixture.root).unwrap().len(), 16);
    for _restart in 0..2 {
        assert!(
            budget::next(&fixture.root)
                .unwrap_err()
                .message
                .contains("16 independent epochs")
        );
    }
}

#[test]
fn grant_chain_rejects_changed_legacy_records_missing_middle_and_orphan_correlations() {
    let fixture = Fixture::new("grant-chain");
    let (_, old) = fixture.replacement();
    persist(
        &path(&fixture.root, &old.assessment.candidate_plan_id, "proof"),
        &old.encode(),
    )
    .unwrap();
    reserve_sources(&fixture.root, &old).unwrap();
    let (_, first) = grant(&fixture, "CHAIN-A");
    reserve_sources(&fixture.root, &first).unwrap();
    let (_, second) = grant(&fixture, "CHAIN-B");
    reserve_sources(&fixture.root, &second).unwrap();
    let legacy = path(&fixture.root, "JOB-OLD", "reserved");
    std::fs::remove_file(&legacy).unwrap();
    assert!(budget::next(&fixture.root).is_err());
    persist(&legacy, &old.encode()).unwrap();
    budget::next(&fixture.root).unwrap();
    let middle = budget::path(&fixture.root, 2, "grant");
    std::fs::remove_file(&middle).unwrap();
    assert!(budget::next(&fixture.root).is_err());
    persist(&middle, &first.encode()).unwrap();
    std::fs::write(&middle, b"corrupt").unwrap();
    assert!(budget::next(&fixture.root).is_err());
    std::fs::write(&middle, first.encode()).unwrap();
    persist(&budget::path(&fixture.root, 4, "job"), b"JOB-NO-GRANT").unwrap();
    assert!(budget::next(&fixture.root).is_err());
}

#[test]
fn budget_seal_cannot_be_nested_widened_or_changed_after_commit() {
    let fixture = Fixture::new("grant-seal");
    let (_, proof) = grant(&fixture, "SEALED");
    reserve_sources(&fixture.root, &proof).unwrap();
    for mutate in [
        |seal: &mut budget::Seal| seal.ordinal = 0,
        |seal: &mut budget::Seal| seal.ordinal = 17,
        |seal: &mut budget::Seal| seal.previous.clear(),
    ] {
        let mut changed = proof.clone();
        mutate(changed.budget.as_mut().unwrap());
        assert!(Proof::decode(&changed.encode()).is_err());
    }
    let mut changed = proof.clone();
    changed.budget.as_mut().unwrap().previous = sha256(b"other ledger").as_bytes().to_vec();
    assert!(budget::validate(&fixture.root, &changed).is_err());
    let mut nested = proof.clone();
    nested.additional_sources.push(proof.clone());
    assert!(Proof::decode(&nested.encode()).is_err());
}

#[test]
fn new_grant_covers_all_sources_without_extending_an_expired_grant() {
    let fixture = Fixture::new("grant-set-time");
    let (_, old) = fixture.replacement();
    let set = with_additional_source(&fixture, &old);
    persist(
        &path(&fixture.root, &set.assessment.candidate_plan_id, "proof"),
        &set.encode(),
    )
    .unwrap();
    reserve_sources(&fixture.root, &set).unwrap();
    // Both source reservations represent one consumed legacy epoch.
    assert_eq!(budget::next(&fixture.root).unwrap().ordinal, 2);
    let (mut record, mut new) = fixture.replacement();
    record.plan.plan_id = "PLAN-COMPLETE-SET-GRANT".into();
    record.plan.plan_sha256 = sha256(b"complete set grant").to_hex();
    new.assessment.candidate_plan_id = record.plan.plan_id.clone();
    new.assessment.candidate_plan_sha256 = record.plan.plan_sha256.clone();
    new.replacement_authority = sha256(&encode_authority_plan(&record)).as_bytes().to_vec();
    new.additional_sources = set.additional_sources.clone();
    for source in &mut new.additional_sources {
        source.assessment.candidate_plan_id = record.plan.plan_id.clone();
        source.assessment.candidate_plan_sha256 = record.plan.plan_sha256.clone();
        source.replacement_authority = new.replacement_authority.clone();
    }
    new.budget = Some(budget::next(&fixture.root).unwrap());
    persist(
        &path(&fixture.root, &record.plan.plan_id, "proof"),
        &new.encode(),
    )
    .unwrap();
    Proof::decode(&new.encode()).unwrap();
    reserve_sources(&fixture.root, &new).unwrap();
    correlate(&fixture.root, &record, "JOB-COMPLETE-SET").unwrap();
    validate_reservations(&fixture.root, &new, "JOB-COMPLETE-SET").unwrap();
    for proof in [&set, &new] {
        assert!(validate_time(proof, proof.issued_at - 1).is_err());
        validate_time(proof, proof.issued_at).unwrap();
        validate_time(proof, proof.expires_at - 1).unwrap();
        assert!(validate_time(proof, proof.expires_at).is_err());
    }
    for source in set.sources() {
        assert_eq!(
            read(&path(&fixture.root, &source.assessment.job_id, "reserved")).unwrap(),
            set.encode()
        );
    }
}
