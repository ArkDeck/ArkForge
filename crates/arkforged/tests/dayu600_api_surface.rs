//! DAYU600 over the same API as DAYU200.
//!
//! AF-V3's goal is that "ArkForge/ArkDeck 使用相同 API 对 DAYU600 完成可信研究"
//! — one client, one API, two devices, different answers. These tests drive the
//! DAYU600 path through the identical request surface and assert that the
//! answers are the honest ones.

use arkforge_core::profile;
use arkforge_ipc::messages::{
    ErrorBody, InspectArtifactResponse, MaterializePlanResponse, Request, Response,
};
use arkforge_ipc::{Api, SessionKind, Status, wire};
use arkforged::{Clock, Service};
use std::path::PathBuf;

const DAYU600_PROFILE: &str = include_str!("../../../profiles/dayu600.yaml");
const DAYU200_PROFILE: &str = include_str!("../../../profiles/dayu200.yaml");
const DAYU600_TRANSCRIPT: &str =
    include_str!("../../../transcripts/dayu600-research-synthetic.yaml");

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "arkforged-600-{}-{}-{:?}",
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
        vec![
            profile::load(DAYU200_PROFILE).unwrap(),
            profile::load(DAYU600_PROFILE).unwrap(),
        ],
        vec![DAYU600_TRANSCRIPT.to_string()],
        Clock::Fixed(1_754_380_800_000),
        None,
    )
    .unwrap()
}

fn request(api: Api, payload: Vec<u8>) -> Request {
    Request {
        request_id: "REQ-600".into(),
        api,
        payload,
    }
}

fn decode_error(response: &Response) -> Option<ErrorBody> {
    ErrorBody::decode(&response.payload).ok()
}

fn complete_public_assessment_payload(payload: &mut Vec<u8>) {
    wire::write_string(payload, 4, "fullRestore");
}

/// A container shaped like a firmware package. Not a PAC file.
fn synthetic_container() -> Vec<u8> {
    let mut bytes = b"BP_R1.0.0".to_vec();
    bytes.extend_from_slice(&[0u8; 7]);
    for index in 0..10u8 {
        let start = bytes.len();
        bytes.push(0x02);
        bytes.push(index);
        for character in format!("IMG_{index}").chars() {
            bytes.push(character as u8);
            bytes.push(0);
        }
        while bytes.len() - start < 32 {
            bytes.push(0);
        }
    }
    bytes.extend(arkforge_artifact::fixture::fixture_body("dayu600", 9_000));
    bytes.extend_from_slice(&[0xffu8; 1024]);
    bytes
}

fn import(service: &mut Service, bytes: &[u8]) -> String {
    let mut payload = Vec::new();
    wire::write_uint64(&mut payload, 1, bytes.len() as u64);
    let mut stream = bytes;
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

#[test]
fn the_dayu600_research_vertical_runs_over_the_same_api() {
    let root = TempRoot::new("vertical");
    let mut service = service(&root);
    let container = synthetic_container();

    // import — the same call as DAYU200
    let artifact_id = import(&mut service, &container);

    // inspect — the container's framing selects the observer, and the answer
    // says ResearchOnly
    let mut payload = Vec::new();
    wire::write_string(&mut payload, 1, &artifact_id);
    let response = service.handle(
        SessionKind::Public,
        &request(Api::InspectArtifact, payload),
        None,
    );
    assert_eq!(response.status, Status::Ok, "{:?}", decode_error(&response));
    let manifest = InspectArtifactResponse::decode(&response.payload).unwrap();
    assert_eq!(manifest.format_id, "unisoc-pac");
    assert_eq!(manifest.confidence, "researchOnly");
    assert!(
        manifest.partitions.is_empty(),
        "this observer has no basis for a partition table"
    );
    assert_eq!(
        manifest.execution_relevant_unknowns.len(),
        12,
        "the exact unknown list crosses the wire"
    );
    assert!(
        manifest
            .execution_relevant_unknowns
            .iter()
            .any(|unknown| unknown.key == "UNI-U01")
    );

    // discover / probe
    let response = service.handle(
        SessionKind::Public,
        &request(Api::DiscoverDevices, Vec::new()),
        None,
    );
    assert_eq!(response.status, Status::Ok);

    let mut payload = Vec::new();
    wire::write_string(&mut payload, 1, "OBS-DAYU600-NORMAL");
    wire::write_string(&mut payload, 2, "org.openharmony.dayu600");
    let response = service.handle(
        SessionKind::Public,
        &request(Api::ProbeDevice, payload),
        None,
    );
    assert_eq!(response.status, Status::Ok, "{:?}", decode_error(&response));

    // materialize — an assessment, with the evidence requirements attached
    let mut payload = Vec::new();
    wire::write_string(&mut payload, 1, &artifact_id);
    wire::write_string(&mut payload, 2, "org.openharmony.dayu600");
    wire::write_string(&mut payload, 3, "OBS-DAYU600-NORMAL");
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
            assert!(assessment.unavailable_reason.contains("17.5"));
            assert!(assessment.known_persistent_effects.is_empty());
            // Every data-impact axis reads unknown — not "preserved", which
            // would claim nothing is touched.
            for impact in &assessment.data_impact {
                assert_eq!(impact.value, "unknown", "{}", impact.key);
            }
            assert!(!assessment.evidence_requirements.is_empty());
            let ids: Vec<&str> = assessment
                .unknowns
                .iter()
                .map(|unknown| unknown.key.as_str())
                .collect();
            assert!(ids.contains(&"UNI-G01"), "{ids:?}");
        }
        MaterializePlanResponse::Plan(plan) => {
            panic!("DAYU600 must never produce an executable plan: {plan:?}")
        }
    }
}

#[test]
fn start_execution_is_unavailable_for_dayu600_at_every_layer() {
    // AF-V3 acceptance: "startExecution 无 bypass". There is no DAYU600-shaped
    // request that reaches execution, because there is no plan id to reach it
    // with — an assessment has no field to carry one.
    let root = TempRoot::new("no-bypass");
    let mut service = service(&root);
    let artifact_id = import(&mut service, &synthetic_container());

    let mut payload = Vec::new();
    wire::write_string(&mut payload, 1, &artifact_id);
    let inspected = service.handle(
        SessionKind::Public,
        &request(Api::InspectArtifact, payload),
        None,
    );
    assert_eq!(inspected.status, Status::Ok);

    // Try every shape a caller might reach for.
    for payload in [
        Vec::new(),
        {
            let mut out = Vec::new();
            wire::write_string(&mut out, 1, "PLAN-DAYU600");
            out
        },
        {
            let mut out = Vec::new();
            wire::write_string(&mut out, 1, &artifact_id);
            wire::write_string(&mut out, 2, &"0".repeat(64));
            wire::write_string(&mut out, 3, "primaryFlash");
            out
        },
    ] {
        let response = service.handle(
            SessionKind::Controller,
            &request(Api::StartExecution, payload),
            None,
        );
        assert_eq!(response.status, Status::Unavailable);
        // The daemon has neither an authority nor a tool, so it refuses on the
        // standing facts before it ever looks at a DAYU600 plan. That the
        // refusal is not DAYU600-specific is the point: no payload reaches a
        // path that could produce an executable plan for it.
        assert_eq!(decode_error(&response).unwrap().code, "NO_PAIRED_AUTHORITY");
    }
}

#[test]
fn a_pac_container_offered_against_the_dayu200_profile_is_refused() {
    // Wrong device through the API: the container is observed as `unisoc-pac`,
    // the profile accepts only `rockchip-images-targz`.
    let root = TempRoot::new("wrong-profile");
    let mut service = service(&root);
    let artifact_id = import(&mut service, &synthetic_container());

    let mut payload = Vec::new();
    wire::write_string(&mut payload, 1, &artifact_id);
    let inspected = service.handle(
        SessionKind::Public,
        &request(Api::InspectArtifact, payload),
        None,
    );
    assert_eq!(inspected.status, Status::Ok);

    let mut payload = Vec::new();
    wire::write_string(&mut payload, 1, &artifact_id);
    wire::write_string(&mut payload, 2, "org.openharmony.dayu200");
    wire::write_string(&mut payload, 3, "OBS-DAYU600-NORMAL");
    complete_public_assessment_payload(&mut payload);
    let response = service.handle(
        SessionKind::Public,
        &request(Api::MaterializePlan, payload),
        None,
    );
    // The DAYU200 transcript is not loaded in this service, so the observation
    // belongs to the DAYU600 recording; either way nothing executable comes
    // back.
    match response.status {
        Status::Ok => match MaterializePlanResponse::decode(&response.payload).unwrap() {
            MaterializePlanResponse::Assessment(assessment) => {
                assert_eq!(assessment.availability, "unavailable");
            }
            MaterializePlanResponse::Plan(plan) => {
                panic!("a PAC container must not produce a Rockchip plan: {plan:?}")
            }
        },
        Status::NotFound | Status::Refused => {}
        other => panic!("unexpected status {other:?}"),
    }
}

#[test]
fn a_dayu200_archive_and_a_pac_container_get_different_observers() {
    let root = TempRoot::new("two-formats");
    let mut service = service(&root);

    let rockchip_id = import(&mut service, &arkforge_artifact::fixture::dayu200_archive());
    let pac_id = import(&mut service, &synthetic_container());
    assert_ne!(rockchip_id, pac_id);

    let inspect = |service: &mut Service, id: &str| {
        let mut payload = Vec::new();
        wire::write_string(&mut payload, 1, id);
        let response = service.handle(
            SessionKind::Public,
            &request(Api::InspectArtifact, payload),
            None,
        );
        assert_eq!(response.status, Status::Ok);
        InspectArtifactResponse::decode(&response.payload).unwrap()
    };

    let rockchip = inspect(&mut service, &rockchip_id);
    assert_eq!(rockchip.format_id, "rockchip-images-targz");
    assert_eq!(rockchip.partitions.len(), 15);

    let pac = inspect(&mut service, &pac_id);
    assert_eq!(pac.format_id, "unisoc-pac");
    assert!(pac.partitions.is_empty());
}
