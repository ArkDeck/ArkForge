//! The operational journal: record encoding, hash chain, on-disk framing, and
//! the exhaustive torn-tail campaign (every truncation offset of a real file
//! is either replayed as a shorter prefix or refused — never silently
//! re-interpreted).

use crate::cbor_repr::diag;
use crate::json::{Json, hex};
use crate::suites::case_id;
use crate::tree::{Case, Tree};
use arkforge_core::digest::decode_canonical;
use arkforge_core::ids::OpaqueId;
use arkforge_engine::durable::{DurableJournal, DurableJournalError};
use arkforge_engine::journal::{FsyncPolicy, Journal, JournalError, JournalRecordKind};
use std::path::PathBuf;

const SUITE: &str = "journal";

pub(crate) fn id(value: &str) -> OpaqueId {
    OpaqueId::new(value).unwrap()
}

pub(crate) fn facts(pairs: &[(&str, &str)]) -> Vec<(OpaqueId, String)> {
    pairs
        .iter()
        .map(|(k, v)| (id(k), (*v).to_string()))
        .collect()
}

/// `(kind, atEpochMs, jobRevision, subject, facts)`.
pub(crate) type SampleRecord = (
    JournalRecordKind,
    u64,
    u64,
    OpaqueId,
    Vec<(OpaqueId, String)>,
);

/// A short but realistic prefix of a job: plan stored, job created, a
/// buffered observation, a permit accepted, an intent recorded.
pub(crate) fn sample_records() -> Vec<SampleRecord> {
    vec![
        (
            JournalRecordKind::PlanStored,
            1_000,
            1,
            id("PLAN-1"),
            facts(&[("planId", "PLAN-1")]),
        ),
        (
            JournalRecordKind::JobCreated,
            1_010,
            1,
            id("JOB-1"),
            facts(&[("jobId", "JOB-1"), ("planId", "PLAN-1")]),
        ),
        (
            JournalRecordKind::PreflightObserved,
            1_020,
            1,
            id("JOB-1"),
            facts(&[("jobId", "JOB-1"), ("mode", "rockusb-loader")]),
        ),
        (
            JournalRecordKind::StepPermitAccepted,
            1_030,
            1,
            id("JOB-1"),
            facts(&[
                ("jobId", "JOB-1"),
                ("stepId", "STEP-1"),
                ("permitId", "PERMIT-1"),
            ]),
        ),
        (
            JournalRecordKind::StepIntentRecorded,
            1_040,
            1,
            id("JOB-1"),
            facts(&[
                ("jobId", "JOB-1"),
                ("stepId", "STEP-1"),
                ("permitId", "PERMIT-1"),
            ]),
        ),
    ]
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "arkforge-conformance-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Scratch(path)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn journal_error_code(error: &JournalError) -> Json {
    let (code, detail) = match error {
        JournalError::SequenceBroken { expected, found } => (
            "JOURNAL_SEQUENCE_BROKEN",
            Json::object(vec![
                ("expected", Json::Unsigned(*expected)),
                ("found", Json::Unsigned(*found)),
            ]),
        ),
        JournalError::ChainBroken { at_sequence } => (
            "JOURNAL_CHAIN_BROKEN",
            Json::object(vec![("atSequence", Json::Unsigned(*at_sequence))]),
        ),
        JournalError::RecordTampered { at_sequence } => (
            "JOURNAL_RECORD_TAMPERED",
            Json::object(vec![("atSequence", Json::Unsigned(*at_sequence))]),
        ),
        JournalError::FsyncPolicyMisdeclared {
            at_sequence,
            kind,
            declared,
        } => (
            "JOURNAL_FSYNC_POLICY_MISDECLARED",
            Json::object(vec![
                ("atSequence", Json::Unsigned(*at_sequence)),
                ("kind", Json::str(kind.as_str())),
                ("declared", Json::str(declared.as_str())),
                ("required", Json::str(kind.fsync_policy().as_str())),
            ]),
        ),
        JournalError::UnknownSchemaVersion(version) => (
            "JOURNAL_UNKNOWN_SCHEMA_VERSION",
            Json::object(vec![("version", Json::Unsigned(*version as u64))]),
        ),
        JournalError::RecordMalformed(field) => (
            "JOURNAL_RECORD_MALFORMED",
            Json::object(vec![("field", Json::str(*field))]),
        ),
        JournalError::Cbor(_) => ("JOURNAL_CBOR", Json::object(vec![])),
    };
    Json::object(vec![
        ("result", Json::str("reject")),
        ("code", Json::str(code)),
        ("detail", detail),
    ])
}

fn durable_outcome(
    outcome: &Result<
        (DurableJournal, arkforge_engine::durable::RecoveryReport),
        DurableJournalError,
    >,
) -> Json {
    match outcome {
        Ok((journal, report)) => Json::object(vec![
            ("result", Json::str("accept")),
            (
                "recordsReplayed",
                Json::Unsigned(report.records_replayed as u64),
            ),
            ("tornTailBytes", Json::Unsigned(report.torn_tail_bytes)),
            ("headDigest", Json::str(journal.head_digest().to_hex())),
        ]),
        Err(DurableJournalError::NotAJournal { .. }) => Json::object(vec![
            ("result", Json::str("reject")),
            ("code", Json::str("JOURNAL_NOT_A_JOURNAL")),
        ]),
        Err(DurableJournalError::FrameLengthInvalid { at_offset, length }) => Json::object(vec![
            ("result", Json::str("reject")),
            ("code", Json::str("JOURNAL_FRAME_LENGTH_INVALID")),
            ("atOffset", Json::Unsigned(*at_offset)),
            ("length", Json::Unsigned(*length as u64)),
        ]),
        Err(DurableJournalError::RecordTooLarge(size)) => Json::object(vec![
            ("result", Json::str("reject")),
            ("code", Json::str("JOURNAL_RECORD_TOO_LARGE")),
            ("size", Json::Unsigned(*size as u64)),
        ]),
        Err(DurableJournalError::Poisoned { .. }) => Json::object(vec![
            ("result", Json::str("reject")),
            ("code", Json::str("JOURNAL_POISONED")),
        ]),
        Err(DurableJournalError::Journal(error)) => journal_error_code(error),
        Err(DurableJournalError::Io { .. }) => Json::object(vec![
            ("result", Json::str("reject")),
            ("code", Json::str("JOURNAL_IO")),
        ]),
    }
}

fn open_bytes(scratch: &Scratch, name: &str, bytes: &[u8]) -> Json {
    let path = scratch.file(name);
    std::fs::write(&path, bytes).unwrap();
    let outcome = DurableJournal::open(&path);
    let json = durable_outcome(&outcome);
    drop(outcome);
    let _ = std::fs::remove_file(&path);
    json
}

pub fn populate(tree: &mut Tree) {
    let mut number = 0u32;

    // ---- 1. the in-memory chain --------------------------------------------
    let mut journal = Journal::new();
    for (kind, at, revision, subject, facts) in sample_records() {
        journal.append(kind, at, revision, subject, facts).unwrap();
    }
    journal.verify().unwrap();

    number += 1;
    let mut record_files: Vec<(&str, Vec<u8>)> = Vec::new();
    let names = [
        "record-1.cbor",
        "record-2.cbor",
        "record-3.cbor",
        "record-4.cbor",
        "record-5.cbor",
    ];
    let mut record_json = Vec::new();
    for (index, record) in journal.records().iter().enumerate() {
        let bytes = record.to_canonical_bytes().unwrap();
        record_json.push(Json::object(vec![
            ("sequence", Json::Unsigned(record.sequence)),
            ("kind", Json::str(record.kind.as_str())),
            ("fsyncPolicy", Json::str(record.fsync_policy.as_str())),
            ("previousDigest", Json::str(record.previous_digest.to_hex())),
            ("recordDigest", Json::str(record.record_digest.to_hex())),
            ("diag", Json::str(diag(&decode_canonical(&bytes).unwrap()))),
        ]));
        record_files.push((names[index], bytes));
    }
    tree.case(
        &Case {
            id: case_id("JOURNAL", number),
            suite: SUITE,
            title: "five-record chain: bodies, digests and links".to_string(),
            requirements: vec!["AF-JRN-001", "AF-JRN-002", "AF-JRN-003", "AF-JRN-004"],
            kind: "encode",
            description: "recordDigest = SHA-256(\"arkforge/v1/journal-record\\0\" || \
                          deterministic_cbor(body without recordDigest)). The stored \
                          record is the body plus `recordDigest`. Record 1 links to the \
                          32-zero-byte chain origin. fsyncPolicy is the policy the kind \
                          requires."
                .to_string(),
            input: Json::object(vec![
                (
                    "schemaVersion",
                    Json::Unsigned(arkforge_engine::journal::SCHEMA_VERSION as u64),
                ),
                (
                    "chainOrigin",
                    Json::str(hex(&arkforge_engine::journal::CHAIN_ORIGIN)),
                ),
                (
                    "records",
                    Json::Array(
                        sample_records()
                            .iter()
                            .map(|(kind, at, revision, subject, facts)| {
                                Json::object(vec![
                                    ("kind", Json::str(kind.as_str())),
                                    ("atEpochMs", Json::Unsigned(*at)),
                                    ("jobRevision", Json::Unsigned(*revision)),
                                    ("subject", Json::str(subject.as_str())),
                                    (
                                        "facts",
                                        Json::Object(
                                            facts
                                                .iter()
                                                .map(|(k, v)| {
                                                    (k.as_str().to_string(), Json::str(v.clone()))
                                                })
                                                .collect(),
                                        ),
                                    ),
                                ])
                            })
                            .collect(),
                    ),
                ),
            ]),
            expected: Json::object(vec![
                ("records", Json::Array(record_json)),
                ("headDigest", Json::str(journal.head_digest().to_hex())),
            ]),
        },
        record_files,
    );

    // ---- 2. fsync policy table -----------------------------------------------
    number += 1;
    tree.case(
        &Case {
            id: case_id("JOURNAL", number),
            suite: SUITE,
            title: "fsync policy per record kind".to_string(),
            requirements: vec!["AF-JRN-005", "AF-JRN-006"],
            kind: "table",
            description: "The policy is a function of the kind. A record declaring a \
                          weaker policy than its kind requires is tampering."
                .to_string(),
            input: Json::object(vec![]),
            expected: Json::object(vec![(
                "kinds",
                Json::Array(
                    JournalRecordKind::ALL
                        .iter()
                        .map(|kind| {
                            Json::object(vec![
                                ("kind", Json::str(kind.as_str())),
                                ("fsyncPolicy", Json::str(kind.fsync_policy().as_str())),
                            ])
                        })
                        .collect(),
                ),
            )]),
        },
        Vec::new(),
    );

    // ---- 3. the on-disk file ---------------------------------------------------
    let scratch = Scratch::new("journal");
    let path = scratch.file("journal.bin");
    let (mut durable, report) = DurableJournal::open(&path).unwrap();
    assert!(!report.existed);
    let mut digests = Vec::new();
    for (kind, at, revision, subject, facts) in sample_records() {
        digests.push(durable.append(kind, at, revision, subject, facts).unwrap());
    }
    durable.sync().unwrap();
    drop(durable);
    let file_bytes = std::fs::read(&path).unwrap();
    let _ = std::fs::remove_file(&path);

    number += 1;
    let file_case = case_id("JOURNAL", number);
    tree.case(
        &Case {
            id: file_case.clone(),
            suite: SUITE,
            title: "on-disk framing: magic, then 4-byte big-endian length + record".to_string(),
            requirements: vec!["AF-JRN-010", "AF-JRN-011"],
            kind: "encode",
            description: "The file begins with the 8-byte magic `ARKFJRN1`. Each frame is \
                          a u32 big-endian length followed by exactly that many bytes of \
                          the canonical record. Length and body are written in one call."
                .to_string(),
            input: Json::object(vec![("records", Json::str(case_id("JOURNAL", 1)))]),
            expected: Json::object(vec![
                ("magicAscii", Json::str("ARKFJRN1")),
                ("fileLength", Json::Unsigned(file_bytes.len() as u64)),
                ("maxFrameBytes", Json::Unsigned(1 << 20)),
                (
                    "recordDigests",
                    Json::strs(digests.iter().map(|d| d.to_hex())),
                ),
            ]),
        },
        vec![("journal.bin", file_bytes.clone())],
    );

    // ---- 4. exhaustive torn tail ---------------------------------------------
    number += 1;
    let mut rows = Vec::new();
    for length in 0..=file_bytes.len() {
        let outcome = open_bytes(&scratch, "torn.bin", &file_bytes[..length]);
        let mut row = Json::object(vec![("length", Json::Unsigned(length as u64))]);
        if let Json::Object(entries) = outcome {
            for (k, v) in entries {
                if k != "headDigest" {
                    row.push(&k, v);
                }
            }
        }
        rows.push(row);
    }
    tree.case(
        &Case {
            id: case_id("JOURNAL", number),
            suite: SUITE,
            title: "every truncation of the file is a shorter prefix or a refusal".to_string(),
            requirements: vec!["AF-JRN-012", "AF-CRASH-001"],
            kind: "replay",
            description: "For each `length` in 0..=fileLength, open the first `length` \
                          bytes of `journal.bin` from AF-CONF-JOURNAL-003. An empty file \
                          is a new journal. A partial magic is not a journal. A cut \
                          inside a frame is a torn tail: the complete frames before it \
                          replay and the rest is discarded and reported. A cut on a frame \
                          boundary is clean."
                .to_string(),
            input: Json::object(vec![("file", Json::str(&file_case))]),
            expected: Json::object(vec![("table", Json::Array(rows))]),
        },
        Vec::new(),
    );

    // ---- 5. tampering ------------------------------------------------------------
    struct Tamper {
        title: &'static str,
        requirements: Vec<&'static str>,
        bytes: Vec<u8>,
        mutation: String,
    }
    let frame_offsets = frame_offsets(&file_bytes);
    let mut tampers: Vec<Tamper> = Vec::new();
    {
        // Flip one byte inside record 2's body (the atEpochMs value).
        let (start, _) = frame_offsets[1];
        let body_start = start + 4;
        let body = &file_bytes[body_start..frame_offsets[2].0];
        let needle = arkforge_core::digest::CborValue::text("atEpochMs")
            .to_canonical_bytes()
            .unwrap();
        let at = body
            .windows(needle.len())
            .position(|w| w == needle.as_slice())
            .unwrap()
            + needle.len();
        let mut bytes = file_bytes.clone();
        // 0x19 0x03 0xf2 (1010) -> change the low byte.
        bytes[body_start + at + 2] ^= 0x01;
        tampers.push(Tamper {
            title: "a byte edited inside a record body breaks that record's digest",
            requirements: vec!["AF-JRN-013"],
            bytes,
            mutation: "record 2 atEpochMs low byte xor 1".into(),
        });
    }
    {
        // Remove frame 2 entirely and re-link nothing: sequence check fires.
        let (s2, _) = frame_offsets[1];
        let (s3, _) = frame_offsets[2];
        let mut bytes = file_bytes[..s2].to_vec();
        bytes.extend_from_slice(&file_bytes[s3..]);
        tampers.push(Tamper {
            title: "a removed middle frame breaks the sequence",
            requirements: vec!["AF-JRN-014"],
            bytes,
            mutation: "frame 2 removed".into(),
        });
    }
    {
        // Re-sign record 3 against record 1: chain broken even though the
        // record digest is self-consistent.
        let mut journal2 = Journal::new();
        for (kind, at, revision, subject, facts) in sample_records() {
            journal2.append(kind, at, revision, subject, facts).unwrap();
        }
        let mut relinked = journal2.records()[2].clone();
        relinked.previous_digest = journal2.records()[0].record_digest;
        relinked.record_digest = relinked.recompute_digest().unwrap();
        let bytes = splice_frame(
            &file_bytes,
            &frame_offsets,
            2,
            &relinked.to_canonical_bytes().unwrap(),
        );
        tampers.push(Tamper {
            title: "a re-linked, re-digested record still breaks the chain",
            requirements: vec!["AF-JRN-015"],
            bytes,
            mutation: "record 3 previousDigest := record 1 digest, digest recomputed".into(),
        });
    }
    {
        let mut journal2 = Journal::new();
        for (kind, at, revision, subject, facts) in sample_records() {
            journal2.append(kind, at, revision, subject, facts).unwrap();
        }
        let mut downgraded = journal2.records()[0].clone();
        downgraded.fsync_policy = FsyncPolicy::Buffered;
        downgraded.record_digest = downgraded.recompute_digest().unwrap();
        let bytes = splice_frame(
            &file_bytes,
            &frame_offsets,
            0,
            &downgraded.to_canonical_bytes().unwrap(),
        );
        tampers.push(Tamper {
            title: "a durable kind declaring buffered is refused as tampering",
            requirements: vec!["AF-JRN-006"],
            bytes,
            mutation: "record 1 (planStored) fsyncPolicy := buffered, digest recomputed".into(),
        });
    }
    {
        let mut journal2 = Journal::new();
        for (kind, at, revision, subject, facts) in sample_records() {
            journal2.append(kind, at, revision, subject, facts).unwrap();
        }
        let mut future = journal2.records()[0].clone();
        future.schema_version = 99;
        future.record_digest = future.recompute_digest().unwrap();
        let bytes = splice_frame(
            &file_bytes,
            &frame_offsets,
            0,
            &future.to_canonical_bytes().unwrap(),
        );
        tampers.push(Tamper {
            title: "an unknown schema version fails closed",
            requirements: vec!["AF-JRN-016"],
            bytes,
            mutation: "record 1 schemaVersion := 99, digest recomputed".into(),
        });
    }
    {
        let mut bytes = file_bytes.clone();
        let (s2, _) = frame_offsets[1];
        bytes[s2..s2 + 4].copy_from_slice(&0u32.to_be_bytes());
        tampers.push(Tamper {
            title: "a zero frame length is not a torn write; it is refused",
            requirements: vec!["AF-JRN-011"],
            bytes,
            mutation: "frame 2 length := 0".into(),
        });
    }
    {
        let mut bytes = file_bytes.clone();
        let (s2, _) = frame_offsets[1];
        bytes[s2..s2 + 4].copy_from_slice(&((1u32 << 20) + 1).to_be_bytes());
        tampers.push(Tamper {
            title: "a frame length above the bound is refused before allocation",
            requirements: vec!["AF-JRN-011"],
            bytes,
            mutation: "frame 2 length := 2^20 + 1".into(),
        });
    }
    {
        let mut bytes = file_bytes.clone();
        bytes[0..8].copy_from_slice(b"ARKFJRN2");
        tampers.push(Tamper {
            title: "a different magic is not a journal this build may append to",
            requirements: vec!["AF-JRN-010"],
            bytes,
            mutation: "magic := ARKFJRN2".into(),
        });
    }
    {
        // A record whose body is valid CBOR but not a record: missing a field.
        let mut journal2 = Journal::new();
        for (kind, at, revision, subject, facts) in sample_records() {
            journal2.append(kind, at, revision, subject, facts).unwrap();
        }
        let original = journal2.records()[0].to_canonical_bytes().unwrap();
        let arkforge_core::digest::CborValue::Map(entries) = decode_canonical(&original).unwrap()
        else {
            unreachable!()
        };
        let without: Vec<_> = entries
            .into_iter()
            .filter(|(k, _)| *k != arkforge_core::digest::CborValue::text("fsyncPolicy"))
            .collect();
        let body = arkforge_core::digest::CborValue::Map(without)
            .to_canonical_bytes()
            .unwrap();
        let bytes = splice_frame(&file_bytes, &frame_offsets, 0, &body);
        tampers.push(Tamper {
            title: "a record missing a field is malformed by name, never defaulted",
            requirements: vec!["AF-JRN-017"],
            bytes,
            mutation: "record 1 without fsyncPolicy".into(),
        });
    }

    for tamper in tampers {
        number += 1;
        let outcome = open_bytes(&scratch, "tamper.bin", &tamper.bytes);
        tree.case(
            &Case {
                id: case_id("JOURNAL", number),
                suite: SUITE,
                title: tamper.title.to_string(),
                requirements: tamper.requirements,
                kind: "replay",
                description: "Open `journal.bin`. A journal that no longer proves its own \
                              history is refused; recovery must not reason from it."
                    .to_string(),
                input: Json::object(vec![
                    ("basedOn", Json::str(&file_case)),
                    ("mutation", Json::str(&tamper.mutation)),
                ]),
                expected: outcome,
            },
            vec![("journal.bin", tamper.bytes)],
        );
    }
}

/// `(start_of_frame, end_of_frame)` byte offsets, after the magic.
fn frame_offsets(file: &[u8]) -> Vec<(usize, usize)> {
    let mut offsets = Vec::new();
    let mut cursor = 8;
    while cursor + 4 <= file.len() {
        let length = u32::from_be_bytes([
            file[cursor],
            file[cursor + 1],
            file[cursor + 2],
            file[cursor + 3],
        ]) as usize;
        let end = cursor + 4 + length;
        offsets.push((cursor, end));
        cursor = end;
    }
    offsets
}

fn splice_frame(file: &[u8], offsets: &[(usize, usize)], index: usize, body: &[u8]) -> Vec<u8> {
    let (start, end) = offsets[index];
    let mut out = file[..start].to_vec();
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(body);
    out.extend_from_slice(&file[end..]);
    out
}
