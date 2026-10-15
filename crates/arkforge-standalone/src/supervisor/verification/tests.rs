use super::*;
use arkforge_ipc::messages::PublicStep;

fn source() -> (
    AuthorityPlanRecord,
    JobSummary,
    ActiveTargetLineage,
    Vec<ActionReceiptSummary>,
) {
    let effects = TARGETS
        .iter()
        .enumerate()
        .map(|(i, target)| Effect {
            kind: "writePartition".into(),
            target: (*target).into(),
            range_start: (i as u64 + 1) * 4096,
            range_length: 4096,
            content_sha256: "a".repeat(64),
        })
        .collect::<Vec<_>>();
    let steps = effects
        .iter()
        .enumerate()
        .map(|(i, effect)| PublicStep {
            step_id: format!("STEP-{i}"),
            kind: "writeTarget".into(),
            semantic_target: format!("partition:{}", effect.target),
            content_sha256: effect.content_sha256.clone(),
            ..Default::default()
        })
        .collect::<Vec<_>>();
    let receipts = steps
        .iter()
        .map(|step| ActionReceiptSummary {
            job_id: "JOB-TEST".into(),
            plan_id: "PLAN-TEST".into(),
            step_id: step.step_id.clone(),
            attempt_id: "ATTEMPT-TEST".into(),
            permit_id: "PERMIT-TEST".into(),
            disposition: "semanticSuccess".into(),
            evidence_sha256: vec![1; 32],
            ..Default::default()
        })
        .collect::<Vec<_>>();
    let record = AuthorityPlanRecord {
        plan: ExecutablePlan {
            plan_id: "PLAN-TEST".into(),
            plan_sha256: "aa".repeat(32),
            public_steps: steps,
            persistent_effects: effects,
            authority_support_campaign: "campaign".into(),
            mechanics_maturity_campaign: "campaign".into(),
            ..Default::default()
        },
        binding_id: "BIND-TEST".into(),
        stable_identity_sha256: vec![1; 32],
        device_id: "OBS-TEST".into(),
        profile_id: "org.openharmony.dayu200@1.0.0".into(),
        supersedes_job_id: String::new(),
        topology_sha256: "bb".repeat(32),
        toolchain_id: "TOOL-TEST".into(),
    };
    let job = JobSummary {
        job_id: "JOB-TEST".into(),
        plan_id: "PLAN-TEST".into(),
        plan_sha256: vec![0xaa; 32],
        state: "succeeded".into(),
        terminal: true,
        completed_steps: 9,
        total_steps: 9,
        ..Default::default()
    };
    let lineage = ActiveTargetLineage {
        plan_id: "PLAN-TEST".into(),
        current_device_id: "OBS-TEST".into(),
        current_stable_identity_sha256: vec![1; 32],
        topology_sha256: "bb".repeat(32),
        revision: 1,
    };
    (record, job, lineage, receipts)
}

#[test]
fn only_complete_correlated_source_writes_are_eligible() {
    let (record, job, lineage, receipts) = source();
    let validate =
        |record: &AuthorityPlanRecord, job: &JobSummary, receipts: &[ActionReceiptSummary]| {
            validate_source(
                record,
                job,
                &lineage,
                &receipts.iter().collect::<Vec<_>>(),
                "campaign",
            )
        };
    validate(&record, &job, &receipts).unwrap();
    for state in ["outcomeUnknown", "confirmedFailed", "running"] {
        let mut changed = job.clone();
        changed.state = state.into();
        assert!(validate(&record, &changed, &receipts).is_err());
    }
    let mut changed = job.clone();
    changed.plan_sha256[0] ^= 1;
    assert!(validate(&record, &changed, &receipts).is_err());
    let mut changed = record.clone();
    changed.plan.persistent_effects[0].target = "system;reboot".into();
    assert!(validate(&changed, &job, &receipts).is_err());
    let mut changed = record.clone();
    changed.plan.persistent_effects[0].content_sha256 = "b".repeat(64);
    assert!(validate(&changed, &job, &receipts).is_err());
    let mut changed = record.clone();
    changed.plan.persistent_effects.remove(0);
    assert!(validate(&changed, &job, &receipts).is_err());
    let mut changed = record.clone();
    changed.plan.authority_support_campaign = "other".into();
    assert!(validate(&changed, &job, &receipts).is_err());
    assert!(validate(&record, &job, &receipts[1..]).is_err());
    let mut changed = receipts.clone();
    changed.push(receipts[0].clone());
    assert!(validate(&record, &job, &changed).is_err());
    let mut changed = receipts.clone();
    changed[0].job_id = "JOB-OTHER".into();
    assert!(validate(&record, &job, &changed).is_err());
}

#[test]
fn partial_is_not_success_and_report_wire_round_trips() {
    let mut result = FlashVerificationResult {
        document: "{}".into(),
        record_id: "VERIFY-TEST".into(),
        verified: 8,
        skipped: 1,
        failed: 0,
    };
    assert_eq!(result.exit_code(), 9);
    assert_eq!(
        FlashVerificationResult::decode(&result.encode()).unwrap(),
        result
    );
    result.skipped = 0;
    result.failed = 1;
    assert_eq!(result.exit_code(), 7);
    result.failed = 0;
    result.verified = 9;
    assert_eq!(result.exit_code(), 0);
    result.verified = 8;
    assert!(FlashVerificationResult::decode(&result.encode()).is_err());
}

#[test]
fn normal_verification_rejects_identity_and_probe_drift() {
    let (_, _, mut lineage, _) = source();
    let observation = DeviceObservationView {
        observation_id: lineage.current_device_id.clone(),
        mode: "hdc-normal".into(),
        topology_sha256: lineage.topology_sha256.clone(),
        descriptor_sha256: "a".repeat(64),
        serial_sha256: "b".repeat(64),
        identity_strength: "serialAndTopology".into(),
        observed_at_epoch_ms: 1,
        malformed_descriptor: false,
        protocol_identity: vec![],
        serial_evidence_kind: "usbDescriptor".into(),
    };
    let probe = DeviceProbeView {
        observation: observation.clone(),
        protocol_facts: vec![],
        profile_id: "org.openharmony.dayu200@1.0.0".into(),
        facts_sha256: "c".repeat(64),
    };
    lineage.current_stable_identity_sha256 = stable_identity_digest(&observation, &probe)
        .as_bytes()
        .to_vec();
    validate_observation(&lineage, &observation, &probe).unwrap();
    let mut fresh = observation.clone();
    fresh.observed_at_epoch_ms += 1000;
    validate_observation(&lineage, &fresh, &probe).unwrap();
    let changes: [fn(&mut DeviceObservationView); 5] = [
        |o| o.mode = "rockusb-loader".into(),
        |o| o.serial_sha256 = "d".repeat(64),
        |o| o.topology_sha256 = "e".repeat(64),
        |o| o.identity_strength = "topologyOnly".into(),
        |o| o.malformed_descriptor = true,
    ];
    for change in changes {
        let mut other = observation.clone();
        change(&mut other);
        assert!(validate_observation(&lineage, &other, &probe).is_err());
        let mut other_probe = probe.clone();
        other_probe.observation = other;
        assert!(validate_observation(&lineage, &observation, &other_probe).is_err());
    }
}

#[test]
fn a_verification_record_cannot_replace_existing_history() {
    let root = std::env::temp_dir().join(format!(
        "arkforge-verification-record-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let name = format!("{}-record.json", now_ms().unwrap());
    persist(&root, &name, "original").unwrap();
    assert!(persist(&root, &name, "replacement").is_err());
    assert_eq!(
        std::fs::read_to_string(root.join(&name)).unwrap(),
        "original"
    );
    std::fs::remove_file(root.join(name)).unwrap();
    std::fs::remove_dir(root).unwrap();
}

#[test]
fn restoration_verification_requires_one_pinned_original_ramdisk_and_full_receipts() {
    let (mut record, mut job, lineage, mut receipts) = source();
    record
        .plan
        .persistent_effects
        .retain(|effect| effect.target == "ramdisk");
    let effect = &mut record.plan.persistent_effects[0];
    effect.content_sha256 = arkforge_artifact::preboot::RESTORATION_SHA256.into();
    effect.range_length = arkforge_artifact::preboot::MAX_IMAGE_BYTES as u64;
    record
        .plan
        .public_steps
        .retain(|step| step.semantic_target == "partition:ramdisk");
    record.plan.public_steps[0].content_sha256 = effect.content_sha256.clone();
    receipts.retain(|receipt| receipt.step_id == record.plan.public_steps[0].step_id);
    job.total_steps = 1;
    job.completed_steps = 1;
    validate_source(
        &record,
        &job,
        &lineage,
        &receipts.iter().collect::<Vec<_>>(),
        "campaign",
    )
    .unwrap();
    record.plan.persistent_effects[0].content_sha256 = "cc".repeat(32);
    record.plan.public_steps[0].content_sha256 = "cc".repeat(32);
    assert!(
        validate_source(
            &record,
            &job,
            &lineage,
            &receipts.iter().collect::<Vec<_>>(),
            "campaign"
        )
        .is_err()
    );
    let one = FlashVerificationResult {
        document: "{}".into(),
        record_id: "VERIFY-RESTORE".into(),
        verified: 1,
        ..Default::default()
    };
    assert_eq!(FlashVerificationResult::decode(&one.encode()).unwrap(), one);
}
