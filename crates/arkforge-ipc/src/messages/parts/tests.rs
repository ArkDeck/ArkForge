use super::*;

#[test]
fn hello_round_trips() {
    let hello = Hello {
        protocol_major: 1,
        protocol_minor: 0,
        session_kind: SessionKind::Controller,
    };
    assert_eq!(Hello::decode(&hello.encode()).unwrap(), hello);
}

#[test]
fn a_hello_with_no_session_kind_is_refused() {
    // SESSION_KIND_UNSPECIFIED is the proto3 zero, so it is absent on the
    // wire. Reading it as "public" would hand an unidentified peer the
    // read-only surface.
    let empty: Vec<u8> = Vec::new();
    assert!(matches!(
        Hello::decode(&empty),
        Err(WireError::MissingField {
            message: "Hello",
            field: 3
        })
    ));
}

#[test]
fn a_hello_with_an_unknown_session_kind_is_refused() {
    let mut out = Vec::new();
    wire::write_uint32(&mut out, 1, 1);
    wire::write_enum(&mut out, 3, 42);
    assert!(matches!(
        Hello::decode(&out),
        Err(WireError::UnknownEnumValue { field: 3, .. })
    ));
}

#[test]
fn request_and_response_round_trip() {
    let request = Request {
        request_id: "REQ-1".into(),
        api: Api::MaterializePlan,
        payload: vec![1, 2, 3],
    };
    assert_eq!(Request::decode(&request.encode()).unwrap(), request);

    let response = Response {
        request_id: "REQ-1".into(),
        api: Api::StartExecution,
        status: Status::Unavailable,
        payload: vec![9],
        stream_sequence: 3,
        stream_end: true,
    };
    assert_eq!(Response::decode(&response.encode()).unwrap(), response);
}

#[test]
fn a_response_with_an_unknown_status_is_refused() {
    let mut out = Vec::new();
    wire::write_string(&mut out, 1, "REQ-1");
    wire::write_enum(&mut out, 2, Api::InspectArtifact.wire_value());
    wire::write_enum(&mut out, 3, 77);
    assert!(matches!(
        Response::decode(&out),
        Err(WireError::UnknownEnumValue { field: 3, .. })
    ));
}

#[test]
fn inspect_response_round_trips_with_a_remainder_partition() {
    let response = InspectArtifactResponse {
        format_id: "example-images-targz".into(),
        content_sha256: "a".repeat(64),
        size_bytes: 730_769_584,
        members: vec![ArchiveMember {
            path: "system.img".into(),
            size_bytes: 2_147_483_648,
            sha256: "b".repeat(64),
            role: "imageCandidate".into(),
        }],
        partitions: vec![
            PartitionEntry {
                index: 0,
                name: "uboot".into(),
                offset_sectors: 8192,
                size_sectors: Some(8192),
                attribute: String::new(),
                grammar_branch: "fixed".into(),
            },
            PartitionEntry {
                index: 14,
                name: "userdata".into(),
                offset_sectors: 19_955_712,
                size_sectors: None,
                attribute: "grow".into(),
                grammar_branch: "remainderGrow".into(),
            },
        ],
        build_facts: vec![KeyValue {
            key: "const.ohos.fullname".into(),
            value: "example-build-1.0.0".into(),
        }],
        unclassified_members: vec!["updater_binary".into()],
        execution_relevant_unknowns: vec![],
        confidence: "researchOnly".into(),
        manifest_sha256: "c".repeat(64),
    };
    let decoded = InspectArtifactResponse::decode(&response.encode()).unwrap();
    assert_eq!(decoded, response);
    // The remainder stays a remainder rather than becoming zero sectors.
    assert_eq!(decoded.partitions[1].size_sectors, None);
}

#[test]
fn materialize_response_round_trips_both_arms() {
    let plan = MaterializePlanResponse::Plan(ExecutablePlan {
        plan_id: "PLAN-1".into(),
        plan_sha256: "d".repeat(64),
        execution_purpose: "supersedingRecovery".into(),
        public_steps: vec![PublicStep {
            step_id: "STEP-001".into(),
            kind: "writeTarget".into(),
            ..PublicStep::default()
        }],
        ..ExecutablePlan::default()
    });
    assert_eq!(
        MaterializePlanResponse::decode(&plan.encode()).unwrap(),
        plan
    );

    let assessment = MaterializePlanResponse::Assessment(Assessment {
        availability: "unavailable".into(),
        unavailable_reason: "hardware gated".into(),
        unknowns: vec![KeyValue {
            key: "RK-M02".into(),
            value: "AF-V2 required".into(),
        }],
        ..Assessment::default()
    });
    assert_eq!(
        MaterializePlanResponse::decode(&assessment.encode()).unwrap(),
        assessment
    );
}

#[test]
fn a_materialize_response_that_is_both_or_neither_is_refused() {
    assert!(MaterializePlanResponse::decode(&[]).is_err());
    let mut both = Vec::new();
    wire::write_message(&mut both, 1, &ExecutablePlan::default().encode());
    wire::write_message(&mut both, 2, &Assessment::default().encode());
    assert!(MaterializePlanResponse::decode(&both).is_err());
}

#[test]
fn a_newer_peers_extra_fields_do_not_break_decoding() {
    let mut out = Request {
        request_id: "REQ-1".into(),
        api: Api::InspectArtifact,
        payload: vec![],
    }
    .encode();
    wire::write_string(&mut out, 500, "a field from a future version");
    wire::write_uint64(&mut out, 501, 12345);
    let decoded = Request::decode(&out).unwrap();
    assert_eq!(decoded.api, Api::InspectArtifact);
    assert_eq!(decoded.request_id, "REQ-1");
}
// -----------------------------------------------------------------------
// Execution and admission
// -----------------------------------------------------------------------

fn key_value(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.into(),
        value: value.into(),
    }
}

fn snapshot() -> StepAdmissionSnapshot {
    StepAdmissionSnapshot {
        job_id: "JOB-1".into(),
        plan_id: "PLAN-1".into(),
        plan_sha256: vec![0x11; 32],
        step_id: "STEP-WRITE-SYSTEM".into(),
        attempt_id: "ATTEMPT-1".into(),
        public_step_sha256: vec![0x22; 32],
        private_action_sha256: vec![0x33; 32],
        effect_set_sha256: vec![0x44; 32],
        admitted_device_facts_sha256: vec![0x55; 32],
        observed_mode: "updater".into(),
        observed_at_epoch_ms: 1_770_000_000_000,
        snapshot_lifetime_ms: 60_000,
        request_id: "REQ-1".into(),
        topology_sha256: vec![0x66; 32],
        descriptor_sha256: vec![0x77; 32],
        serial_sha256: vec![0x88; 32],
        serial_evidence_kind: "descriptor".into(),
        protocol_identity: vec![key_value("usb.identity", "0x2207:0x350a")],
        identity_strength: "serialAndTopology".into(),
        malformed_descriptor: false,
        transport_session_sha256: vec![0x99; 32],
    }
}

#[test]
fn the_admission_snapshot_round_trips() {
    let encoded = snapshot().encode();
    assert_eq!(StepAdmissionSnapshot::decode(&encoded).unwrap(), snapshot());
}

/// architecture.md 8.3: past the lifetime the daemon takes a new snapshot
/// rather than accepting a late permit, because the facts the authority
/// checked are no longer the facts in front of the device.
#[test]
fn a_snapshot_stops_being_fresh_at_its_lifetime() {
    let snapshot = snapshot();
    assert!(snapshot.is_fresh_at(1_770_000_000_000));
    assert!(snapshot.is_fresh_at(1_770_000_059_999));
    assert!(!snapshot.is_fresh_at(1_770_000_060_000));
    assert!(!snapshot.is_fresh_at(1_770_000_060_001));
}

#[test]
fn a_snapshot_whose_lifetime_overflows_is_never_fresh() {
    // "Forever" is not a lifetime a malformed snapshot gets to claim.
    let forever = StepAdmissionSnapshot {
        observed_at_epoch_ms: u64::MAX,
        snapshot_lifetime_ms: 1,
        ..snapshot()
    };
    assert!(!forever.is_fresh_at(0));
    assert!(!forever.is_fresh_at(u64::MAX));
}

#[test]
fn a_job_event_round_trips_with_each_of_its_payloads() {
    let base = JobEvent {
        job_id: "JOB-1".into(),
        sequence: 7,
        kind: JobEventKind::StateChanged,
        at_epoch_ms: 1_770_000_000_000,
        journal_record_sha256: vec![0xAB; 32],
        job_state: "awaitingPermit".into(),
        facts: vec![key_value("note", "state")],
        ..JobEvent::default()
    };
    for event in [
        base.clone(),
        JobEvent {
            kind: JobEventKind::StepAdmissionRequested,
            admission: Some(snapshot()),
            ..base.clone()
        },
        JobEvent {
            kind: JobEventKind::ManagedControlRequested,
            control_request: Some(ManagedControlRequest {
                job_id: "JOB-1".into(),
                step_id: "STEP-ENSURE-MODE".into(),
                request_id: "REQ-2".into(),
                action: ManagedControlAction::EnterUpdater,
                permit_id: "PERMIT-1".into(),
                expected_facts: vec![key_value("example.build.fullname", "example-build-1.0.0")],
                deadline_epoch_ms: 1_770_000_120_000,
            }),
            ..base.clone()
        },
        JobEvent {
            kind: JobEventKind::ActionReceipt,
            receipt: Some(ActionReceiptSummary {
                job_id: "JOB-1".into(),
                plan_id: "PLAN-1".into(),
                step_id: "STEP-VERIFY".into(),
                action_id: "ACT-013".into(),
                attempt_id: "ATTEMPT-1".into(),
                permit_id: "PERMIT-1".into(),
                disposition: "semanticSuccess".into(),
                evidence_sha256: vec![0xCD; 32],
                verification_outcome: "typedSkip".into(),
                verification_strength: String::new(),
                verified_range_start: 0,
                verified_range_length: 0,
                typed_skip_reason: "skipped-lba-read-window".into(),
                failure_classification: String::new(),
                facts: vec![key_value("partition", "example-target")],
            }),
            ..base
        },
    ] {
        let encoded = event.encode();
        assert_eq!(
            JobEvent::decode(&encoded).unwrap(),
            event,
            "{:?}",
            event.kind
        );
    }
}

#[test]
fn every_event_kind_and_control_action_round_trips_through_its_wire_value() {
    for kind in JobEventKind::ALL {
        assert_eq!(JobEventKind::from_wire(kind.wire_value()), Some(kind));
        assert!(!kind.as_str().is_empty());
    }
    for action in ManagedControlAction::ALL {
        assert_eq!(
            ManagedControlAction::from_wire(action.wire_value()),
            Some(action)
        );
    }
    // An unknown enum value is a hard error, never a default
    // (architecture.md 15.2).
    assert_eq!(JobEventKind::from_wire(0), None);
    assert_eq!(JobEventKind::from_wire(99), None);
    assert_eq!(ManagedControlAction::from_wire(0), None);
}

/// The control action's wire vocabulary must equal the one the typed port
/// already publishes, or the adapter's mapping table names actions this
/// wire cannot carry.
#[test]
fn the_wire_control_actions_match_the_typed_ports_vocabulary() {
    let wire: Vec<&str> = ManagedControlAction::ALL
        .into_iter()
        .map(|action| action.as_str())
        .collect();
    assert_eq!(
        wire,
        vec![
            "enter-updater",
            "reboot-to-normal",
            "read-product-facts",
            "read-build-facts",
        ]
    );
}

#[test]
fn a_permit_submission_carries_a_permit_or_a_refusal_but_not_both() {
    let permit = SubmitStepPermitRequest {
        job_id: "JOB-1".into(),
        request_id: "REQ-1".into(),
        permit_cbor: vec![0xA1, 0x01],
        integrity_tag: vec![0x0F; 32],
        pairing_epoch: 3,
        refusal: String::new(),
    };
    assert!(permit.is_well_formed());
    assert_eq!(
        SubmitStepPermitRequest::decode(&permit.encode()).unwrap(),
        permit
    );

    let refusal = SubmitStepPermitRequest {
        permit_cbor: Vec::new(),
        integrity_tag: Vec::new(),
        refusal: "the operator declined the destructive confirmation".into(),
        ..permit.clone()
    };
    assert!(refusal.is_well_formed());

    // Both would leave the daemon choosing which one the authority meant.
    assert!(
        !SubmitStepPermitRequest {
            refusal: "declined".into(),
            ..permit.clone()
        }
        .is_well_formed()
    );
    // Neither is silence dressed up as an answer.
    assert!(
        !SubmitStepPermitRequest {
            permit_cbor: Vec::new(),
            ..permit
        }
        .is_well_formed()
    );
}

/// architecture.md 9.2, on the wire. The daemon refuses the whole receipt
/// rather than dropping the field and carrying on, because a receipt that
/// carried one of these is the leak the port exists to prevent.
#[test]
fn a_control_receipt_naming_a_forbidden_fact_is_detected() {
    let clean = SubmitManagedControlReceiptRequest {
        job_id: "JOB-1".into(),
        request_id: "REQ-2".into(),
        action: ManagedControlAction::EnterUpdater,
        accepted: true,
        facts: vec![key_value("mode", "updater")],
        evidence_sha256: vec![0x01; 32],
        failure_reason: String::new(),
    };
    assert!(clean.forbidden_facts().is_empty());
    assert_eq!(
        SubmitManagedControlReceiptRequest::decode(&clean.encode()).unwrap(),
        clean
    );

    for forbidden in FORBIDDEN_CONTROL_RECEIPT_FACTS {
        let leaky = SubmitManagedControlReceiptRequest {
            facts: vec![key_value("mode", "updater"), key_value(forbidden, "x")],
            ..clean.clone()
        };
        assert_eq!(leaky.forbidden_facts(), vec![forbidden]);
    }
}

/// A typed skip is never any grade of verified. This is the field a second
/// implementation is most likely to fill in helpfully.
#[test]
fn a_receipt_may_not_carry_a_strength_unless_it_verified() {
    let skipped = ActionReceiptSummary {
        verification_outcome: "typedSkip".into(),
        verification_strength: "fullHash".into(),
        ..ActionReceiptSummary::default()
    };
    assert!(!skipped.strength_is_consistent());

    let verified = ActionReceiptSummary {
        verification_outcome: "verified".into(),
        verification_strength: "prefixHash".into(),
        ..ActionReceiptSummary::default()
    };
    assert!(verified.strength_is_consistent());

    let no_verification = ActionReceiptSummary::default();
    assert!(no_verification.strength_is_consistent());
}

#[test]
fn a_receipt_has_one_canonical_semantic_digest() {
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
        verified_range_start: 4096,
        verified_range_length: 8192,
        typed_skip_reason: "skipped-lba-read-window".into(),
        facts: vec![KeyValue {
            key: "partition".into(),
            value: "system".into(),
        }],
        ..ActionReceiptSummary::default()
    };
    let digest = receipt.receipt_digest().unwrap();
    assert_eq!(
        digest.to_hex(),
        "711f220ee4bff3722c2d59106f21e904c791802d667781018b031b7b470e54ae"
    );
    let body = receipt.to_canonical_bytes().unwrap();
    assert_eq!(
        ActionReceiptSummary::from_canonical_bytes(&body).unwrap(),
        receipt
    );

    let duplicate_fact = ActionReceiptSummary {
        facts: vec![
            KeyValue {
                key: "partition".into(),
                value: "system".into(),
            },
            KeyValue {
                key: "partition".into(),
                value: "vendor".into(),
            },
        ],
        ..receipt
    };
    assert!(matches!(
        duplicate_fact.receipt_digest(),
        Err(CborError::DuplicateMapKey(_))
    ));
}

#[test]
fn a_submission_outcome_round_trips_and_a_rejection_always_says_why() {
    let accepted = SubmissionOutcome::accepted();
    assert_eq!(
        SubmissionOutcome::decode(&accepted.encode()).unwrap(),
        accepted
    );

    let rejected = SubmissionOutcome::rejected("STALE_SNAPSHOT", "the snapshot expired");
    assert!(!rejected.accepted);
    assert!(!rejected.rejection_code.is_empty());
    assert!(!rejected.rejection_message.is_empty());
    assert_eq!(
        SubmissionOutcome::decode(&rejected.encode()).unwrap(),
        rejected
    );
}

/// Readiness travels on the handshake, so it has to survive the codec.
/// The blocker list is repeated-string: a daemon missing both halves must
/// report both, not the first one twice or only one.
#[test]
fn a_hello_ack_round_trips_its_readiness() {
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
    assert_eq!(HelloAck::decode(&ack.encode()).unwrap(), ack);

    let ready = HelloAck {
        execution_ready: true,
        execution_blockers: Vec::new(),
        toolchain_id: "example-tool-fixed".into(),
        toolchain_sha256: "a".repeat(64),
        ..ack
    };
    assert_eq!(HelloAck::decode(&ready.encode()).unwrap(), ready);
}

#[test]
fn a_watch_request_round_trips_with_a_resume_cursor() {
    let request = WatchJobRequest {
        job_id: "JOB-1".into(),
        from_sequence: 42,
    };
    assert_eq!(WatchJobRequest::decode(&request.encode()).unwrap(), request);
}
