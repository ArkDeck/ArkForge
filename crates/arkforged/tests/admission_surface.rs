//! The controller execution/admission surface, driven end to end.
//!
//! architecture.md 8, 13, 15.3. These exercise the handshake an authority
//! actually performs: a job is created, it asks for a permit, the authority
//! signs one, the intent becomes durable, the authority performs its own
//! control action and reports what it observed, and the job moves on.
//!
//! Every permit here is minted by the test, which is the one place in this
//! repository that may do so: `crates/arkforged` is forbidden from even naming
//! the minting function (architecture.md 8.6), and these tests stand in for the
//! authority that will.

use arkforge_artifact::{dayu200, fixture};
use arkforge_authority_api::authority_side::mint_integrity_tag;
use arkforge_authority_api::{
    ControllerPairingSecret, CurrentFacts, EffectSetCompleteness, PairingEpoch, PermitIntegrityTag,
    PossibleEffectSet, StepPermit,
};
use arkforge_core::digest::sha256;
use arkforge_core::effect::ByteRange;
use arkforge_core::identity::{
    HostPlatform, MaturityKey, MaturityState, ToolchainIdentity, ToolchainKind, Version,
};
use arkforge_core::ids::{
    ActionId, AttemptId, ControllerSessionId, JobId, OpaqueId, PartitionId, PermitId, PlanId,
    StepId,
};
use arkforge_core::plan::{ExecutionPurpose, FlashPlanEnvelope, PlanMaterialization};
use arkforge_core::profile::{self, DeviceProfile};
use arkforge_core::projection::StoredProviderPlan;
use arkforge_core::{
    AuthorityBindingRef, AuthorityNamespace, AuthoritySupportBinding, AuthoritySupportState,
    PersistentEffect, Sha256Digest,
};
use arkforge_engine::JobState;
use arkforge_engine::durable::DurableJournal;
use arkforge_engine::journal::JournalRecordKind;
use arkforge_engine::recovery::fact as journal_fact;
use arkforge_engine::superseding::{EffectObservation, ReconcileVerdict};
use arkforge_ipc::messages::{
    JobEventKind, KeyValue, ManagedControlAction, SubmitManagedControlReceiptRequest,
};
use arkforge_provider::rockchip::RockchipProvider;
use arkforge_provider::{
    FlashIntent, FlashProvider, MaterializeRequest, MaturityRegistry, ProbeContext,
};
use arkforge_transport::replay::TranscriptTransport;
use arkforge_transport::{DeviceObservation, DeviceTransport, TypedDiscoveryFilter, transcript};
use arkforged::jobs::{
    AdmissionFacts, CancellationDisposition, JobRegistry, JobStop, canonical_facts_digest,
};
use std::path::PathBuf;

const PROFILE_SOURCE: &str = include_str!("../../../profiles/dayu200.yaml");
const CAMPAIGN: &str = include_str!("../../../transcripts/dayu200-gj4-ecamp-96effff15.yaml");
const SECRET: &[u8] = b"an-admission-surface-test-pairing-secret";
const EPOCH: PairingEpoch = PairingEpoch(4);
const NOW: u64 = 1_754_380_800_000;

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "arkforge-admission-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        TempRoot(path)
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_recovery_prefix(
    root: &std::path::Path,
    job_id: &str,
    total_steps: usize,
    kinds: &[JournalRecordKind],
) {
    let path = root.join(format!("{job_id}.journal"));
    let (mut journal, _) = DurableJournal::open(path).unwrap();
    let oid = |value: &str| OpaqueId::new(value).unwrap();
    journal
        .append(
            JournalRecordKind::JobCreated,
            NOW,
            1,
            oid(job_id),
            vec![
                (oid(journal_fact::JOB_ID), job_id.to_string()),
                (oid(journal_fact::PLAN_ID), "PLAN-RECOVERY".to_string()),
                (oid("planDigest"), sha256(b"recovery plan").to_hex()),
                (oid("totalSteps"), total_steps.to_string()),
                (oid("controllerSessionId"), "SESSION-RECOVERY".to_string()),
            ],
        )
        .unwrap();

    for (index, kind) in kinds.iter().enumerate() {
        let mut facts = vec![
            (oid(journal_fact::JOB_ID), job_id.to_string()),
            (oid(journal_fact::STEP_ID), "STEP-1".to_string()),
            (oid(journal_fact::PERMIT_ID), "PERMIT-1".to_string()),
        ];
        if matches!(
            kind,
            JournalRecordKind::SemanticReceiptRecorded
                | JournalRecordKind::PermitConsumed
                | JournalRecordKind::StepCheckpointed
        ) {
            facts.push((
                oid(journal_fact::RECEIPT_DIGEST),
                sha256(b"recovery receipt").to_hex(),
            ));
        }
        journal
            .append(*kind, NOW + index as u64 + 1, 1, oid("STEP-1"), facts)
            .unwrap();
    }
}

struct Fixture {
    envelope: FlashPlanEnvelope,
    private_plan: StoredProviderPlan,
    profile: DeviceProfile,
    observations: Vec<DeviceObservation>,
}

/// Materializes a real plan from the real fixture archive.
///
/// The maturity is a test double: publishing `ProductionVerified` for real
/// requires a real DAYU200 pass (architecture.md 22 AF-V2). What is under test
/// here is the admission handshake, not the gate — the gate has its own tests.
fn plan_fixture() -> Fixture {
    let profile = profile::load(PROFILE_SOURCE).unwrap();
    let archive = fixture::dayu200_archive();
    let manifest = dayu200::inspect(archive.as_slice()).unwrap();
    let provider = RockchipProvider::new();

    let parsed = transcript::parse(CAMPAIGN).unwrap();
    let transport = TranscriptTransport::new(parsed);
    let observations = transport
        .discover(&TypedDiscoveryFilter::default(), NOW)
        .unwrap();
    let observation = observations
        .iter()
        .find(|observation| observation.mode.as_str() == "hdc-normal")
        .expect("the campaign transcript observes a normal-mode device");
    let probe = provider
        .probe(&ProbeContext {
            transport: &transport,
            observation,
            profile: &profile,
        })
        .unwrap();

    let toolchain = ToolchainIdentity {
        id: OpaqueId::new("arkforged-native-rockusb").unwrap(),
        kind: ToolchainKind::NativeProtocol,
        version: Version::new(0, 1, 0),
        backend_digest: sha256(b"native arkforged build"),
        upstream_ref: None,
    };
    let host = HostPlatform::new("macos", "aarch64").unwrap();
    let driver = sha256(b"driver facts");
    let evidence = sha256(b"AD-003,AD-005,AD-006");

    let mut registry = MaturityRegistry::new();
    registry.publish(
        &MaturityKey {
            provider: provider.identity().clone(),
            profile: profile.identity().unwrap(),
            artifact_format: provider.descriptor().artifact_formats[0].clone(),
            toolchain: toolchain.clone(),
            host_platform: host.clone(),
            driver_facts_digest: driver,
            evidence_set_digest: evidence,
        },
        MaturityState::ProductionVerified,
    );

    let request = MaterializeRequest {
        plan_id: PlanId::new("PLAN-ADMISSION").unwrap(),
        execution_purpose: ExecutionPurpose::PrimaryFlash,
        intent: FlashIntent::FullRestore,
        artifact: &manifest,
        artifact_id: OpaqueId::new("ART-ADMISSION").unwrap(),
        profile: &profile,
        probe: &probe,
        authority_binding: binding(),
        authority_support: AuthoritySupportBinding {
            key_digest: sha256(b"test authority support"),
            state: AuthoritySupportState::ProductionVerified,
        },
        toolchain,
        host_platform: host,
        driver_facts_digest: driver,
        evidence_set_digest: evidence,
        created_at_epoch_ms: NOW,
        plan_lifetime_ms: 3_600_000,
    };
    let materialized = provider
        .materialize_with_private_plan(&request, &registry)
        .unwrap();
    let PlanMaterialization::Executable(envelope) = materialized.materialization else {
        panic!("the test registry should permit an executable plan");
    };
    Fixture {
        envelope: *envelope,
        private_plan: materialized.private_plan.unwrap(),
        profile,
        observations,
    }
}

fn binding() -> AuthorityBindingRef {
    AuthorityBindingRef {
        authority_namespace: AuthorityNamespace::new("test-authority").unwrap(),
        binding_id: OpaqueId::new("BINDING-1").unwrap(),
        binding_revision: 1,
        stable_identity_digest: sha256(b"stable identity"),
    }
}

fn secret() -> ControllerPairingSecret {
    ControllerPairingSecret::new(EPOCH, SECRET.to_vec())
}

fn admission_facts(fixture: &Fixture, mode: &str, at: u64) -> AdmissionFacts {
    let observed_mode = match mode {
        "rockusb-loader" => "updater",
        other => other,
    };
    let mut observation = fixture
        .observations
        .iter()
        .find(|observation| observation.mode.as_str() == observed_mode)
        .or_else(|| fixture.observations.first())
        .unwrap_or_else(|| panic!("fixture has no observations"))
        .clone();
    if observation.mode.as_str() != mode {
        observation.mode = arkforge_core::DeviceMode::new(mode).unwrap();
    }
    observation.observed_at_epoch_ms = at;
    AdmissionFacts {
        observation,
        transport_session_digest: sha256(format!("session-{mode}").as_bytes()),
        provider_facts_digest: sha256(b"provider-facts"),
        toolchain_facts_digest: sha256(b"toolchain-facts"),
        artifact_facts_digest: sha256(b"artifact-facts"),
    }
}

fn current_facts(
    snapshot: &arkforge_ipc::messages::StepAdmissionSnapshot,
    now: u64,
) -> CurrentFacts {
    let digest = |bytes: &[u8]| {
        let mut array = [0u8; 32];
        array.copy_from_slice(bytes);
        Sha256Digest::from_bytes(array)
    };
    CurrentFacts {
        now_epoch_ms: now,
        device_facts_digest: digest(&snapshot.admitted_device_facts_sha256),
        transport_session_digest: Some(digest(&snapshot.transport_session_sha256)),
        saw_detach_since_snapshot: false,
        provider_facts_digest: sha256(b"provider-facts"),
        toolchain_facts_digest: sha256(b"toolchain-facts"),
        artifact_facts_digest: sha256(b"artifact-facts"),
    }
}

/// Mints a permit for the admission the daemon just published.
///
/// This is the authority's half. It reads the snapshot the daemon produced and
/// signs exactly the action it names, which is the whole contract.
fn mint(
    snapshot: &arkforge_ipc::messages::StepAdmissionSnapshot,
    permit_id: &str,
    expires_at: u64,
) -> (StepPermit, Vec<u8>, u64) {
    let digest = |bytes: &[u8]| {
        let mut array = [0u8; 32];
        array.copy_from_slice(bytes);
        Sha256Digest::from_bytes(array)
    };
    let mut permit = StepPermit {
        permit_id: PermitId::new(permit_id).unwrap(),
        authority_namespace: AuthorityNamespace::new("test-authority").unwrap(),
        controller_session_id: ControllerSessionId::new("SESSION-1").unwrap(),
        job_id: JobId::new(&snapshot.job_id).unwrap(),
        plan_id: PlanId::new(&snapshot.plan_id).unwrap(),
        plan_digest: digest(&snapshot.plan_sha256),
        step_id: StepId::new(&snapshot.step_id).unwrap(),
        attempt_id: AttemptId::new(&snapshot.attempt_id).unwrap(),
        public_step_digest: digest(&snapshot.public_step_sha256),
        private_action_digest: digest(&snapshot.private_action_sha256),
        effect_set_digest: digest(&snapshot.effect_set_sha256),
        authority_binding: binding(),
        admitted_device_facts_digest: digest(&snapshot.admitted_device_facts_sha256),
        issued_at_epoch_ms: snapshot.observed_at_epoch_ms,
        expires_at_epoch_ms: expires_at,
        single_use: true,
        integrity_tag: PermitIntegrityTag {
            epoch: EPOCH,
            tag: sha256(b""),
        },
    };
    permit.integrity_tag = mint_integrity_tag(&permit, &secret()).unwrap();
    let tag = permit.integrity_tag.tag.as_bytes().to_vec();
    (permit, tag, EPOCH.0)
}

/// The snapshot the job is currently waiting on.
fn pending_snapshot(
    registry: &mut JobRegistry,
    fixture: &Fixture,
    job_id: &str,
    at: u64,
) -> arkforge_ipc::messages::StepAdmissionSnapshot {
    if registry
        .job(job_id)
        .is_some_and(|job| job.needs_admission())
    {
        let mode = registry
            .job(job_id)
            .and_then(|job| job.expected_mode(&fixture.envelope))
            .map(|mode| mode.as_str().to_string())
            .unwrap_or_else(|| "hdc-normal".to_string());
        registry
            .request_next_admission(
                job_id,
                &fixture.envelope,
                &fixture.private_plan,
                &admission_facts(fixture, &mode, at),
                at,
            )
            .unwrap();
    }
    registry
        .job(job_id)
        .unwrap()
        .events_from(0)
        .into_iter()
        .rev()
        .find(|event| event.kind == JobEventKind::StepAdmissionRequested)
        .and_then(|event| event.admission)
        .expect("the job published an admission request")
}

/// The whole handshake for the plan's first step, which is a control action.
#[test]
fn a_job_walks_from_admission_to_a_durable_intent_to_a_control_receipt() {
    let root = TempRoot::new("walk");
    let fixture = plan_fixture();
    let mut registry = JobRegistry::new(&root.0);

    let job_id = registry
        .start(
            &fixture.envelope,
            &fixture.private_plan,
            ControllerSessionId::new("SESSION-1").unwrap(),
            &admission_facts(&fixture, "hdc-normal", NOW),
            NOW,
        )
        .unwrap();

    // The job publishes what it needs and stops. Nothing has been dispatched.
    let job = registry.job(&job_id).unwrap();
    assert_eq!(job.state(), JobState::Preflight);
    let kinds: Vec<JobEventKind> = job.events_from(0).iter().map(|event| event.kind).collect();
    assert_eq!(
        kinds,
        vec![
            JobEventKind::StateChanged,
            JobEventKind::StepAdmissionRequested
        ]
    );

    let snapshot = pending_snapshot(&mut registry, &fixture, &job_id, NOW);
    assert_eq!(snapshot.job_id, job_id);
    assert!(snapshot.is_fresh_at(NOW));

    // The authority signs exactly the action the snapshot names.
    let (permit, tag, epoch) = mint(&snapshot, "PERMIT-1", NOW + 60_000);
    registry
        .submit_permit(
            &job_id,
            &snapshot.request_id,
            Some((permit, tag, epoch)),
            "",
            &secret(),
            &fixture.envelope,
            &fixture.private_plan,
            &fixture.profile,
            Some(current_facts(&snapshot, NOW + 10)),
            NOW + 10,
        )
        .unwrap();

    // The first step is `EnsureMode`, which the authority performs. The job
    // asks for it rather than reaching for a device itself.
    let job = registry.job(&job_id).unwrap();
    assert_eq!(job.state(), JobState::Dispatching);
    let control = job
        .events_from(0)
        .into_iter()
        .rev()
        .find(|event| event.kind == JobEventKind::ManagedControlRequested)
        .and_then(|event| event.control_request)
        .expect("a control action was requested");
    assert_eq!(control.action, ManagedControlAction::EnterUpdater);
    assert_eq!(control.permit_id, "PERMIT-1");

    // The authority reports what its own channel observed. The evidence
    // digest is defined, not opaque: the canonical digest of the receipt's
    // own facts, which the daemon recomputes before accepting.
    let facts = vec![KeyValue {
        key: "mode".into(),
        value: "updater".into(),
    }];
    registry
        .submit_control_receipt(
            &SubmitManagedControlReceiptRequest {
                job_id: job_id.clone(),
                request_id: control.request_id.clone(),
                action: ManagedControlAction::EnterUpdater,
                accepted: true,
                evidence_sha256: canonical_facts_digest(&facts).as_bytes().to_vec(),
                facts,
                failure_reason: String::new(),
            },
            &fixture.envelope,
            &fixture.private_plan,
            NOW + 20,
        )
        .unwrap();

    let job = registry.job(&job_id).unwrap();
    let kinds: Vec<JobEventKind> = job.events_from(0).iter().map(|event| event.kind).collect();
    assert!(kinds.contains(&JobEventKind::ActionReceipt));
    assert!(kinds.contains(&JobEventKind::StepCheckpointed));

    // The production daemon uses the same typed transaction path as the
    // engine: there is one ordered writer, every durable boundary retains the
    // complete correlation, and receiptDigest identifies the semantic receipt
    // rather than merely repeating its transport-evidence digest.
    let receipt = job
        .events_from(0)
        .into_iter()
        .find_map(|event| event.receipt)
        .expect("the control step published its receipt");
    assert_eq!(receipt.attempt_id, snapshot.attempt_id);
    let semantic_digest = receipt.receipt_digest().unwrap();
    let (journal, _) = DurableJournal::open(root.0.join(format!("{job_id}.journal"))).unwrap();
    let records = journal.journal().records();
    let transactional = records
        .iter()
        .filter(|record| {
            matches!(
                record.kind,
                JournalRecordKind::StepPermitAccepted
                    | JournalRecordKind::StepIntentRecorded
                    | JournalRecordKind::PermitConsuming
                    | JournalRecordKind::ExternalDispatchStarted
                    | JournalRecordKind::TransportEvidenceRecorded
                    | JournalRecordKind::SemanticReceiptRecorded
                    | JournalRecordKind::PermitConsumed
                    | JournalRecordKind::StepCheckpointed
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(transactional.len(), 8);
    assert_eq!(
        transactional
            .iter()
            .map(|record| record.kind)
            .collect::<Vec<_>>(),
        vec![
            JournalRecordKind::StepPermitAccepted,
            JournalRecordKind::StepIntentRecorded,
            JournalRecordKind::PermitConsuming,
            JournalRecordKind::ExternalDispatchStarted,
            JournalRecordKind::TransportEvidenceRecorded,
            JournalRecordKind::SemanticReceiptRecorded,
            JournalRecordKind::PermitConsumed,
            JournalRecordKind::StepCheckpointed,
        ]
    );
    for record in &transactional {
        for (key, expected) in [
            (journal_fact::JOB_ID, job_id.as_str()),
            (journal_fact::PLAN_ID, fixture.envelope.plan_id.as_str()),
            (journal_fact::STEP_ID, snapshot.step_id.as_str()),
            (journal_fact::ATTEMPT_ID, snapshot.attempt_id.as_str()),
            (journal_fact::PERMIT_ID, "PERMIT-1"),
        ] {
            assert!(
                record
                    .facts
                    .iter()
                    .any(|(found, value)| { found.as_str() == key && value == expected }),
                "{} omitted {key}",
                record.kind.as_str()
            );
        }
    }
    let recorded_receipt = transactional
        .iter()
        .find(|record| record.kind == JournalRecordKind::SemanticReceiptRecorded)
        .and_then(|record| {
            record
                .facts
                .iter()
                .find(|(key, _)| key.as_str() == journal_fact::RECEIPT_DIGEST)
        })
        .map(|(_, value)| value.as_str())
        .unwrap();
    assert_eq!(recorded_receipt, semantic_digest.to_hex());
    let evidence_hex = receipt
        .evidence_sha256
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_ne!(recorded_receipt, evidence_hex);

    // Step two is a device probe, which this build cannot dispatch. The job
    // stops there having said so, rather than pretending it ran.
    let snapshot = pending_snapshot(&mut registry, &fixture, &job_id, NOW);
    assert_ne!(
        snapshot.step_id, control.step_id,
        "the job moved to the next step"
    );
    let (permit, tag, epoch) = mint(&snapshot, "PERMIT-2", NOW + 60_000);
    registry
        .submit_permit(
            &job_id,
            &snapshot.request_id,
            Some((permit, tag, epoch)),
            "",
            &secret(),
            &fixture.envelope,
            &fixture.private_plan,
            &fixture.profile,
            Some(current_facts(&snapshot, NOW + 30)),
            NOW + 30,
        )
        .unwrap();
    // Step two is this daemon's own to dispatch, so the job now holds work for
    // the dispatcher rather than stopping. Until something takes it and reports
    // back, whether the device changed is unknown — which is exactly what the
    // durable intent already records.
    let job = registry.job(&job_id).unwrap();
    assert_eq!(job.state(), JobState::Dispatching);
    assert!(job.stopped().is_none());

    let work = registry.take_pending_dispatch().expect("work is waiting");
    assert_eq!(work.job_id, job_id);
    assert!(!work.actions.is_empty());
    // Taken means taken: a second dispatcher must not get the same action.
    assert!(registry.take_pending_dispatch().is_none());
}

/// A refusal is an answer, and it is a safe one: no intent was recorded, so the
/// job cancels rather than becoming unknown.
#[test]
fn an_authority_refusal_cancels_safely() {
    let root = TempRoot::new("refusal");
    let fixture = plan_fixture();
    let mut registry = JobRegistry::new(&root.0);
    let job_id = registry
        .start(
            &fixture.envelope,
            &fixture.private_plan,
            ControllerSessionId::new("SESSION-1").unwrap(),
            &admission_facts(&fixture, "hdc-normal", NOW),
            NOW,
        )
        .unwrap();
    let snapshot = pending_snapshot(&mut registry, &fixture, &job_id, NOW);

    registry
        .submit_permit(
            &job_id,
            &snapshot.request_id,
            None,
            "the operator declined the destructive confirmation",
            &secret(),
            &fixture.envelope,
            &fixture.private_plan,
            &fixture.profile,
            Some(current_facts(&snapshot, NOW + 10)),
            NOW + 10,
        )
        .unwrap();

    let job = registry.job(&job_id).unwrap();
    assert_eq!(job.state(), JobState::CancelledSafe);
    assert!(matches!(
        job.stopped(),
        Some(JobStop::RefusedByAuthority { .. })
    ));
    let terminal_sequence = job.last_sequence();
    let journal_path = root.0.join(format!("{job_id}.journal"));
    let terminal_journal_len = std::fs::metadata(&journal_path).unwrap().len();
    drop(registry);

    for _ in 0..2 {
        let reopened = JobRegistry::open(&root.0).unwrap();
        let recovered = reopened.job(&job_id).expect("refused job survives restart");
        assert_eq!(recovered.state(), JobState::CancelledSafe);
        assert_eq!(recovered.last_sequence(), terminal_sequence);
        assert_eq!(
            std::fs::metadata(&journal_path).unwrap().len(),
            terminal_journal_len
        );
    }
}

#[test]
fn restart_rehydrates_a_pre_intent_job_as_cancelled_safe() {
    let root = TempRoot::new("restart-safe");
    let fixture = plan_fixture();
    let job_id = {
        let mut registry = JobRegistry::new(&root.0);
        registry
            .start(
                &fixture.envelope,
                &fixture.private_plan,
                ControllerSessionId::new("SESSION-1").unwrap(),
                &admission_facts(&fixture, "hdc-normal", NOW),
                NOW,
            )
            .unwrap()
    };

    let registry = JobRegistry::open(&root.0).unwrap();
    let job = registry.job(&job_id).expect("job survives restart");
    assert_eq!(job.state(), JobState::CancelledSafe);
    assert_eq!(
        job.events_from(0).last().unwrap().job_state,
        "cancelledSafe"
    );
}

#[test]
fn restart_rehydrates_an_unsettled_intent_as_outcome_unknown_without_replay() {
    let root = TempRoot::new("restart-unknown");
    let fixture = plan_fixture();
    let job_id = {
        let mut registry = JobRegistry::new(&root.0);
        let job_id = registry
            .start(
                &fixture.envelope,
                &fixture.private_plan,
                ControllerSessionId::new("SESSION-1").unwrap(),
                &admission_facts(&fixture, "hdc-normal", NOW),
                NOW,
            )
            .unwrap();
        let snapshot = pending_snapshot(&mut registry, &fixture, &job_id, NOW);
        let (permit, tag, epoch) = mint(&snapshot, "PERMIT-RESTART", NOW + 60_000);
        registry
            .submit_permit(
                &job_id,
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
        job_id
    };

    let mut registry = JobRegistry::open(&root.0).unwrap();
    let job = registry.job(&job_id).expect("job survives restart");
    assert_eq!(job.state(), JobState::OutcomeUnknown);
    let outcome = job
        .events_from(0)
        .last()
        .unwrap()
        .facts
        .iter()
        .find(|fact| fact.key == "outcome")
        .unwrap()
        .value
        .clone();
    assert!(registry.take_pending_dispatch().is_none());
    assert_eq!(outcome, "outcomeUnknown");
}

fn reconcile_fixture() -> (PossibleEffectSet, PersistentEffect) {
    let effect = PersistentEffect::WritePartition {
        partition: PartitionId::new("system").unwrap(),
        range: ByteRange::new(4096, 8192).unwrap(),
        content: sha256(b"expected system image"),
    };
    let mut effects = arkforge_core::EffectSet::read_only();
    effects.persistent.push(effect.clone());
    (
        PossibleEffectSet {
            effects,
            completeness: EffectSetCompleteness::Bounded,
            source_action_ids: vec![ActionId::new("ACT-WRITE-SYSTEM").unwrap()],
        },
        effect,
    )
}

#[test]
fn reconcile_moves_through_the_real_state_and_never_overclaims_partial_scope() {
    use JournalRecordKind::*;

    let root = TempRoot::new("reconcile-state");
    let job_id = "JOB-RECONCILE-001";
    write_recovery_prefix(
        &root.0,
        job_id,
        2,
        &[
            StepPermitAccepted,
            StepIntentRecorded,
            PermitConsuming,
            ExternalDispatchStarted,
        ],
    );
    let mut registry = JobRegistry::open(&root.0).unwrap();
    assert_eq!(
        registry.job(job_id).unwrap().state(),
        JobState::OutcomeUnknown
    );
    let recovered_terminal_sequence = registry.job(job_id).unwrap().last_sequence();
    let (possible, effect) = reconcile_fixture();

    registry
        .begin_reconcile(job_id, possible.digest().unwrap(), "bounded", 1, NOW + 100)
        .unwrap();
    assert_eq!(registry.job(job_id).unwrap().state(), JobState::Reconciling);
    let verdict = registry
        .complete_reconcile(
            job_id,
            &[(effect.clone(), EffectObservation::Indeterminate)],
            vec![("readOnly".into(), "true".into())],
            sha256(b"indeterminate read"),
            false,
            NOW + 101,
        )
        .unwrap();
    assert!(matches!(verdict, ReconcileVerdict::StillUnknown { .. }));
    assert_eq!(
        registry.job(job_id).unwrap().state(),
        JobState::OutcomeUnknown
    );

    // Even seeing the interrupted bytes is not proof that the rest of a
    // multi-step plan ran. The explicit scope gate keeps the job unknown.
    registry
        .begin_reconcile(job_id, possible.digest().unwrap(), "bounded", 1, NOW + 102)
        .unwrap();
    let verdict = registry
        .complete_reconcile(
            job_id,
            &[(effect.clone(), EffectObservation::Present)],
            vec![("readOnly".into(), "true".into())],
            sha256(b"present but partial scope"),
            false,
            NOW + 103,
        )
        .unwrap();
    assert!(matches!(verdict, ReconcileVerdict::StillUnknown { .. }));

    // A future Provider that proves the complete terminal contract can use
    // the declared reconciling -> succeeded edge without any dispatch.
    registry
        .begin_reconcile(job_id, possible.digest().unwrap(), "bounded", 1, NOW + 104)
        .unwrap();
    assert_eq!(
        registry
            .complete_reconcile(
                job_id,
                &[(effect, EffectObservation::Present)],
                vec![("readOnly".into(), "true".into())],
                sha256(b"complete terminal proof"),
                true,
                NOW + 105,
            )
            .unwrap(),
        ReconcileVerdict::Succeeded
    );
    let job = registry.job(job_id).unwrap();
    assert_eq!(job.state(), JobState::Succeeded);
    assert_eq!(job.last_reconcile_assessment().unwrap().0, "succeeded");
    let sequences = job
        .events_from(0)
        .into_iter()
        .map(|event| event.sequence)
        .collect::<Vec<_>>();
    assert!(
        sequences.windows(2).all(|pair| pair[0] < pair[1]),
        "recovered and reconciled event cursors must remain strictly monotonic: {sequences:?}"
    );
    assert!(
        sequences
            .last()
            .is_some_and(|sequence| *sequence > recovered_terminal_sequence),
        "post-restart events must continue after cursor {recovered_terminal_sequence}: {sequences:?}"
    );

    let records = job.journal().records();
    assert!(
        records
            .iter()
            .any(|record| record.kind == PossibleEffectSetRecorded)
    );
    assert!(
        records
            .iter()
            .any(|record| record.kind == ReadOnlyObservationRecorded)
    );
    assert!(
        records
            .iter()
            .any(|record| record.kind == RecoveryAssessmentPublished)
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| record.kind == ExternalDispatchStarted)
            .count(),
        1,
        "reconciliation must not create another external-effect dispatch"
    );
}

#[test]
fn a_restart_during_reconcile_returns_to_unknown_without_replaying_reads_or_writes() {
    use JournalRecordKind::*;

    let root = TempRoot::new("reconcile-restart");
    let job_id = "JOB-RECONCILE-RESTART";
    write_recovery_prefix(
        &root.0,
        job_id,
        1,
        &[StepPermitAccepted, StepIntentRecorded, PermitConsuming],
    );
    let (possible, _) = reconcile_fixture();
    {
        let mut registry = JobRegistry::open(&root.0).unwrap();
        registry
            .begin_reconcile(job_id, possible.digest().unwrap(), "bounded", 1, NOW + 200)
            .unwrap();
        assert_eq!(registry.job(job_id).unwrap().state(), JobState::Reconciling);
    }

    let registry = JobRegistry::open(&root.0).unwrap();
    let job = registry.job(job_id).unwrap();
    assert_eq!(job.state(), JobState::OutcomeUnknown);
    assert!(registry.jobs_needing_admission().is_empty());
    assert_eq!(
        job.journal()
            .records()
            .iter()
            .filter(|record| record.kind == PermitConsuming)
            .count(),
        1
    );
}

#[test]
fn every_durable_restart_prefix_is_classified_without_replaying_dispatch() {
    use JournalRecordKind::*;

    let root = TempRoot::new("restart-prefixes");
    let cases = vec![
        (
            "JOB-RECOVERY-001",
            1,
            vec![StepPermitAccepted],
            JobState::CancelledSafe,
            "cancelledSafe",
        ),
        (
            "JOB-RECOVERY-002",
            1,
            vec![StepPermitAccepted, StepIntentRecorded],
            JobState::OutcomeUnknown,
            "outcomeUnknown",
        ),
        (
            "JOB-RECOVERY-003",
            1,
            vec![StepPermitAccepted, StepIntentRecorded, PermitConsuming],
            JobState::OutcomeUnknown,
            "outcomeUnknown",
        ),
        (
            "JOB-RECOVERY-004",
            1,
            vec![
                StepPermitAccepted,
                StepIntentRecorded,
                PermitConsuming,
                SemanticReceiptRecorded,
            ],
            JobState::OutcomeUnknown,
            "outcomeUnknown",
        ),
        (
            "JOB-RECOVERY-005",
            1,
            vec![
                StepPermitAccepted,
                StepIntentRecorded,
                PermitConsuming,
                SemanticReceiptRecorded,
                PermitConsumed,
            ],
            JobState::OutcomeUnknown,
            "outcomeUnknown",
        ),
        (
            "JOB-RECOVERY-006",
            2,
            vec![
                StepPermitAccepted,
                StepIntentRecorded,
                PermitConsuming,
                SemanticReceiptRecorded,
                PermitConsumed,
                StepCheckpointed,
            ],
            JobState::OutcomeUnknown,
            "outcomeUnknown",
        ),
        (
            "JOB-RECOVERY-007",
            1,
            vec![
                StepPermitAccepted,
                StepIntentRecorded,
                PermitConsuming,
                SemanticReceiptRecorded,
                PermitConsumed,
                StepCheckpointed,
            ],
            JobState::OutcomeUnknown,
            "outcomeUnknown",
        ),
    ];

    for (job_id, total_steps, records, expected_state, expected_outcome) in cases {
        let jobs_root = root.0.join(job_id);
        std::fs::create_dir_all(&jobs_root).unwrap();
        write_recovery_prefix(&jobs_root, job_id, total_steps, &records);

        let registry = JobRegistry::open(&jobs_root).unwrap();
        let job = registry.job(job_id).expect("durable prefix is rehydrated");
        assert_eq!(job.state(), expected_state, "{job_id}");
        assert!(registry.jobs_needing_admission().is_empty(), "{job_id}");
        let events = job.events_from(0);
        let event = events.last().unwrap();
        assert_eq!(event.kind, JobEventKind::OutcomeClassified, "{job_id}");
        assert_eq!(
            event
                .facts
                .iter()
                .find(|fact| fact.key == "outcome")
                .map(|fact| fact.value.as_str()),
            Some(expected_outcome),
            "{job_id}"
        );

        let journal_path = jobs_root.join(format!("{job_id}.journal"));
        let classified_len = std::fs::metadata(&journal_path).unwrap().len();
        drop(registry);
        let reopened = JobRegistry::open(&jobs_root).unwrap();
        assert_eq!(reopened.job(job_id).unwrap().state(), expected_state);
        assert_eq!(
            std::fs::metadata(journal_path).unwrap().len(),
            classified_len,
            "a second restart must not append another classification for {job_id}"
        );
    }
}

/// architecture.md 8.3. A permit signed against facts that are no longer in
/// front of the device is not accepted late; a fresh snapshot is published and
/// the authority signs again.
#[test]
fn a_permit_that_arrives_after_its_snapshot_expired_is_refused_and_re_asked() {
    let root = TempRoot::new("stale");
    let fixture = plan_fixture();
    let mut registry = JobRegistry::new(&root.0);
    let job_id = registry
        .start(
            &fixture.envelope,
            &fixture.private_plan,
            ControllerSessionId::new("SESSION-1").unwrap(),
            &admission_facts(&fixture, "hdc-normal", NOW),
            NOW,
        )
        .unwrap();
    let snapshot = pending_snapshot(&mut registry, &fixture, &job_id, NOW);
    let (permit, tag, epoch) = mint(&snapshot, "PERMIT-LATE", NOW + 600_000);

    let late = NOW + arkforged::jobs::SNAPSHOT_LIFETIME_MS + 1;
    let error = registry
        .submit_permit(
            &job_id,
            &snapshot.request_id,
            Some((permit, tag, epoch)),
            "",
            &secret(),
            &fixture.envelope,
            &fixture.private_plan,
            &fixture.profile,
            Some(current_facts(&snapshot, late)),
            late,
        )
        .unwrap_err();
    assert_eq!(error.code(), "SNAPSHOT_EXPIRED");

    // A fresh snapshot was published, so the authority has something to sign.
    let fresh = pending_snapshot(&mut registry, &fixture, &job_id, late);
    assert_ne!(fresh.observed_at_epoch_ms, snapshot.observed_at_epoch_ms);
    assert!(fresh.is_fresh_at(late));
}

/// The permit must authorize the action about to run, not merely be valid.
#[test]
fn a_permit_for_another_action_is_rejected() {
    let root = TempRoot::new("wrong-action");
    let fixture = plan_fixture();
    let mut registry = JobRegistry::new(&root.0);
    let job_id = registry
        .start(
            &fixture.envelope,
            &fixture.private_plan,
            ControllerSessionId::new("SESSION-1").unwrap(),
            &admission_facts(&fixture, "hdc-normal", NOW),
            NOW,
        )
        .unwrap();
    let snapshot = pending_snapshot(&mut registry, &fixture, &job_id, NOW);

    let mut wrong = snapshot.clone();
    wrong.private_action_sha256 = sha256(b"some other action").as_bytes().to_vec();
    let (permit, tag, epoch) = mint(&wrong, "PERMIT-WRONG", NOW + 60_000);

    let error = registry
        .submit_permit(
            &job_id,
            &snapshot.request_id,
            Some((permit, tag, epoch)),
            "",
            &secret(),
            &fixture.envelope,
            &fixture.private_plan,
            &fixture.profile,
            Some(current_facts(&snapshot, NOW + 10)),
            NOW + 10,
        )
        .unwrap_err();
    assert_eq!(error.code(), "PERMIT_REJECTED");
    // Nothing was recorded: the job is still waiting for the same admission.
    assert_eq!(
        pending_snapshot(&mut registry, &fixture, &job_id, NOW).request_id,
        snapshot.request_id
    );
}

/// A submission that answers a different admission than the one outstanding is
/// refused. Otherwise one job's permit could answer another job's question.
#[test]
fn a_submission_answering_the_wrong_request_is_refused() {
    let root = TempRoot::new("wrong-request");
    let fixture = plan_fixture();
    let mut registry = JobRegistry::new(&root.0);
    let job_id = registry
        .start(
            &fixture.envelope,
            &fixture.private_plan,
            ControllerSessionId::new("SESSION-1").unwrap(),
            &admission_facts(&fixture, "hdc-normal", NOW),
            NOW,
        )
        .unwrap();
    let snapshot = pending_snapshot(&mut registry, &fixture, &job_id, NOW);
    let (permit, tag, epoch) = mint(&snapshot, "PERMIT-1", NOW + 60_000);

    let error = registry
        .submit_permit(
            &job_id,
            "REQ-FROM-SOMEWHERE-ELSE",
            Some((permit, tag, epoch)),
            "",
            &secret(),
            &fixture.envelope,
            &fixture.private_plan,
            &fixture.profile,
            Some(current_facts(&snapshot, NOW + 10)),
            NOW + 10,
        )
        .unwrap_err();
    assert_eq!(error.code(), "WRONG_REQUEST");
}

/// architecture.md 9.2. A receipt carrying a connect key, a path, an argv or a
/// lifecycle action is the leak the typed port exists to prevent, so the whole
/// receipt is refused rather than the field dropped.
#[test]
fn a_control_receipt_carrying_a_forbidden_fact_is_refused_whole() {
    let root = TempRoot::new("forbidden");
    let fixture = plan_fixture();
    let mut registry = JobRegistry::new(&root.0);
    let job_id = registry
        .start(
            &fixture.envelope,
            &fixture.private_plan,
            ControllerSessionId::new("SESSION-1").unwrap(),
            &admission_facts(&fixture, "hdc-normal", NOW),
            NOW,
        )
        .unwrap();
    let snapshot = pending_snapshot(&mut registry, &fixture, &job_id, NOW);
    let (permit, tag, epoch) = mint(&snapshot, "PERMIT-1", NOW + 60_000);
    registry
        .submit_permit(
            &job_id,
            &snapshot.request_id,
            Some((permit, tag, epoch)),
            "",
            &secret(),
            &fixture.envelope,
            &fixture.private_plan,
            &fixture.profile,
            Some(current_facts(&snapshot, NOW + 10)),
            NOW + 10,
        )
        .unwrap();
    let control = registry
        .job(&job_id)
        .unwrap()
        .events_from(0)
        .into_iter()
        .rev()
        .find(|event| event.kind == JobEventKind::ManagedControlRequested)
        .and_then(|event| event.control_request)
        .unwrap();

    for forbidden in ["connectKey", "hdcExecutablePath", "argv", "shell"] {
        let error = registry
            .submit_control_receipt(
                &SubmitManagedControlReceiptRequest {
                    job_id: job_id.clone(),
                    request_id: control.request_id.clone(),
                    action: ManagedControlAction::EnterUpdater,
                    accepted: true,
                    facts: vec![KeyValue {
                        key: forbidden.into(),
                        value: "something the daemon must never learn".into(),
                    }],
                    evidence_sha256: sha256(b"evidence").as_bytes().to_vec(),
                    failure_reason: String::new(),
                },
                &fixture.envelope,
                &fixture.private_plan,
                NOW + 20,
            )
            .unwrap_err();
        assert_eq!(
            error.code(),
            "RECEIPT_CARRIES_FORBIDDEN_FACTS",
            "{forbidden}"
        );
    }
}

/// A control action the authority could not confirm leaves the outcome unknown,
/// not failed. The device may have changed and simply not been observed.
#[test]
fn an_unconfirmed_control_action_is_unknown_rather_than_failed() {
    let root = TempRoot::new("unconfirmed");
    let fixture = plan_fixture();
    let mut registry = JobRegistry::new(&root.0);
    let job_id = registry
        .start(
            &fixture.envelope,
            &fixture.private_plan,
            ControllerSessionId::new("SESSION-1").unwrap(),
            &admission_facts(&fixture, "hdc-normal", NOW),
            NOW,
        )
        .unwrap();
    let snapshot = pending_snapshot(&mut registry, &fixture, &job_id, NOW);
    let (permit, tag, epoch) = mint(&snapshot, "PERMIT-1", NOW + 60_000);
    registry
        .submit_permit(
            &job_id,
            &snapshot.request_id,
            Some((permit, tag, epoch)),
            "",
            &secret(),
            &fixture.envelope,
            &fixture.private_plan,
            &fixture.profile,
            Some(current_facts(&snapshot, NOW + 10)),
            NOW + 10,
        )
        .unwrap();
    let control = registry
        .job(&job_id)
        .unwrap()
        .events_from(0)
        .into_iter()
        .rev()
        .find(|event| event.kind == JobEventKind::ManagedControlRequested)
        .and_then(|event| event.control_request)
        .unwrap();

    // A refusal made no observation, so it carries no evidence bytes. That is
    // the receipt the authority actually sends; demanding a digest of nothing
    // forced it to invent one, and the invented bytes were then refused —
    // leaving both sides waiting on the other.
    registry
        .submit_control_receipt(
            &SubmitManagedControlReceiptRequest {
                job_id: job_id.clone(),
                request_id: control.request_id,
                action: ManagedControlAction::EnterUpdater,
                accepted: false,
                facts: Vec::new(),
                evidence_sha256: Vec::new(),
                failure_reason: "no device rebound within the deadline".into(),
            },
            &fixture.envelope,
            &fixture.private_plan,
            NOW + 20,
        )
        .unwrap();

    let job = registry.job(&job_id).unwrap();
    assert_eq!(job.state(), JobState::OutcomeUnknown);
    assert!(matches!(
        job.stopped(),
        Some(JobStop::ControlOutcomeUnknown { .. })
    ));
    // The classification event names the cause. The journal always kept it;
    // the authority watching the stream used to get "unknown" with no reason,
    // which is a diagnosis that costs a bench visit instead of a read.
    let classified = job
        .events_from(0)
        .into_iter()
        .rev()
        .find(|event| event.kind == JobEventKind::OutcomeClassified)
        .expect("the classification was published");
    assert!(
        classified
            .facts
            .iter()
            .any(|fact| fact.key == "reason"
                && fact.value == "no device rebound within the deadline")
    );
}

/// The evidence digest of an accepted receipt is the canonical digest of its
/// own facts, recomputed by the daemon. Bytes that disagree with the facts are
/// refused with a code naming the drift — not stored as if they were evidence.
#[test]
fn an_accepted_receipt_whose_evidence_disagrees_with_its_facts_is_refused() {
    let root = TempRoot::new("evidence-mismatch");
    let fixture = plan_fixture();
    let mut registry = JobRegistry::new(&root.0);
    let job_id = registry
        .start(
            &fixture.envelope,
            &fixture.private_plan,
            ControllerSessionId::new("SESSION-1").unwrap(),
            &admission_facts(&fixture, "hdc-normal", NOW),
            NOW,
        )
        .unwrap();
    let snapshot = pending_snapshot(&mut registry, &fixture, &job_id, NOW);
    let (permit, tag, epoch) = mint(&snapshot, "PERMIT-1", NOW + 60_000);
    registry
        .submit_permit(
            &job_id,
            &snapshot.request_id,
            Some((permit, tag, epoch)),
            "",
            &secret(),
            &fixture.envelope,
            &fixture.private_plan,
            &fixture.profile,
            Some(current_facts(&snapshot, NOW + 10)),
            NOW + 10,
        )
        .unwrap();
    let control = registry
        .job(&job_id)
        .unwrap()
        .events_from(0)
        .into_iter()
        .rev()
        .find(|event| event.kind == JobEventKind::ManagedControlRequested)
        .and_then(|event| event.control_request)
        .unwrap();

    let error = registry
        .submit_control_receipt(
            &SubmitManagedControlReceiptRequest {
                job_id: job_id.clone(),
                request_id: control.request_id.clone(),
                action: ManagedControlAction::EnterUpdater,
                accepted: true,
                facts: vec![KeyValue {
                    key: "mode".into(),
                    value: "updater".into(),
                }],
                evidence_sha256: sha256(b"not the facts").as_bytes().to_vec(),
                failure_reason: String::new(),
            },
            &fixture.envelope,
            &fixture.private_plan,
            NOW + 20,
        )
        .unwrap_err();
    assert_eq!(error.code(), "CONTROL_EVIDENCE_MISMATCH");

    // The refusal answered nothing: the request is still pending, and a
    // corrected receipt is still takeable.
    let facts = vec![KeyValue {
        key: "mode".into(),
        value: "updater".into(),
    }];
    registry
        .submit_control_receipt(
            &SubmitManagedControlReceiptRequest {
                job_id: job_id.clone(),
                request_id: control.request_id,
                action: ManagedControlAction::EnterUpdater,
                accepted: true,
                evidence_sha256: canonical_facts_digest(&facts).as_bytes().to_vec(),
                facts,
                failure_reason: String::new(),
            },
            &fixture.envelope,
            &fixture.private_plan,
            NOW + 30,
        )
        .unwrap();
}

/// Golden vector, mirrored in ArkDeck's
/// `ArkForgeManagedControlPortContractTests`
/// (`testTheCanonicalFactsDigestMatchesTheDaemonsSpelling`). The authority
/// computes this digest when it builds an accepted receipt and this daemon
/// recomputes it before taking one, so if either side respells it, exactly one
/// of the two suites goes red.
#[test]
fn the_canonical_facts_digest_matches_the_authoritys_spelling() {
    let facts = vec![
        KeyValue {
            key: "mode".into(),
            value: "Loader".into(),
        },
        KeyValue {
            key: "stableIdentitySHA256".into(),
            value: "94a25a89c9c214dc9f8a0cf1b2cb3703a466e132a97fa015dfdbebfc65546f42".into(),
        },
        KeyValue {
            key: "usbTopology".into(),
            value: "17956864".into(),
        },
    ];
    assert_eq!(
        canonical_facts_digest(&facts).to_string(),
        "68c995f4a099a63f61c3226a70e6691889050b767100d991f383c5f09962ad1f"
    );
    assert_eq!(
        canonical_facts_digest(&[]).to_string(),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

/// The control request names a deadline, and the deadline is enforced: an
/// authority that answers nothing costs one deadline, not a job parked at
/// `permitConsuming` until an operator digs the journal out of a CBOR file.
#[test]
fn an_unanswered_control_request_expires_into_outcome_unknown() {
    let root = TempRoot::new("control-expiry");
    let fixture = plan_fixture();
    let mut registry = JobRegistry::new(&root.0);
    let job_id = registry
        .start(
            &fixture.envelope,
            &fixture.private_plan,
            ControllerSessionId::new("SESSION-1").unwrap(),
            &admission_facts(&fixture, "hdc-normal", NOW),
            NOW,
        )
        .unwrap();
    let snapshot = pending_snapshot(&mut registry, &fixture, &job_id, NOW);
    let (permit, tag, epoch) = mint(&snapshot, "PERMIT-1", NOW + 60_000);
    registry
        .submit_permit(
            &job_id,
            &snapshot.request_id,
            Some((permit, tag, epoch)),
            "",
            &secret(),
            &fixture.envelope,
            &fixture.private_plan,
            &fixture.profile,
            Some(current_facts(&snapshot, NOW + 10)),
            NOW + 10,
        )
        .unwrap();
    let control = registry
        .job(&job_id)
        .unwrap()
        .events_from(0)
        .into_iter()
        .rev()
        .find(|event| event.kind == JobEventKind::ManagedControlRequested)
        .and_then(|event| event.control_request)
        .unwrap();

    // Before the deadline the sweep classifies nothing.
    assert!(
        registry
            .expire_stale_controls(control.deadline_epoch_ms)
            .is_empty()
    );

    let expired = registry.expire_stale_controls(control.deadline_epoch_ms + 1);
    assert_eq!(expired, vec![job_id.clone()]);

    let job = registry.job(&job_id).unwrap();
    assert_eq!(job.state(), JobState::OutcomeUnknown);
    assert!(matches!(
        job.stopped(),
        Some(JobStop::ControlOutcomeUnknown { .. })
    ));
    let classified = job
        .events_from(0)
        .into_iter()
        .rev()
        .find(|event| event.kind == JobEventKind::OutcomeClassified)
        .expect("the expiry was published");
    assert!(
        classified
            .facts
            .iter()
            .any(|fact| fact.key == "reason" && fact.value.contains("expired unanswered"))
    );

    // The sweep settles each job once.
    assert!(
        registry
            .expire_stale_controls(control.deadline_epoch_ms + 2)
            .is_empty()
    );
}

/// architecture.md 13.4. Once an intent is durable, cancellation queues until
/// the action reports a settled boundary; it does not interrupt the action.
#[test]
fn a_job_with_a_durable_intent_queues_cancellation() {
    let root = TempRoot::new("cancel");
    let fixture = plan_fixture();
    let mut registry = JobRegistry::new(&root.0);
    let job_id = registry
        .start(
            &fixture.envelope,
            &fixture.private_plan,
            ControllerSessionId::new("SESSION-1").unwrap(),
            &admission_facts(&fixture, "hdc-normal", NOW),
            NOW,
        )
        .unwrap();

    // Before any permit, cancelling is safe.
    let mut early = JobRegistry::new(root.0.join("early"));
    let early_id = early
        .start(
            &fixture.envelope,
            &fixture.private_plan,
            ControllerSessionId::new("SESSION-1").unwrap(),
            &admission_facts(&fixture, "hdc-normal", NOW),
            NOW,
        )
        .unwrap();
    assert_eq!(
        early.cancel(&early_id, NOW + 1).unwrap(),
        CancellationDisposition::CancelledSafe
    );

    // Once a managed action owns the intent, cancellation queues at its safe
    // receipt boundary instead of pretending the action can be interrupted.
    let snapshot = pending_snapshot(&mut registry, &fixture, &job_id, NOW);
    let (permit, tag, epoch) = mint(&snapshot, "PERMIT-1", NOW + 60_000);
    registry
        .submit_permit(
            &job_id,
            &snapshot.request_id,
            Some((permit, tag, epoch)),
            "",
            &secret(),
            &fixture.envelope,
            &fixture.private_plan,
            &fixture.profile,
            Some(current_facts(&snapshot, NOW + 10)),
            NOW + 10,
        )
        .unwrap();
    assert_eq!(
        registry.cancel(&job_id, NOW + 20).unwrap(),
        CancellationDisposition::QueuedAtSafeBoundary
    );
}

#[test]
fn cancellation_during_a_non_interruptible_dispatch_waits_for_its_checkpoint() {
    let root = TempRoot::new("cancel-safe-boundary");
    let fixture = plan_fixture();
    let mut registry = JobRegistry::new(&root.0);
    let job_id = registry
        .start(
            &fixture.envelope,
            &fixture.private_plan,
            ControllerSessionId::new("SESSION-1").unwrap(),
            &admission_facts(&fixture, "hdc-normal", NOW),
            NOW,
        )
        .unwrap();

    let enter = pending_snapshot(&mut registry, &fixture, &job_id, NOW);
    let (permit, tag, epoch) = mint(&enter, "PERMIT-ENTER", NOW + 60_000);
    registry
        .submit_permit(
            &job_id,
            &enter.request_id,
            Some((permit, tag, epoch)),
            "",
            &secret(),
            &fixture.envelope,
            &fixture.private_plan,
            &fixture.profile,
            Some(current_facts(&enter, NOW + 1)),
            NOW + 1,
        )
        .unwrap();
    let control = registry
        .job(&job_id)
        .unwrap()
        .events_from(0)
        .into_iter()
        .rev()
        .find_map(|event| event.control_request)
        .unwrap();
    let control_facts = vec![KeyValue {
        key: "mode".into(),
        value: "Loader".into(),
    }];
    registry
        .submit_control_receipt(
            &SubmitManagedControlReceiptRequest {
                job_id: job_id.clone(),
                request_id: control.request_id,
                action: control.action,
                accepted: true,
                evidence_sha256: canonical_facts_digest(&control_facts).as_bytes().to_vec(),
                facts: control_facts,
                failure_reason: String::new(),
            },
            &fixture.envelope,
            &fixture.private_plan,
            NOW + 2,
        )
        .unwrap();

    let write = pending_snapshot(&mut registry, &fixture, &job_id, NOW + 3);
    let (permit, tag, epoch) = mint(&write, "PERMIT-WRITE", NOW + 60_000);
    registry
        .submit_permit(
            &job_id,
            &write.request_id,
            Some((permit, tag, epoch)),
            "",
            &secret(),
            &fixture.envelope,
            &fixture.private_plan,
            &fixture.profile,
            Some(current_facts(&write, NOW + 4)),
            NOW + 4,
        )
        .unwrap();
    let work = registry
        .take_pending_dispatch()
        .expect("write is in flight");
    assert_eq!(
        registry.cancel(&job_id, NOW + 5).unwrap(),
        CancellationDisposition::QueuedAtSafeBoundary
    );
    assert_ne!(
        registry.job(&job_id).unwrap().state(),
        JobState::CancelledSafe
    );

    registry
        .complete_dispatch(
            &job_id,
            arkforged::jobs::DispatchOutcome {
                disposition: arkforge_core::outcome::ActionDisposition::SemanticSuccess,
                facts: Vec::new(),
                evidence_digest: sha256(b"settled write"),
                verification: None,
            },
            &fixture.envelope,
            &fixture.private_plan,
            NOW + 6,
        )
        .unwrap();
    assert_eq!(
        registry.job(&job_id).unwrap().state(),
        JobState::CancelledSafe
    );
    assert!(registry.take_pending_dispatch().is_none());
    assert_eq!(work.job_id, job_id);
}

/// The permit crosses the wire as the exact bytes the authority signed, and is
/// read back as the same permit. A codec that re-encoded it would be signing
/// something else.
#[test]
fn a_permit_round_trips_through_its_canonical_bytes() {
    let root = TempRoot::new("bytes");
    let fixture = plan_fixture();
    let mut registry = JobRegistry::new(&root.0);
    let job_id = registry
        .start(
            &fixture.envelope,
            &fixture.private_plan,
            ControllerSessionId::new("SESSION-1").unwrap(),
            &admission_facts(&fixture, "hdc-normal", NOW),
            NOW,
        )
        .unwrap();
    let snapshot = pending_snapshot(&mut registry, &fixture, &job_id, NOW);
    let (permit, _, _) = mint(&snapshot, "PERMIT-1", NOW + 60_000);

    let bytes = permit.signing_body().unwrap();
    let decoded = StepPermit::from_canonical_bytes(&bytes).unwrap();
    assert_eq!(decoded.permit_id, permit.permit_id);
    assert_eq!(decoded.private_action_digest, permit.private_action_digest);
    assert_eq!(decoded.signing_body().unwrap(), bytes);

    // Bytes that are not the deterministic encoding are refused rather than
    // accepted and re-encoded into something the tag does not cover.
    let mut trailing = bytes.clone();
    trailing.push(0x00);
    assert!(StepPermit::from_canonical_bytes(&trailing).is_err());
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// A native semantic port that answers like a DAYU200 without touching hardware.
#[derive(Debug, Default)]
struct ScriptedPort {
    operations: std::cell::RefCell<Vec<String>>,
    written: std::cell::RefCell<Vec<String>>,
    full_read_face: bool,
}

impl ScriptedPort {
    fn writes(&self) -> Vec<String> {
        self.written.borrow().clone()
    }

    fn issued(&self, operation: &str) -> usize {
        self.operations
            .borrow()
            .iter()
            .filter(|observed| observed.as_str() == operation)
            .count()
    }

    fn table() -> arkforge_artifact::manifest::PartitionTableFact {
        let rows = [
            ("uboot", 8_192),
            ("misc", 16_384),
            ("bootctrl", 24_576),
            ("resource", 28_672),
            ("boot_linux", 40_960),
            ("ramdisk", 237_568),
            ("system", 245_760),
            ("vendor", 4_440_064),
            ("sys-prod", 6_537_216),
            ("chip-prod", 6_639_616),
            ("updater", 6_742_016),
            ("eng_system", 6_815_744),
            ("eng_chipset", 6_848_512),
            ("chip_ckm", 6_938_624),
            ("userdata", 19_955_712),
        ];
        let entries = rows
            .iter()
            .enumerate()
            .map(|(index, (name, offset))| {
                let next = rows.get(index + 1).map(|(_, next)| *next);
                arkforge_artifact::manifest::PartitionEntryFact {
                    index: index as u32,
                    name: (*name).to_string(),
                    offset_sectors: *offset,
                    size_sectors: next.map(|next| next - *offset),
                    attribute: None,
                    grammar_branch: if next.is_some() {
                        arkforge_artifact::manifest::GrammarBranch::Fixed
                    } else {
                        arkforge_artifact::manifest::GrammarBranch::RemainderGrow
                    },
                }
            })
            .collect();
        arkforge_artifact::manifest::PartitionTableFact {
            device: "native-rockusb".into(),
            logical_block_size: 512,
            entries,
        }
    }
}

impl arkforge_provider::rockchip_execute::RockUsbPort for ScriptedPort {
    fn discover(
        &self,
    ) -> Result<
        arkforge_provider::rockchip_execute::RockUsbObservation<
            Vec<arkforge_provider::rockchip_execute::RockUsbDevice>,
        >,
        arkforge_provider::rockchip_execute::RockUsbPortFailure,
    > {
        self.operations.borrow_mut().push("discover".into());
        Ok(arkforge_provider::rockchip_execute::RockUsbObservation {
            value: vec![arkforge_provider::rockchip_execute::RockUsbDevice {
                vendor_id: 0x2207,
                product_id: 0x350a,
                usb_specification: Some(0x0200),
                location: arkforge_provider::rockchip_execute::RockUsbLocation::IokitTopology(
                    0x0112_0000,
                ),
                mode: "loader".into(),
                serial: Some("SCRIPTED".into()),
                product_name: Some("DAYU200 Loader".into()),
                vendor_name: Some("Rockchip".into()),
                device_release: Some(0x0100),
            }],
            evidence_digest: sha256(b"scripted native discovery"),
        })
    }

    fn read_partition_table(
        &self,
    ) -> Result<
        arkforge_provider::rockchip_execute::RockUsbObservation<
            arkforge_artifact::manifest::PartitionTableFact,
        >,
        arkforge_provider::rockchip_execute::RockUsbPortFailure,
    > {
        self.operations
            .borrow_mut()
            .push("readPartitionTable".into());
        Ok(arkforge_provider::rockchip_execute::RockUsbObservation {
            value: Self::table(),
            evidence_digest: sha256(b"scripted native GPT"),
        })
    }

    fn read_sectors(
        &self,
        begin_sector: u64,
        sectors: u64,
        _scratch: &std::path::Path,
    ) -> Result<
        arkforge_provider::rockchip_execute::RockUsbObservation<Vec<u8>>,
        arkforge_provider::rockchip_execute::RockUsbPortFailure,
    > {
        self.operations.borrow_mut().push("readSectors".into());
        let mut bytes = if begin_sector == 1 || self.full_read_face {
            (0..sectors as usize * 512)
                .map(|index| (index % 251) as u8)
                .collect()
        } else {
            vec![0xCC; sectors as usize * 512]
        };
        if begin_sector == 1 {
            bytes[..8].copy_from_slice(b"EFI PART");
        }
        Ok(arkforge_provider::rockchip_execute::RockUsbObservation {
            evidence_digest: sha256(&bytes),
            value: bytes,
        })
    }

    fn write_partition(
        &self,
        partition: &str,
        _begin_sector: u64,
        image: &mut arkforge_provider::rockchip_execute::ValidatedImage,
    ) -> Result<
        arkforge_provider::rockchip_execute::RockUsbMutationReceipt,
        arkforge_provider::rockchip_execute::RockUsbPortFailure,
    > {
        let image = image.staged();
        let bytes = std::fs::read(&image.path).map_err(|error| {
            arkforge_provider::rockchip_execute::RockUsbPortFailure::BeforeIo(error.to_string())
        })?;
        self.operations.borrow_mut().push("writePartition".into());
        self.written.borrow_mut().push(partition.to_string());
        Ok(
            arkforge_provider::rockchip_execute::RockUsbMutationReceipt {
                semantic_success: true,
                evidence_digest: sha256(b"scripted native WRITE_LBA"),
                duration_ms: 1,
                detail: "native WRITE_LBA confirmed".into(),
                progress: Some(arkforge_provider::rockchip_execute::RockUsbWriteProgress {
                    payload_bytes: bytes.len() as u64,
                    wire_sectors: bytes.len() as u64 / 512 + u64::from(bytes.len() % 512 != 0),
                    chunks: 1,
                    chunk_sectors: 1,
                    payload_digest: sha256(&bytes),
                }),
            },
        )
    }

    fn reset_device(
        &self,
    ) -> Result<
        arkforge_provider::rockchip_execute::RockUsbMutationReceipt,
        arkforge_provider::rockchip_execute::RockUsbPortFailure,
    > {
        self.operations.borrow_mut().push("resetDevice".into());
        Ok(
            arkforge_provider::rockchip_execute::RockUsbMutationReceipt {
                semantic_success: true,
                evidence_digest: sha256(b"scripted native DEVICE_RESET"),
                duration_ms: 1,
                detail: "native DEVICE_RESET confirmed".into(),
                progress: None,
            },
        )
    }
}

/// Drives a job to completion, answering every admission and running every
/// dispatch, and returns the receipts the job published.
fn walk_to_completion(
    registry: &mut JobRegistry,
    dispatcher: &mut arkforged::dispatch::Dispatcher<'_>,
    fixture: &Fixture,
    job_id: &str,
) -> Vec<arkforge_ipc::messages::ActionReceiptSummary> {
    let mut clock = NOW + 1;
    // One iteration per step transition; the bound is a runaway guard, not a
    // step count.
    for round in 0..200u32 {
        clock += 1;
        if registry.job(job_id).unwrap().stopped().is_some() {
            break;
        }

        if let Some(work) = registry.take_pending_dispatch() {
            let outcome = dispatcher.run(&work);
            registry
                .complete_dispatch(
                    &work.job_id,
                    outcome,
                    &fixture.envelope,
                    &fixture.private_plan,
                    clock,
                )
                .unwrap();
            continue;
        }

        let job = registry.job(job_id).unwrap();
        let latest = job.events_from(0);
        if let Some(control) = latest
            .iter()
            .rev()
            .find(|event| event.kind == JobEventKind::ManagedControlRequested)
            .and_then(|event| event.control_request.clone())
        {
            let already_answered = latest.iter().any(|event| {
                event.kind == JobEventKind::ActionReceipt
                    && event
                        .receipt
                        .as_ref()
                        .is_some_and(|receipt| receipt.step_id == control.step_id)
            });
            if !already_answered {
                let facts = if control.expected_facts.is_empty() {
                    vec![KeyValue {
                        key: "mode".into(),
                        value: "updater".into(),
                    }]
                } else {
                    control.expected_facts.clone()
                };
                registry
                    .submit_control_receipt(
                        &SubmitManagedControlReceiptRequest {
                            job_id: job_id.to_string(),
                            request_id: control.request_id.clone(),
                            action: control.action,
                            accepted: true,
                            evidence_sha256: canonical_facts_digest(&facts).as_bytes().to_vec(),
                            facts,
                            failure_reason: String::new(),
                        },
                        &fixture.envelope,
                        &fixture.private_plan,
                        clock,
                    )
                    .unwrap();
                continue;
            }
        }

        let snapshot = pending_snapshot(registry, fixture, job_id, clock);
        let (permit, tag, epoch) = mint(&snapshot, &format!("PERMIT-{round}"), clock + 60_000);
        registry
            .submit_permit(
                job_id,
                &snapshot.request_id,
                Some((permit, tag, epoch)),
                "",
                &secret(),
                &fixture.envelope,
                &fixture.private_plan,
                &fixture.profile,
                Some(current_facts(&snapshot, clock)),
                clock,
            )
            .unwrap();
    }

    registry
        .job(job_id)
        .unwrap()
        .events_from(0)
        .into_iter()
        .filter_map(|event| event.receipt)
        .collect()
}

/// The whole plan, end to end, with every write dispatched.
#[test]
fn a_job_dispatches_every_step_and_reaches_a_verdict_on_each() {
    let root = TempRoot::new("dispatch-walk");
    let fixture = plan_fixture();
    let jobs_root = root.0.join("jobs");
    let mut registry = JobRegistry::new(&jobs_root);
    let port = ScriptedPort::default();
    let mut dispatcher =
        arkforged::dispatch::Dispatcher::new(root.0.join("store"), root.0.join("work"), &port);

    // The dispatcher stages out of the same store the plan was built from.
    stage_archive_into(&root.0.join("store"));

    let job_id = registry
        .start(
            &fixture.envelope,
            &fixture.private_plan,
            ControllerSessionId::new("SESSION-1").unwrap(),
            &admission_facts(&fixture, "hdc-normal", NOW),
            NOW,
        )
        .unwrap();
    let receipts = walk_to_completion(&mut registry, &mut dispatcher, &fixture, &job_id);

    let job = registry.job(&job_id).unwrap();
    assert_eq!(
        job.stopped(),
        Some(&JobStop::Completed),
        "state {:?}, receipts {}",
        job.state(),
        receipts.len()
    );
    assert_eq!(job.state(), JobState::Succeeded);

    // Nine partitions were written through native typed calls in Profile order.
    let writes = port.writes();
    assert_eq!(writes.len(), 9, "{writes:?}");
    assert_eq!(
        writes,
        vec![
            "uboot".to_string(),
            "resource".to_string(),
            "boot_linux".to_string(),
            "ramdisk".to_string(),
            "system".to_string(),
            "vendor".to_string(),
            "updater".to_string(),
            "chip_ckm".to_string(),
            "userdata".to_string(),
        ],
        "writes run in the profile's declared order"
    );

    // The device's own table was read before any of them.
    assert_eq!(port.issued("readPartitionTable"), 1);
    assert_eq!(port.issued("resetDevice"), 1);
    let rebinds = job
        .journal()
        .records()
        .iter()
        .filter(|record| record.kind == JournalRecordKind::RebindObserved)
        .collect::<Vec<_>>();
    assert!(
        !rebinds.is_empty(),
        "each sealed mode change must enter rebindWait and record the fresh session"
    );
    assert!(rebinds.iter().all(|record| {
        record
            .facts
            .iter()
            .any(|(key, _)| key.as_str() == "transportSessionDigest")
    }));

    let postflight = receipts
        .iter()
        .find(|receipt| receipt.step_id == "STEP-023")
        .expect("postflight publishes a receipt");
    assert!(postflight.facts.iter().any(|fact| {
        fact.key == "const.ohos.fullname" && fact.value == fixture::FIXTURE_BUILD_VERSION
    }));
    assert!(
        postflight
            .facts
            .iter()
            .any(|fact| fact.key == "const.product.model" && fact.value == "ohos")
    );

    // Every readback landed outside the measured read window, so every one is a
    // typed skip — and a typed skip carries no strength (architecture.md 16.4).
    let verdicts: Vec<&str> = receipts
        .iter()
        .filter(|receipt| !receipt.verification_outcome.is_empty())
        .map(|receipt| receipt.verification_outcome.as_str())
        .collect();
    assert_eq!(verdicts.len(), 9, "one verdict per target");
    assert!(
        verdicts.iter().all(|outcome| *outcome == "typedSkip"),
        "{verdicts:?}"
    );
    for receipt in &receipts {
        assert!(
            receipt.strength_is_consistent(),
            "a typed skip must carry no strength: {receipt:?}"
        );
        if receipt.verification_outcome == "typedSkip" {
            assert_eq!(receipt.typed_skip_reason, "skipped-lba-read-window");
        }
    }
    for event in job
        .events_from(0)
        .into_iter()
        .filter(|event| event.receipt.is_some())
    {
        let linked = job
            .journal()
            .records()
            .iter()
            .find(|record| {
                record.record_digest.as_bytes().as_slice() == event.journal_record_sha256.as_slice()
            })
            .expect("receipt event links to a durable journal record");
        assert_eq!(linked.kind, JournalRecordKind::SemanticReceiptRecorded);
    }

    // Success is itself a durable classification. Reopening the registry must
    // neither reinterpret the completed job as a safe cancellation nor append
    // another record. Repeat the check to catch restart-by-restart growth.
    let terminal_sequence = job.events_from(0).last().unwrap().sequence;
    let journal_path = jobs_root.join(format!("{job_id}.journal"));
    let terminal_journal_len = std::fs::metadata(&journal_path).unwrap().len();
    drop(registry);

    for _ in 0..2 {
        let reopened = JobRegistry::open(&jobs_root).unwrap();
        let recovered = reopened
            .job(&job_id)
            .expect("completed job survives restart");
        assert_eq!(recovered.state(), JobState::Succeeded);
        assert_eq!(recovered.stopped(), Some(&JobStop::Completed));
        let recovered_receipts = recovered
            .events_from(0)
            .into_iter()
            .filter_map(|event| event.receipt)
            .collect::<Vec<_>>();
        assert_eq!(recovered_receipts.len(), receipts.len());
        for (recovered_receipt, original_receipt) in recovered_receipts.iter().zip(&receipts) {
            assert_eq!(recovered_receipt.step_id, original_receipt.step_id);
            assert_eq!(
                recovered_receipt.receipt_digest().unwrap(),
                original_receipt.receipt_digest().unwrap(),
                "a restarted daemon must replay the same canonical receipt, not only an opaque digest"
            );
        }
        assert!(recovered_receipts.last().is_some_and(|receipt| {
            receipt.step_id == "STEP-023"
                && receipt
                    .facts
                    .iter()
                    .any(|fact| fact.key == "const.ohos.fullname")
        }));
        assert_eq!(
            recovered.events_from(0).last().unwrap().sequence,
            terminal_sequence
        );
        assert_eq!(
            std::fs::metadata(&journal_path).unwrap().len(),
            terminal_journal_len,
            "rehydrating a terminal job must not rewrite its journal"
        );
    }
}

#[test]
fn a_conclusive_verification_failure_stops_before_reset_and_survives_restart() {
    let root = TempRoot::new("verification-failed");
    let fixture = plan_fixture();
    let jobs_root = root.0.join("jobs");
    let mut registry = JobRegistry::new(&jobs_root);
    let port = ScriptedPort {
        full_read_face: true,
        ..ScriptedPort::default()
    };
    let mut dispatcher =
        arkforged::dispatch::Dispatcher::new(root.0.join("store"), root.0.join("work"), &port);
    stage_archive_into(&root.0.join("store"));

    let job_id = registry
        .start(
            &fixture.envelope,
            &fixture.private_plan,
            ControllerSessionId::new("SESSION-1").unwrap(),
            &admission_facts(&fixture, "hdc-normal", NOW),
            NOW,
        )
        .unwrap();
    let receipts = walk_to_completion(&mut registry, &mut dispatcher, &fixture, &job_id);
    let job = registry.job(&job_id).unwrap();
    assert_eq!(job.state(), JobState::ConfirmedFailed);
    assert!(matches!(
        job.stopped(),
        Some(JobStop::ConfirmedFailure { .. })
    ));
    assert!(receipts.iter().any(|receipt| {
        receipt.verification_outcome == "failed" && !receipt.failure_classification.is_empty()
    }));
    assert_eq!(port.writes().len(), 9, "writes finish before verification");
    assert_eq!(port.issued("resetDevice"), 0, "failure blocks reset");

    let terminal_sequence = job.last_sequence();
    drop(registry);
    let reopened = JobRegistry::open(&jobs_root).unwrap();
    let recovered = reopened.job(&job_id).unwrap();
    assert_eq!(recovered.state(), JobState::ConfirmedFailed);
    assert_eq!(recovered.last_sequence(), terminal_sequence);
    assert!(matches!(
        recovered.stopped(),
        Some(JobStop::ConfirmedFailure { .. })
    ));
}

/// A refused staging precondition never reaches native USB, and the job says
/// so without becoming unknown.
#[test]
fn a_dispatch_refused_before_native_usb_confirms_no_effect() {
    let root = TempRoot::new("dispatch-refused");
    let fixture = plan_fixture();
    let mut registry = JobRegistry::new(root.0.join("jobs"));
    let port = ScriptedPort::default();
    let mut dispatcher =
        arkforged::dispatch::Dispatcher::new(root.0.join("store"), root.0.join("work"), &port);
    // No archive in the store, so staging cannot resolve — a refusal that
    // happens before native USB runs.
    let job_id = registry
        .start(
            &fixture.envelope,
            &fixture.private_plan,
            ControllerSessionId::new("SESSION-1").unwrap(),
            &admission_facts(&fixture, "hdc-normal", NOW),
            NOW,
        )
        .unwrap();

    let mut clock = NOW + 1;
    let mut refused = None;
    for round in 0..40u32 {
        clock += 1;
        if registry.job(&job_id).unwrap().stopped().is_some() {
            break;
        }
        if let Some(work) = registry.take_pending_dispatch() {
            let outcome = dispatcher.run(&work);
            let disposition = outcome.disposition;
            registry
                .complete_dispatch(
                    &work.job_id,
                    outcome,
                    &fixture.envelope,
                    &fixture.private_plan,
                    clock,
                )
                .unwrap();
            if disposition != arkforge_core::outcome::ActionDisposition::SemanticSuccess {
                refused = Some(disposition);
                break;
            }
            continue;
        }
        let job = registry.job(&job_id).unwrap();
        if let Some(control) = job
            .events_from(0)
            .iter()
            .rev()
            .find(|event| event.kind == JobEventKind::ManagedControlRequested)
            .and_then(|event| event.control_request.clone())
        {
            let answered = job.events_from(0).iter().any(|event| {
                event
                    .receipt
                    .as_ref()
                    .is_some_and(|r| r.step_id == control.step_id)
            });
            if !answered {
                registry
                    .submit_control_receipt(
                        &SubmitManagedControlReceiptRequest {
                            job_id: job_id.clone(),
                            request_id: control.request_id,
                            action: control.action,
                            accepted: true,
                            facts: Vec::new(),
                            evidence_sha256: canonical_facts_digest(&[]).as_bytes().to_vec(),
                            failure_reason: String::new(),
                        },
                        &fixture.envelope,
                        &fixture.private_plan,
                        clock,
                    )
                    .unwrap();
                continue;
            }
        }
        let snapshot = pending_snapshot(&mut registry, &fixture, &job_id, clock);
        let (permit, tag, epoch) = mint(&snapshot, &format!("PERMIT-R{round}"), clock + 60_000);
        registry
            .submit_permit(
                &job_id,
                &snapshot.request_id,
                Some((permit, tag, epoch)),
                "",
                &secret(),
                &fixture.envelope,
                &fixture.private_plan,
                &fixture.profile,
                Some(current_facts(&snapshot, clock)),
                clock,
            )
            .unwrap();
    }

    assert_eq!(
        refused,
        Some(arkforge_core::outcome::ActionDisposition::ConfirmedNoEffect),
        "a staging failure is provably no effect, not an unknown outcome"
    );
    assert!(port.writes().is_empty(), "no write was spawned");
}

/// Imports the fixture archive so the dispatcher has something to stage.
fn stage_archive_into(store_root: &std::path::Path) {
    let store = arkforge_artifact::cas::ContentAddressedStore::open(
        store_root,
        arkforge_artifact::cas::CasQuota::dayu200_default(),
    )
    .unwrap();
    let archive = fixture::dayu200_archive();
    store
        .import(archive.as_slice(), archive.len() as u64, None)
        .unwrap();
}

/// A plan materialized for one tool must not run against another.
///
/// The toolchain digest is part of the maturity combination
/// (architecture.md 12.3): a daemon with different bytes bound would be
/// executing a combination nobody published, and it would look like success.
#[test]
fn a_plan_built_for_another_toolchain_is_refused_by_digest() {
    let fixture = plan_fixture();
    let mut engine = arkforge_engine::Engine::new();
    engine
        .plans_mut()
        .insert(arkforge_engine::StoredPlan {
            envelope: fixture.envelope.clone(),
            private_plan: fixture.private_plan.clone(),
        })
        .unwrap();

    let plan_id = arkforge_core::PlanId::new(fixture.envelope.plan_id.as_str()).unwrap();
    let digest = fixture.envelope.plan_digest;

    // The tool the plan was built for.
    let matching = arkforge_engine::ExecutionReadiness {
        authority_paired: true,
        dispatcher: Some(arkforge_engine::BoundToolchain {
            id: OpaqueId::new("example-tool-fixed").unwrap(),
            backend_digest: fixture.envelope.toolchain.backend_digest,
        }),
    };
    assert!(engine.start_execution(&plan_id, digest, &matching).is_ok());

    // A different one, with everything else in place.
    let other = arkforge_engine::ExecutionReadiness {
        authority_paired: true,
        dispatcher: Some(arkforge_engine::BoundToolchain {
            id: OpaqueId::new("example-tool-fixed").unwrap(),
            backend_digest: sha256(b"some other build of the tool"),
        }),
    };
    // Standing readiness is satisfied — the refusal is specific to this plan.
    assert!(other.is_ready());
    let error = engine
        .start_execution(&plan_id, digest, &other)
        .unwrap_err();
    match error {
        arkforge_engine::EngineError::ExecutionDisabled(blockers) => {
            assert_eq!(blockers.len(), 1);
            assert_eq!(blockers[0].code(), "TOOLCHAIN_DIGEST_MISMATCH");
        }
        other => panic!("expected a toolchain refusal, got {other}"),
    }
}
