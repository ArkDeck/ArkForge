//! The daemon's API surface.
//!
//! AF-V1 acceptance: "daemon read-only API" and "startExecution disabled".
//! These assertions are about the service, so they run without a socket; the
//! socket path is covered by `socket_roundtrip.rs`.

use arkforge_artifact::fixture;
use arkforge_authority_api::{ControllerPairingSecret, PairingEpoch};
use arkforge_core::digest::sha256;
use arkforge_core::ids::OpaqueId;
use arkforge_core::profile;
use arkforge_engine::BoundToolchain;
use arkforge_ipc::messages::{
    ErrorBody, InspectArtifactResponse, MaterializePlanResponse, Request, Response,
    SubmitStepPermitRequest,
};
use arkforge_ipc::{Api, SessionKind, Status, wire};
use arkforged::{Clock, Service};
use std::path::PathBuf;

const PROFILE_SOURCE: &str = include_str!("../../../profiles/dayu200.yaml");
const CAMPAIGN: &str = include_str!("../../../transcripts/dayu200-gj4-ecamp-96effff15.yaml");

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "arkforged-{}-{}-{:?}",
            name,
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

fn service(root: &TempRoot) -> Service {
    Service::new(
        &root.0.join("store"),
        vec![profile::load(PROFILE_SOURCE).unwrap()],
        vec![CAMPAIGN.to_string()],
        Clock::Fixed(1_754_380_800_000),
        // Not an acceptance campaign. These cases assert the API surface of an
        // ordinary daemon, and one started as a campaign would answer
        // differently about what it can execute.
        None,
    )
    .unwrap()
}

fn request(api: Api, payload: Vec<u8>) -> Request {
    Request {
        request_id: "REQ-1".into(),
        api,
        payload,
    }
}

fn string_payload(field: u32, value: &str) -> Vec<u8> {
    let mut out = Vec::new();
    wire::write_string(&mut out, field, value);
    out
}

fn complete_public_assessment_payload(payload: &mut Vec<u8>) {
    wire::write_string(payload, 4, "fullRestore");
}

/// Imports the fixture archive and returns its artifact id.
fn import(service: &mut Service) -> String {
    let archive = fixture::dayu200_archive();
    let mut payload = Vec::new();
    wire::write_uint64(&mut payload, 1, archive.len() as u64);
    let mut stream = archive.as_slice();
    let response = service.handle(
        SessionKind::Controller,
        &request(Api::ImportArtifact, payload),
        Some(&mut stream),
    );
    assert_eq!(response.status, Status::Ok, "{:?}", decode_error(&response));
    let mut reader = wire::Reader::new(&response.payload);
    let mut artifact_id = String::new();
    while let Some((field, value)) = reader.next_field().unwrap() {
        if field == 1 {
            artifact_id = value.as_str(1).unwrap().to_string();
        }
    }
    artifact_id
}

fn decode_error(response: &Response) -> Option<ErrorBody> {
    ErrorBody::decode(&response.payload).ok()
}

/// A daemon with neither an authority nor a tool reports **both**, so an
/// operator fixing one does not have to discover the second by trying again.
#[test]
fn start_execution_reports_every_standing_blocker_at_once() {
    let root = TempRoot::new("start-controller");
    let mut service = service(&root);
    assert!(!service.readiness().is_ready());

    let response = service.handle(
        SessionKind::Controller,
        &request(Api::StartExecution, Vec::new()),
        None,
    );
    assert_eq!(response.status, Status::Unavailable);
    let error = decode_error(&response).unwrap();
    // The code is the first blocker; the message carries them all.
    assert_eq!(error.code, "NO_PAIRED_AUTHORITY");
    assert!(
        error.message.contains("NO_PAIRED_AUTHORITY"),
        "{}",
        error.message
    );
    assert!(error.message.contains("NO_DISPATCHER"), "{}", error.message);
}

/// Pairing alone is not readiness. A daemon that reported ready here would let
/// a job walk to its first dispatch, spend a permit, and stop with nothing to
/// run it — which has to be reconciled rather than simply not started.
#[test]
fn a_paired_daemon_with_no_dispatcher_still_refuses_and_says_which_half_is_missing() {
    let root = TempRoot::new("paired-no-tool");
    let mut service = service(&root);
    service.pair_authority(ControllerPairingSecret::new(
        PairingEpoch(1),
        b"a-pairing-secret-long-enough-to-be-a-key".to_vec(),
    ));
    assert!(!service.readiness().is_ready());

    let response = service.handle(
        SessionKind::Controller,
        &request(Api::StartExecution, Vec::new()),
        None,
    );
    assert_eq!(response.status, Status::Unavailable);
    let error = decode_error(&response).unwrap();
    assert_eq!(error.code, "NO_DISPATCHER");
    assert!(
        !error.message.contains("NO_PAIRED_AUTHORITY"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("permit would be spent"),
        "the refusal must say why it refuses now rather than later: {}",
        error.message
    );
}

/// Both halves bound: readiness is a standing fact a client can read.
#[test]
fn a_paired_and_native_bound_daemon_is_ready() {
    let root = TempRoot::new("ready");
    let mut service = service(&root);
    service.pair_authority(ControllerPairingSecret::new(
        PairingEpoch(1),
        b"a-pairing-secret-long-enough-to-be-a-key".to_vec(),
    ));
    service.bind_native_dispatcher(BoundToolchain {
        id: OpaqueId::new("arkforged-native-rockusb").unwrap(),
        backend_digest: sha256(b"native arkforged build"),
    });
    assert!(service.readiness().is_ready());
    assert!(service.readiness().standing_blockers().is_empty());

    // Ready is not "any plan will run": an unknown plan is still unknown.
    let response = service.handle(
        SessionKind::Controller,
        &request(Api::StartExecution, Vec::new()),
        None,
    );
    assert_eq!(response.status, Status::InvalidArgument);
}

#[test]
fn start_execution_is_refused_outright_on_the_public_socket() {
    let root = TempRoot::new("start-public");
    let mut service = service(&root);
    let response = service.handle(
        SessionKind::Public,
        &request(Api::StartExecution, Vec::new()),
        None,
    );
    assert_eq!(response.status, Status::Refused);
    assert_eq!(
        decode_error(&response).unwrap().code,
        "SESSION_NOT_PERMITTED"
    );
}

#[test]
fn the_public_socket_cannot_import_an_artifact() {
    let root = TempRoot::new("public-import");
    let mut service = service(&root);
    let archive = fixture::dayu200_archive();
    let mut stream = archive.as_slice();
    let response = service.handle(
        SessionKind::Public,
        &request(Api::ImportArtifact, Vec::new()),
        Some(&mut stream),
    );
    assert_eq!(response.status, Status::Refused);
    assert_eq!(
        decode_error(&response).unwrap().code,
        "SESSION_NOT_PERMITTED"
    );
    // And nothing was stored.
    assert!(
        !root.0.join("store/objects").exists() || {
            std::fs::read_dir(root.0.join("store/objects"))
                .map(|entries| entries.count() == 0)
                .unwrap_or(true)
        }
    );
}

/// Every implemented job surface distinguishes an unknown job from an absent
/// capability.
#[test]
fn an_unknown_job_is_not_found_on_status_and_recovery_surfaces() {
    let root = TempRoot::new("job-surface");
    let mut service = service(&root);

    for api in [
        Api::WatchJob,
        Api::CancelJob,
        Api::GetJob,
        Api::ReconcileJob,
        Api::PlanSupersedingRecovery,
        Api::GetRecoveryGuide,
    ] {
        let response = service.handle(SessionKind::Controller, &request(api, Vec::new()), None);
        assert_eq!(response.status, Status::NotFound, "{api}");
        assert_eq!(
            decode_error(&response).unwrap().code,
            "UNKNOWN_JOB",
            "{api}"
        );
    }
}

#[test]
fn durable_job_status_listing_is_read_only_on_the_public_socket() {
    let root = TempRoot::new("job-list");
    let mut service = service(&root);
    let response = service.handle(
        SessionKind::Public,
        &request(Api::ListJobs, Vec::new()),
        None,
    );
    assert_eq!(response.status, Status::Ok);
    assert!(response.payload.is_empty());
}

/// Answering an admission is minting authority. A public caller that could do
/// it would be an authority nobody paired.
#[test]
fn the_public_socket_cannot_answer_an_admission() {
    let root = TempRoot::new("public-admission");
    let mut service = service(&root);
    for api in [Api::SubmitStepPermit, Api::SubmitManagedControlReceipt] {
        let response = service.handle(SessionKind::Public, &request(api, Vec::new()), None);
        assert_eq!(response.status, Status::Refused, "{api}");
        assert_eq!(
            decode_error(&response).unwrap().code,
            "SESSION_NOT_PERMITTED",
            "{api}"
        );
    }
}

/// Without a paired authority the admission surface refuses too, and names the
/// same standing condition `startExecution` names.
#[test]
fn an_unpaired_daemon_accepts_no_permit() {
    let root = TempRoot::new("unpaired-admission");
    let mut service = service(&root);
    let response = service.handle(
        SessionKind::Controller,
        &request(
            Api::SubmitStepPermit,
            SubmitStepPermitRequest {
                job_id: "JOB-1".into(),
                request_id: "REQ-1".into(),
                permit_cbor: vec![0xA0],
                integrity_tag: vec![0u8; 32],
                pairing_epoch: 1,
                refusal: String::new(),
            }
            .encode(),
        ),
        None,
    );
    assert_eq!(response.status, Status::Unavailable);
    let error = decode_error(&response).unwrap();
    assert_eq!(error.code, "NO_PAIRED_AUTHORITY");
    assert!(
        error.message.contains("no authority is paired"),
        "{}",
        error.message
    );
}

#[test]
fn the_read_only_vertical_runs_over_the_api() {
    let root = TempRoot::new("vertical");
    let mut service = service(&root);

    // import
    let artifact_id = import(&mut service);
    assert_eq!(artifact_id.len(), 64);

    // inspect
    let response = service.handle(
        SessionKind::Public,
        &request(Api::InspectArtifact, string_payload(1, &artifact_id)),
        None,
    );
    assert_eq!(response.status, Status::Ok, "{:?}", decode_error(&response));
    let manifest = InspectArtifactResponse::decode(&response.payload).unwrap();
    assert_eq!(manifest.format_id, "rockchip-images-targz");
    assert_eq!(manifest.members.len(), 17);
    assert_eq!(manifest.partitions.len(), 15);
    assert_eq!(
        manifest
            .partitions
            .iter()
            .find(|entry| entry.name == "userdata")
            .unwrap()
            .size_sectors,
        None,
        "the remainder partition stays a remainder across the wire"
    );
    assert!(
        manifest
            .build_facts
            .iter()
            .any(|fact| fact.key == "const.ohos.fullname")
    );

    // discover
    let response = service.handle(
        SessionKind::Public,
        &request(Api::DiscoverDevices, Vec::new()),
        None,
    );
    assert_eq!(response.status, Status::Ok);
    // Every observation, then the one this vertical is about — not whichever
    // arrived last. Since AD-027 the daemon also enumerates real USB, so what
    // else is on the bus is the developer's business and must not decide
    // whether this case passes. Selecting by id keeps the replay vertical a
    // statement about the transcript.
    let mut observation_ids: Vec<String> = Vec::new();
    let mut reader = wire::Reader::new(&response.payload);
    while let Some((field, value)) = reader.next_field().unwrap() {
        if field == 1 {
            let mut inner = wire::Reader::new(value.as_bytes().unwrap());
            while let Some((inner_field, inner_value)) = inner.next_field().unwrap() {
                if inner_field == 1 {
                    observation_ids.push(inner_value.as_str(1).unwrap().to_string());
                }
            }
        }
    }
    assert!(
        observation_ids.iter().any(|id| id == "OBS-PREFLIGHT"),
        "the transcript's device is missing from {observation_ids:?}"
    );
    let observation_id = "OBS-PREFLIGHT".to_string();

    // probe
    let mut payload = string_payload(1, &observation_id);
    wire::write_string(&mut payload, 2, "org.openharmony.dayu200@1.0.0");
    let response = service.handle(
        SessionKind::Public,
        &request(Api::ProbeDevice, payload),
        None,
    );
    assert_eq!(response.status, Status::Ok, "{:?}", decode_error(&response));

    // materialize — assessment, because AF-V1 is hardware-gated
    let mut payload = string_payload(1, &artifact_id);
    wire::write_string(&mut payload, 2, "org.openharmony.dayu200@1.0.0");
    wire::write_string(&mut payload, 3, &observation_id);
    complete_public_assessment_payload(&mut payload);
    let response = service.handle(
        SessionKind::Public,
        &request(Api::MaterializePlan, payload),
        None,
    );
    assert_eq!(response.status, Status::Ok, "{:?}", decode_error(&response));
    match MaterializePlanResponse::decode(&response.payload).unwrap() {
        MaterializePlanResponse::Assessment(assessment) => {
            assert_eq!(assessment.availability, "unavailable");
            assert!(
                assessment
                    .unknowns
                    .iter()
                    .any(|unknown| unknown.key == "RK-M02"
                        && unknown.value.contains("not published"))
            );
            // The assessment still shows the full data impact, so an operator
            // can see that userdata would be overwritten.
            assert!(
                assessment
                    .data_impact
                    .iter()
                    .any(|impact| impact.key == "userdata" && impact.value == "overwritten")
            );
            assert_eq!(assessment.known_persistent_effects.len(), 9);
        }
        MaterializePlanResponse::Plan(plan) => {
            panic!("AF-V1 must not hand out an executable plan: {plan:?}")
        }
    }
}

#[test]
fn inspecting_an_unknown_artifact_is_not_found_rather_than_internal() {
    let root = TempRoot::new("unknown-artifact");
    let mut service = service(&root);
    let response = service.handle(
        SessionKind::Public,
        &request(Api::InspectArtifact, string_payload(1, &"a".repeat(64))),
        None,
    );
    assert_eq!(response.status, Status::NotFound);
}

#[test]
fn a_malformed_artifact_id_is_an_argument_error() {
    let root = TempRoot::new("bad-id");
    let mut service = service(&root);
    let response = service.handle(
        SessionKind::Public,
        &request(Api::InspectArtifact, string_payload(1, "not-a-digest")),
        None,
    );
    assert_eq!(response.status, Status::InvalidArgument);
}

#[test]
fn an_import_whose_digest_does_not_match_is_refused() {
    let root = TempRoot::new("digest-drift");
    let mut service = service(&root);
    let archive = fixture::dayu200_archive();
    let mut payload = Vec::new();
    wire::write_uint64(&mut payload, 1, archive.len() as u64);
    wire::write_string(&mut payload, 2, &"b".repeat(64));
    let mut stream = archive.as_slice();
    let response = service.handle(
        SessionKind::Controller,
        &request(Api::ImportArtifact, payload),
        Some(&mut stream),
    );
    assert_eq!(response.status, Status::Refused);
    assert!(decode_error(&response).unwrap().message.contains("hash to"));
}

#[test]
fn materialize_requires_an_inspected_artifact() {
    let root = TempRoot::new("materialize-order");
    let mut service = service(&root);
    let artifact_id = import(&mut service);
    let mut payload = string_payload(1, &artifact_id);
    wire::write_string(&mut payload, 2, "org.openharmony.dayu200@1.0.0");
    wire::write_string(&mut payload, 3, "OBS-PREFLIGHT");
    complete_public_assessment_payload(&mut payload);
    let response = service.handle(
        SessionKind::Public,
        &request(Api::MaterializePlan, payload),
        None,
    );
    assert_eq!(response.status, Status::NotFound);
    assert_eq!(
        decode_error(&response).unwrap().code,
        "ARTIFACT_NOT_INSPECTED"
    );
}
