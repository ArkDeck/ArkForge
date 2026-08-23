//! Canonical ActionReceipt bodies and semantic digests. The protobuf summary
//! is transport; these CBOR bytes are the cross-language receipt identity.

use crate::cbor_repr::diag;
use crate::json::Json;
use crate::suites::case_id;
use crate::tree::{Case, Tree};
use arkforge_core::digest::{CanonicalCbor, CborError, Domain, decode_canonical, sha256};
use arkforge_ipc::messages::{ActionReceiptSummary, KeyValue};

const SUITE: &str = "action-receipt";

fn domain_text() -> String {
    String::from_utf8_lossy(Domain::ActionReceipt.as_bytes()).replace('\0', "\\0")
}

fn fact(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: value.to_string(),
    }
}

fn emit(tree: &mut Tree, number: u32, title: &str, receipt: ActionReceiptSummary) {
    let body = receipt
        .to_canonical_bytes()
        .expect("fixture receipt encodes");
    let digest = receipt.receipt_digest().expect("fixture receipt digests");
    tree.case(
        &Case {
            id: case_id("RECEIPT", number),
            suite: SUITE,
            title: title.to_string(),
            requirements: vec!["AF-DIG-012", "AF-ENG-011"],
            kind: "digest",
            description: "Encode the ActionReceipt semantic body as deterministic CBOR, then compute SHA-256(arkforge/v1/action-receipt\\0 || body). Protobuf bytes and transport evidence alone are not the receipt identity.".to_string(),
            input: Json::object(vec![
                ("bodyFile", Json::str("receipt.cbor")),
                ("bodySha256", Json::str(sha256(&body).to_hex())),
            ]),
            expected: Json::object(vec![
                ("domain", Json::str(domain_text())),
                ("receiptDigest", Json::str(digest.to_hex())),
                ("diag", Json::str(diag(&decode_canonical(&body).unwrap()))),
            ]),
        },
        vec![("receipt.cbor", body)],
    );
}

pub fn populate(tree: &mut Tree) {
    emit(
        tree,
        1,
        "semantic success with a full verification range",
        ActionReceiptSummary {
            job_id: "JOB-1".into(),
            plan_id: "PLAN-1".into(),
            step_id: "STEP-1".into(),
            action_id: "ACT-1".into(),
            attempt_id: "ATTEMPT-1".into(),
            permit_id: "PERMIT-1".into(),
            disposition: "semanticSuccess".into(),
            evidence_sha256: vec![0xaa; 32],
            verification_outcome: "verified".into(),
            verification_strength: "fullHash".into(),
            verified_range_start: 4096,
            verified_range_length: 8192,
            facts: vec![fact("partition", "system"), fact("toolExit", "0")],
            ..ActionReceiptSummary::default()
        },
    );

    emit(
        tree,
        2,
        "typed skip has a range and reason but no verification strength",
        ActionReceiptSummary {
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
            facts: vec![fact("partition", "system")],
            ..ActionReceiptSummary::default()
        },
    );

    emit(
        tree,
        3,
        "settled no-effect failure is still a durable semantic receipt",
        ActionReceiptSummary {
            job_id: "JOB-2".into(),
            plan_id: "PLAN-2".into(),
            step_id: "STEP-2".into(),
            action_id: String::new(),
            attempt_id: "ATTEMPT-1".into(),
            permit_id: "PERMIT-2".into(),
            disposition: "confirmedNoEffect".into(),
            evidence_sha256: vec![0x11; 32],
            facts: vec![fact("failureBoundary", "beforeExternalIo")],
            ..ActionReceiptSummary::default()
        },
    );

    let duplicate = ActionReceiptSummary {
        job_id: "JOB-1".into(),
        plan_id: "PLAN-1".into(),
        step_id: "STEP-1".into(),
        attempt_id: "ATTEMPT-1".into(),
        permit_id: "PERMIT-1".into(),
        disposition: "semanticSuccess".into(),
        evidence_sha256: vec![0; 32],
        facts: vec![fact("partition", "system"), fact("partition", "vendor")],
        ..ActionReceiptSummary::default()
    };
    assert!(matches!(
        duplicate.receipt_digest(),
        Err(CborError::DuplicateMapKey(_))
    ));
    tree.case(
        &Case {
            id: case_id("RECEIPT", 4),
            suite: SUITE,
            title: "duplicate fact keys have no receipt digest".to_string(),
            requirements: vec!["AF-DIG-006", "AF-DIG-012"],
            kind: "verify",
            description: "Facts encode as one CBOR map. Equal fact keys MUST reject the whole receipt; a port may not keep the first or last value.".to_string(),
            input: Json::object(vec![(
                "facts",
                Json::Array(vec![
                    Json::Array(vec![Json::str("partition"), Json::str("system")]),
                    Json::Array(vec![Json::str("partition"), Json::str("vendor")]),
                ]),
            )]),
            expected: Json::object(vec![
                ("result", Json::str("reject")),
                ("errorClass", Json::str("CBOR_DUPLICATE_MAP_KEY")),
            ]),
        },
        Vec::new(),
    );

    let invalid_key = ActionReceiptSummary {
        job_id: "JOB-1".into(),
        plan_id: "PLAN-1".into(),
        step_id: "STEP-1".into(),
        attempt_id: "ATTEMPT-1".into(),
        permit_id: "PERMIT-1".into(),
        disposition: "semanticSuccess".into(),
        evidence_sha256: vec![0; 32],
        facts: vec![fact("has whitespace", "value")],
        ..ActionReceiptSummary::default()
    };
    assert!(matches!(
        invalid_key.receipt_digest(),
        Err(CborError::ModelViolation(_))
    ));
    tree.case(
        &Case {
            id: case_id("RECEIPT", 5),
            suite: SUITE,
            title: "receipt fact keys use the OpaqueId grammar".to_string(),
            requirements: vec!["AF-ID-001", "AF-DIG-012"],
            kind: "verify",
            description: "Fact values are explicit free text; fact keys are identifiers and must match [A-Za-z0-9._:-]{1,128}. Reject before hashing.".to_string(),
            input: Json::object(vec![("factKey", Json::str("has whitespace"))]),
            expected: Json::object(vec![
                ("result", Json::str("reject")),
                ("errorClass", Json::str("CBOR_MODEL_VIOLATION")),
            ]),
        },
        Vec::new(),
    );
}
