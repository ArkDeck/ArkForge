//! One complete dispatch derived from the golden transcript replay surface.
//! The replay record is evidence, not permission to execute on hardware.

use crate::cbor_repr::diag;
use crate::json::Json;
use crate::suites::case_id;
use crate::tree::{Case, Tree};
use arkforge_core::digest::{CanonicalCbor, decode_canonical, sha256};
use arkforge_ipc::messages::{ActionReceiptSummary, KeyValue};
use arkforge_transport::replay::TranscriptTransport;
use arkforge_transport::transcript;

const SUITE: &str = "transcript-dispatch";
const CAMPAIGN: &str = include_str!("../../../../transcripts/dayu200-gj4-ecamp-96effff15.yaml");

pub fn populate(tree: &mut Tree) {
    let parsed = transcript::parse(CAMPAIGN).expect("published transcript parses");
    let transport = TranscriptTransport::new(parsed);
    let record = transport
        .invocation("flash-partitions", 0)
        .expect("campaign records its destructive step");
    let evidence = record
        .response_digest
        .expect("successful invocation carries response evidence");
    let receipt = ActionReceiptSummary {
        job_id: "JOB-CONF-REPLAY".into(),
        plan_id: "PLAN-CONF-REPLAY".into(),
        step_id: "STEP-FLASH-PARTITIONS".into(),
        action_id: "ACT-FLASH-PARTITIONS".into(),
        attempt_id: "ATTEMPT-1".into(),
        permit_id: "PERMIT-CONF-REPLAY".into(),
        disposition: "semanticSuccess".into(),
        evidence_sha256: evidence.as_bytes().to_vec(),
        facts: record
            .semantic
            .iter()
            .map(|item| KeyValue {
                key: item.key.to_string(),
                value: item.value.clone(),
            })
            .collect(),
        ..ActionReceiptSummary::default()
    };
    let receipt_body = receipt.to_canonical_bytes().unwrap();
    tree.case(
        &Case {
            id: case_id("TRANSCRIPT-DISPATCH", 1),
            suite: SUITE,
            title: "replay one recorded destructive step into a semantic receipt".to_string(),
            requirements: vec!["AF-TRN-020", "AF-TRN-023", "AF-DIG-012"],
            kind: "replay",
            description: "Parse the committed transcript, select occurrence 0 of the semantic action flash-partitions, require status=ok and responseDigest, copy its ordered semantic facts into the receipt, and compute the canonical ActionReceipt digest. This is contract replay only; a replay toolchain never permits an executable plan.".to_string(),
            input: Json::object(vec![
                ("transcriptFile", Json::str("transcript.yaml")),
                ("transcriptSha256", Json::str(sha256(CAMPAIGN.as_bytes()).to_hex())),
                ("action", Json::str("flash-partitions")),
                ("occurrence", Json::Unsigned(0)),
            ]),
            expected: Json::object(vec![
                ("recordSequence", Json::Unsigned(record.sequence as u64)),
                ("recordStatus", Json::str(record.status.as_str())),
                ("evidenceSha256", Json::str(evidence.to_hex())),
                ("receiptDigest", Json::str(receipt.receipt_digest().unwrap().to_hex())),
                ("receiptDiag", Json::str(diag(&decode_canonical(&receipt_body).unwrap()))),
                ("hardwareExecutionPermitted", Json::Bool(false)),
            ]),
        },
        vec![
            ("transcript.yaml", CAMPAIGN.as_bytes().to_vec()),
            ("receipt.cbor", receipt_body),
        ],
    );

    assert!(transport.invocation("erase-unknown-partition", 0).is_err());
    tree.case(
        &Case {
            id: case_id("TRANSCRIPT-DISPATCH", 2),
            suite: SUITE,
            title: "replay refuses an action the transcript never recorded".to_string(),
            requirements: vec!["AF-TRN-023"],
            kind: "replay",
            description: "A replay implementation may select only the named occurrence actually present. It never invents a response or falls through to a hardware backend.".to_string(),
            input: Json::object(vec![
                ("transcriptFile", Json::str("transcript.yaml")),
                ("action", Json::str("erase-unknown-partition")),
                ("occurrence", Json::Unsigned(0)),
            ]),
            expected: Json::object(vec![
                ("result", Json::str("reject")),
                ("errorClass", Json::str("TRANSPORT_UNSUPPORTED")),
                ("hardwareExecutionAttempted", Json::Bool(false)),
            ]),
        },
        vec![("transcript.yaml", CAMPAIGN.as_bytes().to_vec())],
    );
}
