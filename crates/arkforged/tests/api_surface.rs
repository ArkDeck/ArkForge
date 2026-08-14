//! The daemon's API surface.
//!
//! AF-V1 acceptance: "daemon read-only API" and "startExecution disabled".
//! These assertions are about the service, so they run without a socket; the
//! socket path is covered by `socket_roundtrip.rs`.

use arkforge_artifact::fixture;
use arkforge_core::profile;
use arkforged::Service;
use arkforge_ipc::messages::{
    ErrorBody, InspectArtifactResponse, MaterializePlanResponse, Request, Response,
};
use arkforge_ipc::{wire, Api, SessionKind, Status};
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
        1_754_380_800_000,
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

#[test]
fn start_execution_is_unavailable_on_the_controller_socket() {
    let root = TempRoot::new("start-controller");
    let mut service = service(&root);
    let response = service.handle(
        SessionKind::Controller,
        &request(Api::StartExecution, Vec::new()),
        None,
    );
    assert_eq!(response.status, Status::Unavailable);
    let error = decode_error(&response).unwrap();
    assert_eq!(error.code, "EXECUTION_DISABLED");
    // The refusal names what is still missing, so an operator reading it is not
    // sent to look for a stage that has already shipped.
    assert!(
        error.message.contains("no authority is paired"),
        "{}",
        error.message
    );
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
    assert_eq!(decode_error(&response).unwrap().code, "SESSION_NOT_PERMITTED");
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
    assert_eq!(decode_error(&response).unwrap().code, "SESSION_NOT_PERMITTED");
    // And nothing was stored.
    assert!(!root.0.join("store/objects").exists() || {
        std::fs::read_dir(root.0.join("store/objects"))
            .map(|entries| entries.count() == 0)
            .unwrap_or(true)
    });
}

#[test]
fn every_job_surface_call_is_unavailable_in_this_build() {
    let root = TempRoot::new("job-surface");
    let mut service = service(&root);
    for api in [
        Api::WatchJob,
        Api::CancelJob,
        Api::ReconcileJob,
        Api::PlanSupersedingRecovery,
        Api::GetRecoveryGuide,
    ] {
        let response = service.handle(SessionKind::Controller, &request(api, Vec::new()), None);
        assert_eq!(response.status, Status::Unavailable, "{api}");
    }
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
    assert!(manifest
        .build_facts
        .iter()
        .any(|fact| fact.key == "const.ohos.fullname"));

    // discover
    let response = service.handle(
        SessionKind::Public,
        &request(Api::DiscoverDevices, Vec::new()),
        None,
    );
    assert_eq!(response.status, Status::Ok);
    let mut observation_id = String::new();
    let mut reader = wire::Reader::new(&response.payload);
    while let Some((field, value)) = reader.next_field().unwrap() {
        if field == 1 {
            let mut inner = wire::Reader::new(value.as_bytes().unwrap());
            while let Some((inner_field, inner_value)) = inner.next_field().unwrap() {
                if inner_field == 1 {
                    observation_id = inner_value.as_str(1).unwrap().to_string();
                }
            }
        }
    }
    assert_eq!(observation_id, "OBS-PREFLIGHT");

    // probe
    let mut payload = string_payload(1, &observation_id);
    wire::write_string(&mut payload, 2, "org.openharmony.dayu200");
    let response = service.handle(
        SessionKind::Public,
        &request(Api::ProbeDevice, payload),
        None,
    );
    assert_eq!(response.status, Status::Ok, "{:?}", decode_error(&response));

    // materialize — assessment, because AF-V1 is hardware-gated
    let mut payload = string_payload(1, &artifact_id);
    wire::write_string(&mut payload, 2, "org.openharmony.dayu200");
    wire::write_string(&mut payload, 3, &observation_id);
    let response = service.handle(
        SessionKind::Public,
        &request(Api::MaterializePlan, payload),
        None,
    );
    assert_eq!(response.status, Status::Ok, "{:?}", decode_error(&response));
    match MaterializePlanResponse::decode(&response.payload).unwrap() {
        MaterializePlanResponse::Assessment(assessment) => {
            assert_eq!(assessment.availability, "unavailable");
            assert!(assessment
                .unknowns
                .iter()
                .any(|unknown| unknown.value.contains("AF-V2")));
            // The assessment still shows the full data impact, so an operator
            // can see that userdata would be overwritten.
            assert!(assessment
                .data_impact
                .iter()
                .any(|impact| impact.key == "userdata" && impact.value == "overwritten"));
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
    assert!(decode_error(&response)
        .unwrap()
        .message
        .contains("hash to"));
}

#[test]
fn materialize_requires_an_inspected_artifact() {
    let root = TempRoot::new("materialize-order");
    let mut service = service(&root);
    let artifact_id = import(&mut service);
    let mut payload = string_payload(1, &artifact_id);
    wire::write_string(&mut payload, 2, "org.openharmony.dayu200");
    wire::write_string(&mut payload, 3, "OBS-PREFLIGHT");
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
