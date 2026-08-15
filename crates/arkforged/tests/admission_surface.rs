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
    ControllerPairingSecret, PairingEpoch, PermitIntegrityTag, StepPermit,
};
use arkforge_core::digest::sha256;
use arkforge_core::identity::{
    HostPlatform, MaturityKey, MaturityState, ToolchainIdentity, ToolchainKind, Version,
};
use arkforge_core::ids::{
    AttemptId, ControllerSessionId, JobId, OpaqueId, PermitId, PlanId, StepId,
};
use arkforge_core::plan::{FlashPlanEnvelope, PlanMaterialization};
use arkforge_core::profile::{self, DeviceProfile};
use arkforge_core::projection::StoredProviderPlan;
use arkforge_core::{AuthorityBindingRef, AuthorityNamespace, Sha256Digest};
use arkforge_engine::JobState;
use arkforge_ipc::messages::{
    JobEventKind, KeyValue, ManagedControlAction, SubmitManagedControlReceiptRequest,
};
use arkforge_provider::rockchip::RockchipProvider;
use arkforge_provider::{
    FlashIntent, FlashProvider, MaterializeRequest, MaturityRegistry, ProbeContext,
};
use arkforge_transport::replay::TranscriptTransport;
use arkforge_transport::{transcript, DeviceTransport, TypedDiscoveryFilter};
use arkforged::jobs::{JobRegistry, JobStop};
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

struct Fixture {
    envelope: FlashPlanEnvelope,
    private_plan: StoredProviderPlan,
    profile: DeviceProfile,
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
        id: OpaqueId::new("example-tool-fixed").unwrap(),
        kind: ToolchainKind::FixedTool,
        version: Version::new(1, 32, 0),
        backend_digest: sha256(b"fixed tool"),
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
        intent: FlashIntent::FullRestore,
        artifact: &manifest,
        artifact_id: OpaqueId::new("ART-ADMISSION").unwrap(),
        profile: &profile,
        probe: &probe,
        authority_binding: binding(),
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
    registry: &JobRegistry,
    job_id: &str,
) -> arkforge_ipc::messages::StepAdmissionSnapshot {
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
        .start(&fixture.envelope, &fixture.private_plan, NOW)
        .unwrap();

    // The job publishes what it needs and stops. Nothing has been dispatched.
    let job = registry.job(&job_id).unwrap();
    assert_eq!(job.state(), JobState::Preflight);
    let kinds: Vec<JobEventKind> = job.events_from(0).iter().map(|event| event.kind).collect();
    assert_eq!(
        kinds,
        vec![JobEventKind::StateChanged, JobEventKind::StepAdmissionRequested]
    );

    let snapshot = pending_snapshot(&registry, &job_id);
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

    // The authority reports what its own channel observed.
    registry
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
                evidence_sha256: sha256(b"control evidence").as_bytes().to_vec(),
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

    // Step two is a device probe, which this build cannot dispatch. The job
    // stops there having said so, rather than pretending it ran.
    let snapshot = pending_snapshot(&registry, &job_id);
    assert_ne!(snapshot.step_id, control.step_id, "the job moved to the next step");
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
        .start(&fixture.envelope, &fixture.private_plan, NOW)
        .unwrap();
    let snapshot = pending_snapshot(&registry, &job_id);

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
            NOW + 10,
        )
        .unwrap();

    let job = registry.job(&job_id).unwrap();
    assert_eq!(job.state(), JobState::CancelledSafe);
    assert!(matches!(
        job.stopped(),
        Some(JobStop::RefusedByAuthority { .. })
    ));
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
        .start(&fixture.envelope, &fixture.private_plan, NOW)
        .unwrap();
    let snapshot = pending_snapshot(&registry, &job_id);
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
            late,
        )
        .unwrap_err();
    assert_eq!(error.code(), "SNAPSHOT_EXPIRED");

    // A fresh snapshot was published, so the authority has something to sign.
    let fresh = pending_snapshot(&registry, &job_id);
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
        .start(&fixture.envelope, &fixture.private_plan, NOW)
        .unwrap();
    let snapshot = pending_snapshot(&registry, &job_id);

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
            NOW + 10,
        )
        .unwrap_err();
    assert_eq!(error.code(), "PERMIT_REJECTED");
    // Nothing was recorded: the job is still waiting for the same admission.
    assert_eq!(
        pending_snapshot(&registry, &job_id).request_id,
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
        .start(&fixture.envelope, &fixture.private_plan, NOW)
        .unwrap();
    let snapshot = pending_snapshot(&registry, &job_id);
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
        .start(&fixture.envelope, &fixture.private_plan, NOW)
        .unwrap();
    let snapshot = pending_snapshot(&registry, &job_id);
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
        assert_eq!(error.code(), "RECEIPT_CARRIES_FORBIDDEN_FACTS", "{forbidden}");
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
        .start(&fixture.envelope, &fixture.private_plan, NOW)
        .unwrap();
    let snapshot = pending_snapshot(&registry, &job_id);
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

    registry
        .submit_control_receipt(
            &SubmitManagedControlReceiptRequest {
                job_id: job_id.clone(),
                request_id: control.request_id,
                action: ManagedControlAction::EnterUpdater,
                accepted: false,
                facts: Vec::new(),
                evidence_sha256: sha256(b"nothing observed").as_bytes().to_vec(),
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
}

/// architecture.md 13.4. Once an intent is durable there is an unresolved
/// effect, and a job with one may not report `cancelledSafe`.
#[test]
fn a_job_with_a_durable_intent_cannot_be_cancelled_safely() {
    let root = TempRoot::new("cancel");
    let fixture = plan_fixture();
    let mut registry = JobRegistry::new(&root.0);
    let job_id = registry
        .start(&fixture.envelope, &fixture.private_plan, NOW)
        .unwrap();

    // Before any permit, cancelling is safe.
    let mut early = JobRegistry::new(&root.0.join("early"));
    let early_id = early
        .start(&fixture.envelope, &fixture.private_plan, NOW)
        .unwrap();
    assert_eq!(
        early.cancel(&early_id, NOW + 1).unwrap(),
        JobState::CancelledSafe
    );

    // After one, it is not.
    let snapshot = pending_snapshot(&registry, &job_id);
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
            NOW + 10,
        )
        .unwrap();
    let error = registry.cancel(&job_id, NOW + 20).unwrap_err();
    assert_eq!(error.code(), "CANCEL_NOT_SAFE");
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
        .start(&fixture.envelope, &fixture.private_plan, NOW)
        .unwrap();
    let snapshot = pending_snapshot(&registry, &job_id);
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

/// A tool port that answers like the real one, without a device.
///
/// The outputs are the shapes measured on a DAYU200 in Loader mode on
/// 2026-08-15 (AD-018, AD-019): the three-column partition listing, a windowed
/// read face, and the vendor tool's own success markers. Scripting them is what
/// lets the whole job walk — writes included — run in a test suite that must
/// never touch hardware.
#[derive(Debug, Default)]
struct ScriptedPort {
    argv_log: std::cell::RefCell<Vec<Vec<String>>>,
}

const REAL_PPT: &str = concat!(
    "**********Partition Info(GPT)**********\r\n",
    "NO  LBA       Name                \r\n",
    "00  00002000  uboot\r\n",
    "01  00004000  misc\r\n",
    "02  00006000  bootctrl\r\n",
    "03  00007000  resource\r\n",
    "04  0000A000  boot_linux\r\n",
    "05  0003A000  ramdisk\r\n",
    "06  0003C000  system\r\n",
    "07  0043C000  vendor\r\n",
    "08  0063C000  sys-prod\r\n",
    "09  00655000  chip-prod\r\n",
    "10  0066E000  updater\r\n",
    "11  0067E000  eng_system\r\n",
    "12  00686000  eng_chipset\r\n",
    "13  0069E000  chip_ckm\r\n",
    "14  01308000  userdata\r\n",
);

impl ScriptedPort {
    fn writes(&self) -> Vec<Vec<String>> {
        self.argv_log
            .borrow()
            .iter()
            .filter(|argv| argv.first().map(String::as_str) == Some("wlx"))
            .cloned()
            .collect()
    }

    fn issued(&self, command: &str) -> usize {
        self.argv_log
            .borrow()
            .iter()
            .filter(|argv| argv.first().map(String::as_str) == Some(command))
            .count()
    }
}

impl arkforge_provider::rockchip_execute::FixedToolPort for ScriptedPort {
    fn run(
        &self,
        invocation: &arkforge_provider::rockchip_execute::ToolInvocation,
    ) -> Result<arkforge_provider::rockchip_execute::ToolReceipt, String> {
        self.argv_log.borrow_mut().push(invocation.argv.clone());
        let receipt = |stdout: String| arkforge_provider::rockchip_execute::ToolReceipt {
            exited_zero: true,
            stdout,
            stderr: String::new(),
            truncated: false,
            duration_ms: 1,
        };
        match invocation.argv.first().map(String::as_str) {
            Some("ld") => Ok(receipt(
                "DevNo=1\tVid=0x2207,Pid=0x350a,LocationID=102\tLoader\n".into(),
            )),
            Some("ppt") => Ok(receipt(REAL_PPT.into())),
            Some("rd") => Ok(receipt("Reset Device OK.\n".into())),
            Some("wlx") => Ok(receipt("Write LBA from file (100%)\n".into())),
            Some("rl") => {
                let begin: u64 = invocation.argv[1].parse().map_err(|_| "bad sector")?;
                let sectors: usize = invocation.argv[2].parse().map_err(|_| "bad count")?;
                let out = std::path::PathBuf::from(&invocation.argv[3]);
                // Sector 1 carries a real table; everything else reads as the
                // erased-medium filler, which is what a windowed read face
                // returns regardless of what is on the medium (AD-006).
                let bytes = if begin == 1 {
                    let mut block = vec![0u8; sectors * 512];
                    block[..8].copy_from_slice(b"EFI PART");
                    block
                } else {
                    vec![0xCC; sectors * 512]
                };
                std::fs::write(&out, &bytes).map_err(|error| error.to_string())?;
                Ok(receipt(String::new()))
            }
            other => Err(format!("this port scripts no answer for {other:?}")),
        }
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
                        .map_or(false, |receipt| receipt.step_id == control.step_id)
            });
            if !already_answered {
                registry
                    .submit_control_receipt(
                        &SubmitManagedControlReceiptRequest {
                            job_id: job_id.to_string(),
                            request_id: control.request_id.clone(),
                            action: control.action,
                            accepted: true,
                            facts: vec![KeyValue {
                                key: "mode".into(),
                                value: "updater".into(),
                            }],
                            evidence_sha256: sha256(b"control").as_bytes().to_vec(),
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

        let snapshot = pending_snapshot(registry, job_id);
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
    let mut registry = JobRegistry::new(root.0.join("jobs"));
    let port = ScriptedPort::default();
    let mut dispatcher =
        arkforged::dispatch::Dispatcher::new(root.0.join("store"), root.0.join("work"), &port);

    // The dispatcher stages out of the same store the plan was built from.
    stage_archive_into(&root.0.join("store"));

    let job_id = registry
        .start(&fixture.envelope, &fixture.private_plan, NOW)
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

    // Nine partitions were written, each by name, each from a staged file.
    let writes = port.writes();
    assert_eq!(writes.len(), 9, "{writes:?}");
    let written: Vec<&str> = writes.iter().map(|argv| argv[1].as_str()).collect();
    assert_eq!(
        written,
        vec![
            "uboot",
            "resource",
            "boot_linux",
            "ramdisk",
            "system",
            "vendor",
            "updater",
            "chip_ckm",
            "userdata",
        ],
        "writes run in the profile's declared order"
    );

    // The device's own table was read before any of them.
    assert_eq!(port.issued("ppt"), 1);
    assert_eq!(port.issued("rd"), 1);

    // Every readback landed outside the measured read window, so every one is a
    // typed skip — and a typed skip carries no strength (architecture.md 16.4).
    let verdicts: Vec<&str> = receipts
        .iter()
        .filter(|receipt| !receipt.verification_outcome.is_empty())
        .map(|receipt| receipt.verification_outcome.as_str())
        .collect();
    assert_eq!(verdicts.len(), 9, "one verdict per target");
    assert!(verdicts.iter().all(|outcome| *outcome == "typedSkip"), "{verdicts:?}");
    for receipt in &receipts {
        assert!(
            receipt.strength_is_consistent(),
            "a typed skip must carry no strength: {receipt:?}"
        );
        if receipt.verification_outcome == "typedSkip" {
            assert_eq!(receipt.typed_skip_reason, "skipped-lba-read-window");
        }
    }
}

/// A write the profile does not allow never reaches the tool, and the job says
/// so without becoming unknown: nothing was spawned, so nothing happened.
#[test]
fn a_dispatch_refused_before_the_spawn_confirms_no_effect() {
    let root = TempRoot::new("dispatch-refused");
    let fixture = plan_fixture();
    let mut registry = JobRegistry::new(root.0.join("jobs"));
    let port = ScriptedPort::default();
    let mut dispatcher =
        arkforged::dispatch::Dispatcher::new(root.0.join("store"), root.0.join("work"), &port);
    // No archive in the store, so staging cannot resolve — a refusal that
    // happens before any tool runs.
    let job_id = registry
        .start(&fixture.envelope, &fixture.private_plan, NOW)
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
                    .map_or(false, |r| r.step_id == control.step_id)
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
                            evidence_sha256: sha256(b"c").as_bytes().to_vec(),
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
        let snapshot = pending_snapshot(&registry, &job_id);
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
