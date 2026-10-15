use super::*;
use arkforge_ipc::messages::{Effect, PublicStep};

fn observation(mode: &str) -> DeviceObservationView {
    DeviceObservationView {
        observation_id: format!("OBS-{mode}"),
        mode: mode.into(),
        topology_sha256: "a".repeat(64),
        descriptor_sha256: sha256(mode.as_bytes()).to_hex(),
        serial_sha256: "b".repeat(64),
        identity_strength: "serialAndTopology".into(),
        observed_at_epoch_ms: 1,
        malformed_descriptor: false,
        protocol_identity: vec![],
        serial_evidence_kind: "usbDescriptor".into(),
    }
}

fn probe(observation: &DeviceObservationView) -> DeviceProbeView {
    DeviceProbeView {
        observation: observation.clone(),
        protocol_facts: vec![],
        profile_id: "org.openharmony.dayu200".into(),
        facts_sha256: "c".repeat(64),
    }
}

fn source() -> (
    AuthorityPlanRecord,
    JobSummary,
    ActiveTargetLineage,
    Vec<ActionReceiptSummary>,
) {
    let normal = observation("hdc-normal");
    let loader = observation("rockusb-loader");
    let effects = TARGETS
        .iter()
        .enumerate()
        .map(|(i, target)| Effect {
            kind: "writePartition".into(),
            target: (*target).into(),
            range_start: (i as u64 + 1) * 4096,
            range_length: 4096,
            content_sha256: "d".repeat(64),
        })
        .collect::<Vec<_>>();
    let mut steps = ["ensureMode", "probeDevice", "validateLayout"]
        .into_iter()
        .map(|kind| PublicStep {
            kind: kind.into(),
            ..Default::default()
        })
        .collect::<Vec<_>>();
    for effect in &effects {
        for kind in ["writeTarget", "verifyTarget"] {
            steps.push(PublicStep {
                kind: kind.into(),
                semantic_target: format!("partition:{}", effect.target),
                content_sha256: effect.content_sha256.clone(),
                ..Default::default()
            });
        }
    }
    steps.extend(
        ["reboot", "postflightProbe"]
            .into_iter()
            .map(|kind| PublicStep {
                kind: kind.into(),
                ..Default::default()
            }),
    );
    for (i, step) in steps.iter_mut().enumerate() {
        step.step_id = format!("STEP-{:03}", i + 1);
    }
    let receipts = steps
        .iter()
        .map(|s| ActionReceiptSummary {
            job_id: "JOB-SOURCE".into(),
            plan_id: "PLAN-SOURCE".into(),
            step_id: s.step_id.clone(),
            attempt_id: "ATTEMPT-SOURCE".into(),
            permit_id: "PERMIT-SOURCE".into(),
            disposition: "semanticSuccess".into(),
            evidence_sha256: vec![1; 32],
            ..Default::default()
        })
        .collect();
    let record = AuthorityPlanRecord {
        plan: ExecutablePlan {
            plan_id: "PLAN-SOURCE".into(),
            plan_sha256: "dd".repeat(32),
            execution_purpose: "primaryFlash".into(),
            public_steps: steps,
            persistent_effects: effects,
            authority_support_campaign: "campaign".into(),
            mechanics_maturity_campaign: "campaign".into(),
            ..Default::default()
        },
        binding_id: "BIND-SOURCE".into(),
        stable_identity_sha256: stable_identity_digest(&normal, &probe(&normal))
            .as_bytes()
            .to_vec(),
        device_id: normal.observation_id,
        profile_id: "org.openharmony.dayu200@1.0.0".into(),
        supersedes_job_id: String::new(),
        topology_sha256: normal.topology_sha256,
        toolchain_id: "TOOL-SOURCE".into(),
    };
    let job = JobSummary {
        job_id: "JOB-SOURCE".into(),
        plan_id: "PLAN-SOURCE".into(),
        plan_sha256: vec![0xdd; 32],
        state: "succeeded".into(),
        terminal: true,
        completed_steps: 23,
        total_steps: 23,
        ..Default::default()
    };
    let lineage = ActiveTargetLineage {
        plan_id: job.plan_id.clone(),
        current_stable_identity_sha256: stable_identity_digest(&loader, &probe(&loader))
            .as_bytes()
            .to_vec(),
        current_device_id: loader.observation_id,
        topology_sha256: loader.topology_sha256,
        revision: 2,
    };
    (record, job, lineage, receipts)
}

#[test]
fn reloaded_original_and_current_bindings_accept_only_the_same_board() {
    let (record, job, lineage, _) = source();
    let runtime = std::env::temp_dir().join(format!("arkforge-restoration-{}", std::process::id()));
    std::fs::create_dir(&runtime).unwrap();
    persist_authority_plan(&runtime, &record).unwrap();
    persist_job_lineage(&runtime, &job.job_id, &lineage).unwrap();
    let loaded_record = load_authority_plan(&runtime, &job.plan_id).unwrap();
    let loaded_lineage = load_job_lineage(&runtime, &job.job_id).unwrap();
    assert_eq!(loaded_record, record);
    assert_eq!(loaded_lineage, lineage);
    assert!(load_job_lineage(&runtime, "JOB-OTHER").is_err());
    for mode in ["hdc-normal", "rockusb-loader"] {
        let mut fresh = observation(mode);
        fresh.observed_at_epoch_ms += 10_000;
        validate_target(&loaded_record, &loaded_lineage, &fresh, &probe(&fresh)).unwrap();
        let changes: [fn(&mut DeviceObservationView); 7] = [
            |o| o.observation_id = "OBS-OTHER".into(),
            |o| o.serial_sha256 = "e".repeat(64),
            |o| o.topology_sha256 = "e".repeat(64),
            |o| o.descriptor_sha256 = "e".repeat(64),
            |o| o.identity_strength = "topologyOnly".into(),
            |o| o.malformed_descriptor = true,
            |o| o.mode = "rockusb-maskrom".into(),
        ];
        for change in changes {
            let mut other = fresh.clone();
            change(&mut other);
            assert!(
                validate_target(&loaded_record, &loaded_lineage, &other, &probe(&other)).is_err()
            );
            assert!(
                validate_target(&loaded_record, &loaded_lineage, &fresh, &probe(&other)).is_err()
            );
        }
        let mut wrong_probe = probe(&fresh);
        wrong_probe.profile_id = "other@1.0.0".into();
        assert!(validate_target(&loaded_record, &loaded_lineage, &fresh, &wrong_probe).is_err());
    }
    assert_eq!(load_authority_plan(&runtime, &job.plan_id).unwrap(), record);
    assert_eq!(load_job_lineage(&runtime, &job.job_id).unwrap(), lineage);
    std::fs::remove_dir_all(runtime).unwrap();
}

#[test]
fn only_complete_receipts_through_the_final_reboot_allow_restoration() {
    let (record, mut job, lineage, mut receipts) = source();
    let check = |job: &JobSummary, receipts: &[ActionReceiptSummary]| {
        validate_source(
            &record,
            job,
            &lineage,
            &receipts.iter().collect::<Vec<_>>(),
            "campaign",
        )
    };
    check(&job, &receipts).unwrap();
    receipts.pop();
    job.completed_steps -= 1;
    for (state, terminal) in [("rebindWait", false), ("cancelledSafe", true)] {
        job.state = state.into();
        job.terminal = terminal;
        check(&job, &receipts).unwrap();
    }
    for state in [
        "outcomeUnknown",
        "running",
        "confirmedFailed",
        "cancelRequested",
        "succeeded",
    ] {
        let mut other = job.clone();
        other.state = state.into();
        assert!(check(&other, &receipts).is_err());
    }
    assert!(check(&job, &receipts[1..]).is_err());
    let mut other = receipts.clone();
    other[1] = other[0].clone();
    assert!(check(&job, &other).is_err());
    let changes: [fn(&mut ActionReceiptSummary); 4] = [
        |r| r.disposition = "confirmedPartialEffect".into(),
        |r| r.plan_id = "PLAN-OTHER".into(),
        |r| r.job_id = "JOB-OTHER".into(),
        |r| r.failure_classification = "transportLost".into(),
    ];
    for change in changes {
        let mut other = receipts.clone();
        change(&mut other[0]);
        assert!(check(&job, &other).is_err());
    }
    let changes: [fn(&mut AuthorityPlanRecord); 5] = [
        |r| r.plan.authority_support_campaign = "other".into(),
        |r| r.plan.public_steps[21].kind = "writeTarget".into(),
        |r| {
            r.plan.persistent_effects.pop();
        },
        |r| r.plan.public_steps[3].content_sha256 = "f".repeat(64),
        |r| r.plan.plan_sha256 = "f".repeat(64),
    ];
    for change in changes {
        let mut other = record.clone();
        change(&mut other);
        assert!(
            validate_source(
                &other,
                &job,
                &lineage,
                &receipts.iter().collect::<Vec<_>>(),
                "campaign"
            )
            .is_err()
        );
    }
}
