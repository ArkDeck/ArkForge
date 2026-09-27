use arkforge_ipc::messages::{
    Assessment, ErrorBody, Hello, ImportArtifactRequest, ImportArtifactResponse, JobEvent,
    JobEventKind, KeyValue, MaterializePlanResponse, Request, Response,
    SubmitManagedControlReceiptRequest, SubmitStepPermitRequest,
};
use arkforge_ipc::{Api, SessionKind, Status};

fn bytes(hex: &str) -> Vec<u8> {
    assert!(hex.len().is_multiple_of(2));
    hex.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let text = std::str::from_utf8(pair).expect("ASCII hex");
            u8::from_str_radix(text, 16).expect("valid hex")
        })
        .collect()
}

#[test]
fn handshake_and_request_match_the_swift_sdk() {
    let hello = Hello {
        protocol_major: 1,
        protocol_minor: 0,
        session_kind: SessionKind::Controller,
    };
    assert_eq!(hello.encode(), bytes("08011802"));
    assert_eq!(Hello::decode(&hello.encode()).unwrap(), hello);

    let request = Request {
        request_id: "REQ-1".into(),
        api: Api::DiscoverDevices,
        payload: Vec::new(),
    };
    assert_eq!(request.encode(), bytes("0a055245512d311003"));
    assert_eq!(Request::decode(&request.encode()).unwrap(), request);
}

#[test]
fn import_header_and_response_use_the_existing_swift_field_layout() {
    assert_eq!(
        ImportArtifactRequest {
            expected_size_bytes: 300,
            expected_sha256: "D".into(),
        }
        .encode(),
        bytes("08ac02120144")
    );
    assert!(ImportArtifactRequest::default().encode().is_empty());
    let expected = ImportArtifactResponse {
        artifact_id: "A".into(),
        sha256: "D".into(),
        size_bytes: 300,
        deduplicated: true,
    };
    assert_eq!(
        ImportArtifactResponse::decode(&bytes("0a014112014418ac022001")).unwrap(),
        expected
    );
    // An unknown field remains skippable; a known field with the wrong wire
    // type and a truncated length remain errors under the existing codec.
    assert_eq!(
        ImportArtifactResponse::decode(&bytes("0a014112014418ac022001980601")).unwrap(),
        expected
    );
    assert!(ImportArtifactResponse::decode(&bytes("0801")).is_err());
    assert!(ImportArtifactResponse::decode(&bytes("0a0541")).is_err());
}

#[test]
fn materialize_assessment_support_fields_match_the_swift_sdk() {
    let response = MaterializePlanResponse::Assessment(Assessment {
        availability: "unavailable".into(),
        unavailable_reason: "public".into(),
        mechanics_maturity_key_sha256: "mechanics".into(),
        mechanics_maturity_state: "hardwareCampaign".into(),
        authority_support_key_sha256: "support".into(),
        authority_support_state: "hardwareGated".into(),
        ..Assessment::default()
    });
    assert_eq!(
        response.encode(),
        bytes(
            "124a2a0b756e617661696c61626c6532067075626c696342096d656368616e696373\
             4a10686172647761726543616d706169676e5207737570706f7274\
             5a0d68617264776172654761746564"
        )
    );
    assert_eq!(
        MaterializePlanResponse::decode(&response.encode()).unwrap(),
        response
    );
}

#[test]
fn permit_and_managed_receipt_match_the_swift_sdk() {
    let permit = SubmitStepPermitRequest {
        job_id: "J".into(),
        request_id: "R".into(),
        permit_cbor: bytes("a1616101"),
        integrity_tag: vec![0xab, 0xcd],
        pairing_epoch: 7,
        refusal: String::new(),
    };
    assert_eq!(
        permit.encode(),
        bytes("0a014a1201521a04a16161012202abcd2807")
    );
    assert_eq!(
        SubmitStepPermitRequest::decode(&permit.encode()).unwrap(),
        permit
    );

    let receipt = SubmitManagedControlReceiptRequest {
        job_id: "J".into(),
        request_id: "R".into(),
        action: arkforge_ipc::messages::ManagedControlAction::ReadBuildFacts,
        accepted: true,
        facts: vec![KeyValue {
            key: "build".into(),
            value: "1".into(),
        }],
        evidence_sha256: vec![0x01, 0x02],
        failure_reason: String::new(),
    };
    assert_eq!(
        receipt.encode(),
        bytes("0a014a120152180420012a0a0a056275696c6412013132020102")
    );
    assert_eq!(
        SubmitManagedControlReceiptRequest::decode(&receipt.encode()).unwrap(),
        receipt
    );
}

#[test]
fn event_and_error_match_the_swift_sdk() {
    let event = JobEvent {
        job_id: "J".into(),
        sequence: 2,
        kind: JobEventKind::StateChanged,
        at_epoch_ms: 3,
        journal_record_sha256: vec![0x01],
        job_state: "running".into(),
        ..JobEvent::default()
    };
    assert_eq!(
        event.encode(),
        bytes("0a014a1002180120032a0101320772756e6e696e67")
    );
    assert_eq!(JobEvent::decode(&event.encode()).unwrap(), event);

    let error = ErrorBody {
        code: "E".into(),
        message: "no".into(),
    };
    let response = Response {
        request_id: "R".into(),
        api: Api::StartExecution,
        status: Status::Refused,
        payload: error.encode(),
        stream_sequence: 0,
        stream_end: true,
    };
    assert_eq!(
        response.encode(),
        bytes("0a01521006180222070a014512026e6f3001")
    );
    assert_eq!(Response::decode(&response.encode()).unwrap(), response);
    assert_eq!(ErrorBody::decode(&response.payload).unwrap(), error);
}
