//! Contract a consumer runs against a pinned ArkForge revision.
//!
//! ```text
//! cargo test -p arkforge-ipc --test consumer_contract
//! ```
//!
//! The bytes below are the Hello body a peer sends. A 4-byte big-endian
//! length prefixes that body on the socket (`docs/arkdeck-contract.md`).

use arkforge_ipc::contract::{
    self, BOOT_LOOP_RESTORATION_SCHEMA, CONTRACT_TAG, FLASH_VERIFICATION_SCHEMA,
    FLASH_VERIFY_EXIT_FAILED, FLASH_VERIFY_EXIT_MATCHED, FLASH_VERIFY_EXIT_PARTIAL,
    PROTOCOL_VERSION,
};
use arkforge_ipc::framing::{read_frame, write_frame};
use arkforge_ipc::messages::{Hello, HelloAck};
use arkforge_ipc::recovery::SupersedingRecoveryAssessment;
use arkforge_ipc::{PROTOCOL_MAJOR, PROTOCOL_MINOR, SessionKind};
use std::io::Cursor;

#[test]
fn the_pinned_protocol_is_major_1_minor_0() {
    assert!(contract::protocol_version_matches_constants());
    assert_eq!(PROTOCOL_VERSION, "1.0");
    assert_eq!(PROTOCOL_MAJOR, 1);
    assert_eq!(PROTOCOL_MINOR, 0);
    assert_eq!(CONTRACT_TAG, "arkforge-contract-v1.0.0");
    assert_ne!(PROTOCOL_VERSION, "0.1.0");
    assert_ne!(PROTOCOL_VERSION, "1.0.0");
}

#[test]
fn handshake_refuses_only_a_different_major() {
    assert_eq!(contract::handshake_refusal(PROTOCOL_MAJOR, 0), None);
    assert_eq!(contract::handshake_refusal(PROTOCOL_MAJOR, 5), None);
    assert_eq!(
        contract::handshake_refusal(PROTOCOL_MAJOR + 1, 0).as_deref(),
        Some("protocol major 2 is not compatible with 1")
    );
    assert_eq!(
        contract::handshake_refusal(0, PROTOCOL_MINOR).as_deref(),
        Some("protocol major 0 is not compatible with 1")
    );
}

#[test]
fn hello_round_trips_and_pins_the_public_and_controller_bytes() {
    let public = Hello {
        protocol_major: PROTOCOL_MAJOR,
        protocol_minor: PROTOCOL_MINOR,
        session_kind: SessionKind::Public,
    };
    let public_body = public.encode();
    assert_eq!(public_body, [0x08, 0x01, 0x18, 0x01]);
    assert_eq!(Hello::decode(&public_body).unwrap(), public);
    assert_eq!(Hello::decode(&public_body).unwrap().encode(), public_body);

    let controller = Hello {
        protocol_major: PROTOCOL_MAJOR,
        protocol_minor: PROTOCOL_MINOR,
        session_kind: SessionKind::Controller,
    };
    assert_eq!(controller.encode(), [0x08, 0x01, 0x18, 0x02]);

    let mut framed = Vec::new();
    write_frame(&mut framed, &public_body).unwrap();
    assert_eq!(framed, [0x00, 0x00, 0x00, 0x04, 0x08, 0x01, 0x18, 0x01]);
    let decoded = read_frame(&mut Cursor::new(&framed)).unwrap().unwrap();
    assert_eq!(Hello::decode(&decoded).unwrap(), public);
}

#[test]
fn an_unknown_hello_field_is_skipped_and_re_encoding_drops_it() {
    let mut body = vec![0x08, 0x01, 0x18, 0x01];
    // Field 99, varint 7. Tag (99 << 3) | 0 = 792 = 0x98 0x06.
    body.extend_from_slice(&[0x98, 0x06, 0x07]);
    let hello = Hello::decode(&body).unwrap();
    assert_eq!(hello.protocol_major, PROTOCOL_MAJOR);
    assert_eq!(hello.protocol_minor, PROTOCOL_MINOR);
    assert_eq!(hello.session_kind, SessionKind::Public);
    assert_eq!(hello.encode(), [0x08, 0x01, 0x18, 0x01]);
}

#[test]
fn hello_ack_round_trips_a_refusal_and_the_daemon_version_separately() {
    let refused = HelloAck {
        protocol_major: PROTOCOL_MAJOR,
        protocol_minor: PROTOCOL_MINOR,
        session_kind: SessionKind::Public,
        daemon_version: "0.1.0".into(),
        refusal: contract::handshake_refusal(2, 0),
        execution_ready: false,
        execution_blockers: vec!["NO_PAIRED_AUTHORITY".into()],
        toolchain_id: String::new(),
        toolchain_sha256: String::new(),
    };
    let encoded = refused.encode();
    assert_eq!(HelloAck::decode(&encoded).unwrap(), refused);
    assert_eq!(HelloAck::decode(&encoded).unwrap().encode(), encoded);
    assert_eq!(refused.daemon_version, "0.1.0");
    assert_ne!(refused.daemon_version, PROTOCOL_VERSION);
}

#[test]
fn a_profile_recovery_version_round_trips_apart_from_the_ipc_protocol() {
    let assessment = SupersedingRecoveryAssessment {
        job_id: "JOB-1".into(),
        eligible: false,
        blocker_code: "SOURCE_NOT_ELIGIBLE".into(),
        blocker_reason: "outcome still unknown".into(),
        covered_effects: vec!["ramdisk".into()],
        recovery_contract_id: "org.openharmony.dayu200".into(),
        recovery_contract_version: "1.0.0".into(),
        recovery_contract_sha256: "ab".repeat(32),
        ..SupersedingRecoveryAssessment::default()
    };
    let decoded = SupersedingRecoveryAssessment::decode(&assessment.encode()).unwrap();
    assert_eq!(decoded, assessment);
    assert_eq!(decoded.encode(), assessment.encode());
    assert_eq!(decoded.recovery_contract_version, "1.0.0");
    assert_ne!(decoded.recovery_contract_version, PROTOCOL_VERSION);
    assert_ne!(decoded.recovery_contract_version, CONTRACT_TAG);
}

#[test]
fn flash_verify_exits_follow_failed_then_skipped() {
    assert_eq!(
        contract::flash_verify_exit_code(0, 0),
        FLASH_VERIFY_EXIT_MATCHED
    );
    assert_eq!(contract::flash_verify_exit_code(0, 0), 0);
    assert_eq!(contract::flash_verify_outcome(0, 0), "verified");

    assert_eq!(
        contract::flash_verify_exit_code(1, 0),
        FLASH_VERIFY_EXIT_FAILED
    );
    assert_eq!(contract::flash_verify_exit_code(1, 4), 7);
    assert_eq!(contract::flash_verify_outcome(1, 4), "failed");

    assert_eq!(
        contract::flash_verify_exit_code(0, 1),
        FLASH_VERIFY_EXIT_PARTIAL
    );
    assert_eq!(contract::flash_verify_exit_code(0, 1), 9);
    assert_eq!(contract::flash_verify_outcome(0, 1), "partial");

    assert_eq!(FLASH_VERIFICATION_SCHEMA, "arkforge.flash-verification/v1");
    assert_eq!(
        BOOT_LOOP_RESTORATION_SCHEMA,
        "arkforge.boot-loop-restoration/v1"
    );
}

#[test]
fn the_api_versions_file_matches_the_ipc_contract() {
    let versions = include_str!("../../../api/versions.yaml");
    for needle in [
        "schema: arkforge.contract-versions/v1",
        "proto: api/arkforge.proto",
        "package: arkforge.ipc.v1",
        "protocolMajor: 1",
        "protocolMinor: 0",
        "protocolVersion: \"1.0\"",
        &format!("contractTag: {CONTRACT_TAG}"),
        "cargoPackage: \"0.1.0\"",
        "specVersion: 1.0.0-draft.4",
    ] {
        assert!(
            versions.contains(needle),
            "api/versions.yaml lacks {needle}"
        );
    }
}

#[test]
fn the_boot_loop_script_publishes_the_pinned_summary_schema() {
    let script = include_str!("../../../hack/restore-boot-loop.py");
    assert!(script.contains(BOOT_LOOP_RESTORATION_SCHEMA));
    assert!(script.contains("No usable USB session"));
    assert!(script.contains("enter Loader physically"));
}
