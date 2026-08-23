//! Protobuf wire vectors for `proto/arkforge.proto` and the decoder's refusal
//! rules. The schema file is the contract; these bytes are what it means.

use crate::json::{Json, hex};
use crate::suites::case_id;
use crate::tree::{Case, Tree};
use arkforge_ipc::messages::{
    ActionReceiptSummary, ErrorBody, Hello, HelloAck, JobEvent, JobEventKind, JobSummary, KeyValue,
    ManagedControlAction, ManagedControlRequest, Request, Response, StepAdmissionSnapshot,
    SubmissionOutcome, SubmitManagedControlReceiptRequest, SubmitStepPermitRequest,
};
use arkforge_ipc::wire::{self, WireError};
use arkforge_ipc::{Api, SessionKind, Status};

const SUITE: &str = "protobuf";

fn wire_error_class(error: &WireError) -> Json {
    let (code, detail) = match error {
        WireError::Truncated => ("PB_TRUNCATED", Json::object(vec![])),
        WireError::VarintTooLong => ("PB_VARINT_TOO_LONG", Json::object(vec![])),
        WireError::UnsupportedWireType(t) => (
            "PB_UNSUPPORTED_WIRE_TYPE",
            Json::object(vec![("wireType", Json::Unsigned(*t as u64))]),
        ),
        WireError::FieldZero => ("PB_FIELD_ZERO", Json::object(vec![])),
        WireError::InvalidUtf8(field) => (
            "PB_INVALID_UTF8",
            Json::object(vec![("field", Json::Unsigned(*field as u64))]),
        ),
        WireError::LengthOverflow => ("PB_LENGTH_OVERFLOW", Json::object(vec![])),
        WireError::DepthLimit => ("PB_DEPTH_LIMIT", Json::object(vec![])),
        WireError::TrailingBytes(n) => (
            "PB_TRAILING_BYTES",
            Json::object(vec![("count", Json::Unsigned(*n as u64))]),
        ),
        WireError::UnknownEnumValue { field, value } => (
            "PB_UNKNOWN_ENUM_VALUE",
            Json::object(vec![
                ("field", Json::Unsigned(*field as u64)),
                ("value", Json::Signed(*value)),
            ]),
        ),
        WireError::FrameTooLarge(n) => (
            "PB_FRAME_TOO_LARGE",
            Json::object(vec![("size", Json::Unsigned(*n as u64))]),
        ),
        WireError::MissingField { message, field } => (
            "PB_MISSING_FIELD",
            Json::object(vec![
                ("message", Json::str(*message)),
                ("field", Json::Unsigned(*field as u64)),
            ]),
        ),
    };
    Json::object(vec![
        ("result", Json::str("reject")),
        ("code", Json::str(code)),
        ("detail", detail),
    ])
}

fn kv(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.into(),
        value: value.into(),
    }
}

pub fn populate(tree: &mut Tree) {
    let mut number = 0u32;

    // ---- encode vectors --------------------------------------------------
    let hello = Hello {
        protocol_major: 1,
        protocol_minor: 0,
        session_kind: SessionKind::Controller,
    };
    let hello_public = Hello {
        protocol_major: 1,
        protocol_minor: 0,
        session_kind: SessionKind::Public,
    };
    let ack = HelloAck {
        protocol_major: 1,
        protocol_minor: 0,
        session_kind: SessionKind::Controller,
        daemon_version: "0.1.0".into(),
        refusal: None,
        execution_ready: false,
        execution_blockers: vec!["NO_PAIRED_AUTHORITY".into(), "NO_DISPATCHER".into()],
        toolchain_id: String::new(),
        toolchain_sha256: String::new(),
    };
    let ack_refused = HelloAck {
        protocol_major: 1,
        protocol_minor: 0,
        session_kind: SessionKind::Public,
        daemon_version: "0.1.0".into(),
        refusal: Some("protocol major 2 is not compatible with 1".into()),
        execution_ready: false,
        execution_blockers: vec![],
        toolchain_id: String::new(),
        toolchain_sha256: String::new(),
    };
    let request = Request {
        request_id: "REQ-1".into(),
        api: Api::DiscoverDevices,
        payload: Vec::new(),
    };
    let response = Response {
        request_id: "REQ-1".into(),
        api: Api::WatchJob,
        status: Status::Ok,
        payload: vec![0xde, 0xad],
        stream_sequence: 7,
        stream_end: true,
    };
    let error = ErrorBody {
        code: "PLAN_DIGEST_MISMATCH".into(),
        message: "plan PLAN-1: caller expects digest a, store holds b".into(),
    };
    let snapshot = StepAdmissionSnapshot {
        job_id: "JOB-1".into(),
        plan_id: "PLAN-1".into(),
        plan_sha256: vec![0x11; 32],
        step_id: "STEP-1".into(),
        attempt_id: "ATTEMPT-1".into(),
        public_step_sha256: vec![0x22; 32],
        private_action_sha256: vec![0x33; 32],
        effect_set_sha256: vec![0x44; 32],
        admitted_device_facts_sha256: vec![0x55; 32],
        observed_mode: "rockusb-loader".into(),
        observed_at_epoch_ms: 1_770_000_000_000,
        snapshot_lifetime_ms: 120_000,
        request_id: "ADM-1".into(),
        topology_sha256: vec![0x66; 32],
        descriptor_sha256: vec![0x77; 32],
        serial_sha256: vec![0x88; 32],
        serial_evidence_kind: "serialAndTopology".into(),
        protocol_identity: vec![kv("rockusb.mode", "Loader"), kv("rockusb.chip", "rk3568")],
        identity_strength: "strong".into(),
        malformed_descriptor: false,
        transport_session_sha256: vec![0x99; 32],
    };
    let permit_submission = SubmitStepPermitRequest {
        job_id: "JOB-1".into(),
        request_id: "ADM-1".into(),
        permit_cbor: vec![0xa0],
        integrity_tag: vec![0xab; 32],
        pairing_epoch: 1,
        refusal: String::new(),
    };
    let permit_refusal = SubmitStepPermitRequest {
        job_id: "JOB-1".into(),
        request_id: "ADM-1".into(),
        permit_cbor: Vec::new(),
        integrity_tag: Vec::new(),
        pairing_epoch: 0,
        refusal: "binding revision moved".into(),
    };
    let outcome_accepted = SubmissionOutcome::accepted();
    let outcome_rejected = SubmissionOutcome::rejected("PERMIT_REJECTED", "stale pairing epoch");
    let control = ManagedControlRequest {
        job_id: "JOB-1".into(),
        step_id: "STEP-1".into(),
        request_id: "CTL-1".into(),
        action: ManagedControlAction::EnterUpdater,
        permit_id: "PERMIT-1".into(),
        expected_facts: vec![],
        deadline_epoch_ms: 1_770_000_060_000,
    };
    let control_receipt = SubmitManagedControlReceiptRequest {
        job_id: "JOB-1".into(),
        request_id: "CTL-1".into(),
        action: ManagedControlAction::ReadBuildFacts,
        accepted: true,
        facts: vec![kv("ohos.build.version", "OpenHarmony-7.0.0.37")],
        evidence_sha256: vec![0xcc; 32],
        failure_reason: String::new(),
    };
    let receipt = ActionReceiptSummary {
        job_id: "JOB-1".into(),
        plan_id: "PLAN-1".into(),
        step_id: "STEP-5".into(),
        action_id: "ACT-5".into(),
        attempt_id: "ATTEMPT-1".into(),
        permit_id: "PERMIT-5".into(),
        disposition: "semanticSuccess".into(),
        evidence_sha256: vec![0xdd; 32],
        verification_outcome: "typedSkip".into(),
        verification_strength: String::new(),
        verified_range_start: 0,
        verified_range_length: 0,
        typed_skip_reason: "skipped-lba-read-window".into(),
        failure_classification: String::new(),
        facts: vec![kv("writePayloadSha256", "ee")],
    };
    let summary = JobSummary {
        job_id: "JOB-1".into(),
        plan_id: "PLAN-1".into(),
        plan_sha256: vec![0x11; 32],
        state: "outcomeUnknown".into(),
        terminal: false,
        current_step_id: "STEP-5".into(),
        completed_steps: 4,
        total_steps: 13,
        last_sequence: 42,
        stopped_reason: String::new(),
    };
    let event = JobEvent {
        job_id: "JOB-1".into(),
        sequence: 9,
        kind: JobEventKind::StepAdmissionRequested,
        at_epoch_ms: 1_770_000_000_500,
        journal_record_sha256: vec![0xee; 32],
        job_state: String::new(),
        admission: Some(snapshot.clone()),
        control_request: None,
        receipt: None,
        facts: vec![],
    };

    struct Vector {
        title: &'static str,
        message: &'static str,
        bytes: Vec<u8>,
        round_trip: bool,
    }
    let vectors = vec![
        Vector {
            title: "Hello (controller)",
            message: "Hello",
            bytes: hello.encode(),
            round_trip: Hello::decode(&hello.encode()).unwrap() == hello,
        },
        Vector {
            title: "Hello (public)",
            message: "Hello",
            bytes: hello_public.encode(),
            round_trip: Hello::decode(&hello_public.encode()).unwrap() == hello_public,
        },
        Vector {
            title: "HelloAck with two execution blockers",
            message: "HelloAck",
            bytes: ack.encode(),
            round_trip: HelloAck::decode(&ack.encode()).unwrap() == ack,
        },
        Vector {
            title: "HelloAck refusing the handshake",
            message: "HelloAck",
            bytes: ack_refused.encode(),
            round_trip: HelloAck::decode(&ack_refused.encode()).unwrap() == ack_refused,
        },
        Vector {
            title: "Request with empty payload (field 3 omitted)",
            message: "Request",
            bytes: request.encode(),
            round_trip: Request::decode(&request.encode()).unwrap() == request,
        },
        Vector {
            title: "Response on a stream",
            message: "Response",
            bytes: response.encode(),
            round_trip: Response::decode(&response.encode()).unwrap() == response,
        },
        Vector {
            title: "Error body",
            message: "Error",
            bytes: error.encode(),
            round_trip: ErrorBody::decode(&error.encode()).unwrap() == error,
        },
        Vector {
            title: "StepAdmissionSnapshot (all fields)",
            message: "StepAdmissionSnapshot",
            bytes: snapshot.encode(),
            round_trip: StepAdmissionSnapshot::decode(&snapshot.encode()).unwrap() == snapshot,
        },
        Vector {
            title: "SubmitStepPermitRequest carrying a permit",
            message: "SubmitStepPermitRequest",
            bytes: permit_submission.encode(),
            round_trip: SubmitStepPermitRequest::decode(&permit_submission.encode()).unwrap()
                == permit_submission,
        },
        Vector {
            title: "SubmitStepPermitRequest carrying a refusal",
            message: "SubmitStepPermitRequest",
            bytes: permit_refusal.encode(),
            round_trip: SubmitStepPermitRequest::decode(&permit_refusal.encode()).unwrap()
                == permit_refusal,
        },
        Vector {
            title: "SubmitStepPermitResponse accepted (only field 1)",
            message: "SubmitStepPermitResponse",
            bytes: outcome_accepted.encode(),
            round_trip: SubmissionOutcome::decode(&outcome_accepted.encode()).unwrap()
                == outcome_accepted,
        },
        Vector {
            title: "SubmitStepPermitResponse rejected (accepted=false is omitted)",
            message: "SubmitStepPermitResponse",
            bytes: outcome_rejected.encode(),
            round_trip: SubmissionOutcome::decode(&outcome_rejected.encode()).unwrap()
                == outcome_rejected,
        },
        Vector {
            title: "ManagedControlRequest enter-updater",
            message: "ManagedControlRequest",
            bytes: control.encode(),
            round_trip: ManagedControlRequest::decode(&control.encode()).unwrap() == control,
        },
        Vector {
            title: "SubmitManagedControlReceiptRequest with a build fact",
            message: "SubmitManagedControlReceiptRequest",
            bytes: control_receipt.encode(),
            round_trip: SubmitManagedControlReceiptRequest::decode(&control_receipt.encode())
                .unwrap()
                == control_receipt,
        },
        Vector {
            title: "ActionReceiptSummary with a typed skip",
            message: "ActionReceiptSummary",
            bytes: receipt.encode(),
            round_trip: ActionReceiptSummary::decode(&receipt.encode()).unwrap() == receipt,
        },
        Vector {
            title: "JobSummary",
            message: "JobSummary",
            bytes: summary.encode(),
            round_trip: JobSummary::decode(&summary.encode()).unwrap() == summary,
        },
        Vector {
            title: "JobEvent carrying an admission snapshot",
            message: "JobEvent",
            bytes: event.encode(),
            round_trip: JobEvent::decode(&event.encode()).unwrap() == event,
        },
    ];
    for vector in vectors {
        number += 1;
        assert!(vector.round_trip, "{} must round-trip", vector.title);
        tree.case(
            &Case {
                id: case_id("PB", number),
                suite: SUITE,
                title: vector.title.to_string(),
                requirements: vec!["AF-IPC-010", "AF-IPC-011"],
                kind: "encode",
                description: format!(
                    "Encoding of `arkforge.ipc.v1.{}` per proto/arkforge.proto. Proto3 \
                     defaults (0, false, empty) are omitted; fields are written in field \
                     number order; repeated message fields are one tag per element.",
                    vector.message
                ),
                input: Json::object(vec![("message", Json::str(vector.message))]),
                expected: Json::object(vec![
                    ("hex", Json::str(hex(&vector.bytes))),
                    ("roundTrip", Json::Bool(true)),
                ]),
            },
            vec![("message.pb", vector.bytes)],
        );
    }

    // ---- enum wire values ---------------------------------------------------
    number += 1;
    tree.case(
        &Case {
            id: case_id("PB", number),
            suite: SUITE,
            title: "enum wire values and their text spellings".to_string(),
            requirements: vec!["AF-IPC-012"],
            kind: "table",
            description: "Zero is UNSPECIFIED for every enum and is never accepted on a \
                          required enum field. Any other unknown value is a hard error."
                .to_string(),
            input: Json::object(vec![]),
            expected: Json::object(vec![
                (
                    "SessionKind",
                    Json::Array(
                        [SessionKind::Public, SessionKind::Controller]
                            .iter()
                            .map(|k| {
                                Json::object(vec![
                                    ("wire", Json::Unsigned(k.wire_value() as u64)),
                                    (
                                        "text",
                                        Json::str(match k {
                                            SessionKind::Public => "public",
                                            SessionKind::Controller => "controller",
                                        }),
                                    ),
                                ])
                            })
                            .collect(),
                    ),
                ),
                (
                    "Api",
                    Json::Array(
                        Api::ALL
                            .iter()
                            .map(|a| {
                                Json::object(vec![
                                    ("wire", Json::Unsigned(a.wire_value() as u64)),
                                    ("text", Json::str(a.as_str())),
                                ])
                            })
                            .collect(),
                    ),
                ),
                (
                    "Status",
                    Json::Array(
                        [
                            Status::Ok,
                            Status::Refused,
                            Status::Unavailable,
                            Status::InvalidArgument,
                            Status::NotFound,
                            Status::Internal,
                        ]
                        .iter()
                        .map(|s| {
                            Json::object(vec![
                                ("wire", Json::Unsigned(s.wire_value() as u64)),
                                ("text", Json::str(s.as_str())),
                            ])
                        })
                        .collect(),
                    ),
                ),
                (
                    "JobEventKind",
                    Json::Array(
                        JobEventKind::ALL
                            .iter()
                            .map(|k| {
                                Json::object(vec![("wire", Json::Unsigned(k.wire_value() as u64))])
                            })
                            .collect(),
                    ),
                ),
                (
                    "ManagedControlAction",
                    Json::Array(
                        ManagedControlAction::ALL
                            .iter()
                            .map(|a| {
                                Json::object(vec![
                                    ("wire", Json::Unsigned(a.wire_value() as u64)),
                                    ("text", Json::str(a.as_str())),
                                ])
                            })
                            .collect(),
                    ),
                ),
                (
                    "publicSessionMayCall",
                    Json::strs(
                        Api::ALL
                            .iter()
                            .filter(|a| SessionKind::Public.may_call(**a))
                            .map(|a| a.as_str()),
                    ),
                ),
                (
                    "controllerOnly",
                    Json::strs(
                        Api::ALL
                            .iter()
                            .filter(|a| !SessionKind::Public.may_call(**a))
                            .map(|a| a.as_str()),
                    ),
                ),
                (
                    "forbiddenControlReceiptFacts",
                    Json::strs(arkforge_ipc::messages::FORBIDDEN_CONTROL_RECEIPT_FACTS),
                ),
            ]),
        },
        Vec::new(),
    );

    // ---- decoder rules --------------------------------------------------------
    struct DecodeCase {
        title: &'static str,
        requirement: &'static str,
        bytes: Vec<u8>,
    }
    let mut decode_cases: Vec<DecodeCase> = Vec::new();
    {
        let mut with_unknown = hello.encode();
        wire::write_string(&mut with_unknown, 9, "future field");
        decode_cases.push(DecodeCase {
            title: "an unknown field is skipped (forward compatibility)",
            requirement: "AF-IPC-013",
            bytes: with_unknown,
        });
    }
    {
        let mut out = Vec::new();
        wire::write_uint32(&mut out, 1, 1);
        wire::write_enum(&mut out, 3, 7);
        decode_cases.push(DecodeCase {
            title: "an unknown enum value is a hard error, not a default",
            requirement: "AF-IPC-014",
            bytes: out,
        });
    }
    {
        let mut out = Vec::new();
        wire::write_uint32(&mut out, 1, 1);
        wire::write_uint32(&mut out, 2, 0);
        decode_cases.push(DecodeCase {
            title: "a required enum field that is absent (zero) is refused",
            requirement: "AF-IPC-014",
            bytes: out,
        });
    }
    {
        let mut out = Vec::new();
        wire::write_uint32(&mut out, 1, 1);
        wire::write_enum(&mut out, 3, 0);
        decode_cases.push(DecodeCase {
            title: "explicit zero for a required enum is the same as absent",
            requirement: "AF-IPC-014",
            bytes: out,
        });
    }
    decode_cases.push(DecodeCase {
        title: "field number 0 is refused",
        requirement: "AF-IPC-015",
        bytes: vec![0x00, 0x01],
    });
    decode_cases.push(DecodeCase {
        title: "start-group wire type (3) is not in the subset",
        requirement: "AF-IPC-015",
        bytes: vec![0x0b],
    });
    decode_cases.push(DecodeCase {
        title: "end-group wire type (4) is not in the subset",
        requirement: "AF-IPC-015",
        bytes: vec![0x0c],
    });
    decode_cases.push(DecodeCase {
        title: "varint longer than 10 bytes",
        requirement: "AF-IPC-015",
        bytes: vec![
            0x08, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01,
        ],
    });
    decode_cases.push(DecodeCase {
        title: "length-delimited field overruns the message",
        requirement: "AF-IPC-015",
        bytes: vec![0x1a, 0x05, 0x01, 0x02],
    });
    decode_cases.push(DecodeCase {
        title: "message ends mid-varint",
        requirement: "AF-IPC-015",
        bytes: vec![0x08, 0x80],
    });
    decode_cases.push(DecodeCase {
        title: "a string field with invalid UTF-8 is refused",
        requirement: "AF-IPC-016",
        bytes: {
            let mut out = Vec::new();
            wire::write_bytes(&mut out, 1, &[0xc3, 0x28]);
            wire::write_enum(&mut out, 2, 3);
            out
        },
    });
    {
        // Hello decoded as Request: field 1 arrives as a varint where a string
        // is expected — a wire-type mismatch is refused, not coerced.
        decode_cases.push(DecodeCase {
            title: "wire-type mismatch (varint where a string is expected) is refused",
            requirement: "AF-IPC-015",
            bytes: hello.encode(),
        });
    }
    for case in decode_cases {
        number += 1;
        // Decode as Hello unless the title says Request/string; the Hello and
        // Request decoders share the reader, so the class is the same.
        let outcome: Result<Json, WireError> =
            if case.requirement == "AF-IPC-016" || case.title.starts_with("wire-type") {
                Request::decode(&case.bytes).map(|r| {
                    Json::object(vec![
                        ("result", Json::str("accept")),
                        ("requestId", Json::str(r.request_id)),
                        ("api", Json::str(r.api.as_str())),
                    ])
                })
            } else {
                Hello::decode(&case.bytes).map(|h| {
                    Json::object(vec![
                        ("result", Json::str("accept")),
                        ("protocolMajor", Json::Unsigned(h.protocol_major as u64)),
                        ("protocolMinor", Json::Unsigned(h.protocol_minor as u64)),
                        (
                            "sessionKind",
                            Json::Unsigned(h.session_kind.wire_value() as u64),
                        ),
                    ])
                })
            };
        let expected = match outcome {
            Ok(json) => json,
            Err(error) => wire_error_class(&error),
        };
        let decoder = if case.requirement == "AF-IPC-016" || case.title.starts_with("wire-type") {
            "Request"
        } else {
            "Hello"
        };
        tree.case(
            &Case {
                id: case_id("PB", number),
                suite: SUITE,
                title: case.title.to_string(),
                requirements: vec![case.requirement],
                kind: "decode",
                description: format!("Decode the bytes as `{decoder}`."),
                input: Json::object(vec![
                    ("decoder", Json::str(decoder)),
                    ("hex", Json::str(hex(&case.bytes))),
                ]),
                expected,
            },
            Vec::new(),
        );
    }

    // ---- framing ---------------------------------------------------------------
    number += 1;
    let mut frame = Vec::new();
    arkforge_ipc::framing::write_frame(&mut frame, &hello.encode()).unwrap();
    tree.case(
        &Case {
            id: case_id("PB", number),
            suite: SUITE,
            title: "frame = u32 big-endian length + message".to_string(),
            requirements: vec!["AF-IPC-001", "AF-IPC-002"],
            kind: "encode",
            description: format!(
                "A frame is a 4-byte big-endian length followed by that many bytes. The \
                 length is checked against the {}-byte bound before any allocation. A \
                 clean end of stream between frames is not an error; a cut inside a \
                 frame is.",
                wire::MAX_FRAME_BYTES
            ),
            input: Json::object(vec![("message", Json::str(case_id("PB", 1)))]),
            expected: Json::object(vec![
                ("hex", Json::str(hex(&frame))),
                (
                    "maxFrameBytes",
                    Json::Unsigned(wire::MAX_FRAME_BYTES as u64),
                ),
                (
                    "protocolMajor",
                    Json::Unsigned(arkforge_ipc::PROTOCOL_MAJOR as u64),
                ),
                (
                    "protocolMinor",
                    Json::Unsigned(arkforge_ipc::PROTOCOL_MINOR as u64),
                ),
                (
                    "negotiation",
                    Json::Array(
                        [(1u32, 0u32), (1, 7), (2, 0), (0, 0)]
                            .iter()
                            .map(|(major, minor)| {
                                Json::object(vec![
                                    ("peerMajor", Json::Unsigned(*major as u64)),
                                    ("peerMinor", Json::Unsigned(*minor as u64)),
                                    (
                                        "accepted",
                                        Json::Bool(arkforge_ipc::negotiate(*major, *minor).is_ok()),
                                    ),
                                ])
                            })
                            .collect(),
                    ),
                ),
            ]),
        },
        vec![("frame.bin", frame)],
    );
}
