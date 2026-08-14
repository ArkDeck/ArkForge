//! The operational journal.
//!
//! architecture.md 13.2. Every record carries a schema version, a sequence, the
//! previous record's digest and its own, so a truncated or edited journal is
//! detectable rather than merely suspicious.
//!
//! Scope note: fsync policy and the crash campaign that proves it are AF-V2.
//! What is here is the record model and the chain check, which AF-V1 uses to
//! record read-only observations and which AF-V2 builds durability on.

use arkforge_core::digest::{digest_canonical, CanonicalCbor, CborError, CborValue, Domain};
use arkforge_core::ids::OpaqueId;
use arkforge_core::Sha256Digest;
use core::fmt;

pub const SCHEMA_VERSION: u32 = 1;

/// The record kinds architecture.md 13.2 enumerates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum JournalRecordKind {
    PlanStored,
    JobCreated,
    LocalExecutorLeaseAcquired,
    PreflightObserved,
    StepAdmissionRequested,
    StepPermitAccepted,
    StepIntentRecorded,
    PermitConsuming,
    ExternalDispatchStarted,
    TransportEvidenceRecorded,
    SemanticReceiptRecorded,
    PermitConsumed,
    StepCheckpointed,
    RebindObserved,
    CancellationRequested,
    PostflightRecorded,
    OutcomeClassified,
    PossibleEffectSetRecorded,
    RecoveryAssessmentPublished,
    RecoveryGuidePublished,
    /// A read-only observation, which is all AF-V1 produces.
    ReadOnlyObservationRecorded,
}

impl JournalRecordKind {
    pub fn as_str(self) -> &'static str {
        match self {
            JournalRecordKind::PlanStored => "planStored",
            JournalRecordKind::JobCreated => "jobCreated",
            JournalRecordKind::LocalExecutorLeaseAcquired => "localExecutorLeaseAcquired",
            JournalRecordKind::PreflightObserved => "preflightObserved",
            JournalRecordKind::StepAdmissionRequested => "stepAdmissionRequested",
            JournalRecordKind::StepPermitAccepted => "stepPermitAccepted",
            JournalRecordKind::StepIntentRecorded => "stepIntentRecorded",
            JournalRecordKind::PermitConsuming => "permitConsuming",
            JournalRecordKind::ExternalDispatchStarted => "externalDispatchStarted",
            JournalRecordKind::TransportEvidenceRecorded => "transportEvidenceRecorded",
            JournalRecordKind::SemanticReceiptRecorded => "semanticReceiptRecorded",
            JournalRecordKind::PermitConsumed => "permitConsumed",
            JournalRecordKind::StepCheckpointed => "stepCheckpointed",
            JournalRecordKind::RebindObserved => "rebindObserved",
            JournalRecordKind::CancellationRequested => "cancellationRequested",
            JournalRecordKind::PostflightRecorded => "postflightRecorded",
            JournalRecordKind::OutcomeClassified => "outcomeClassified",
            JournalRecordKind::PossibleEffectSetRecorded => "possibleEffectSetRecorded",
            JournalRecordKind::RecoveryAssessmentPublished => "recoveryAssessmentPublished",
            JournalRecordKind::RecoveryGuidePublished => "recoveryGuidePublished",
            JournalRecordKind::ReadOnlyObservationRecorded => "readOnlyObservationRecorded",
        }
    }
}

/// One journal record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalRecord {
    pub schema_version: u32,
    pub sequence: u64,
    pub job_revision: u64,
    pub kind: JournalRecordKind,
    pub at_epoch_ms: u64,
    pub subject: OpaqueId,
    pub facts: Vec<(OpaqueId, String)>,
    pub previous_digest: Sha256Digest,
    pub record_digest: Sha256Digest,
}

impl JournalRecord {
    /// The bytes the record digest covers: everything but the digest itself.
    fn body(&self) -> CborValue {
        CborValue::map(vec![
            (
                "schemaVersion",
                CborValue::Unsigned(self.schema_version as u64),
            ),
            ("sequence", CborValue::Unsigned(self.sequence)),
            ("jobRevision", CborValue::Unsigned(self.job_revision)),
            ("kind", CborValue::text(self.kind.as_str())),
            ("atEpochMs", CborValue::Unsigned(self.at_epoch_ms)),
            ("subject", self.subject.to_cbor()),
            (
                "facts",
                CborValue::Map(
                    self.facts
                        .iter()
                        .map(|(key, value)| (key.to_cbor(), CborValue::text(value.clone())))
                        .collect(),
                ),
            ),
            ("previousDigest", self.previous_digest.to_cbor()),
        ])
    }

    pub fn recompute_digest(&self) -> Result<Sha256Digest, CborError> {
        digest_canonical(Domain::JournalRecord, &BodyView(self.body()))
    }
}

struct BodyView(CborValue);

impl CanonicalCbor for BodyView {
    fn to_cbor(&self) -> CborValue {
        self.0.clone()
    }
}

/// An append-only, hash-chained journal.
#[derive(Debug, Default)]
pub struct Journal {
    records: Vec<JournalRecord>,
}

/// The digest a chain starts from, so the first record is covered too.
pub const CHAIN_ORIGIN: [u8; 32] = [0u8; 32];

impl Journal {
    pub fn new() -> Self {
        Journal {
            records: Vec::new(),
        }
    }

    pub fn head_digest(&self) -> Sha256Digest {
        self.records
            .last()
            .map(|record| record.record_digest)
            .unwrap_or(Sha256Digest::from_bytes(CHAIN_ORIGIN))
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn records(&self) -> &[JournalRecord] {
        &self.records
    }

    /// Appends a record, computing its sequence, chain link and digest.
    pub fn append(
        &mut self,
        kind: JournalRecordKind,
        at_epoch_ms: u64,
        job_revision: u64,
        subject: OpaqueId,
        facts: Vec<(OpaqueId, String)>,
    ) -> Result<&JournalRecord, JournalError> {
        let mut record = JournalRecord {
            schema_version: SCHEMA_VERSION,
            sequence: self.records.len() as u64 + 1,
            job_revision,
            kind,
            at_epoch_ms,
            subject,
            facts,
            previous_digest: self.head_digest(),
            record_digest: Sha256Digest::from_bytes(CHAIN_ORIGIN),
        };
        record.record_digest = record.recompute_digest().map_err(JournalError::Cbor)?;
        self.records.push(record);
        Ok(self.records.last().expect("just pushed"))
    }

    /// Verifies sequence, links and digests over the whole chain.
    pub fn verify(&self) -> Result<(), JournalError> {
        let mut previous = Sha256Digest::from_bytes(CHAIN_ORIGIN);
        for (index, record) in self.records.iter().enumerate() {
            let expected_sequence = index as u64 + 1;
            if record.sequence != expected_sequence {
                return Err(JournalError::SequenceBroken {
                    expected: expected_sequence,
                    found: record.sequence,
                });
            }
            if record.schema_version != SCHEMA_VERSION {
                return Err(JournalError::UnknownSchemaVersion(record.schema_version));
            }
            if record.previous_digest != previous {
                return Err(JournalError::ChainBroken {
                    at_sequence: record.sequence,
                });
            }
            let recomputed = record.recompute_digest().map_err(JournalError::Cbor)?;
            if recomputed != record.record_digest {
                return Err(JournalError::RecordTampered {
                    at_sequence: record.sequence,
                });
            }
            previous = record.record_digest;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalError {
    SequenceBroken { expected: u64, found: u64 },
    ChainBroken { at_sequence: u64 },
    RecordTampered { at_sequence: u64 },
    UnknownSchemaVersion(u32),
    Cbor(CborError),
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JournalError::SequenceBroken { expected, found } => {
                write!(f, "journal sequence {found} where {expected} was expected")
            }
            JournalError::ChainBroken { at_sequence } => {
                write!(f, "journal chain link broken at record {at_sequence}")
            }
            JournalError::RecordTampered { at_sequence } => {
                write!(f, "journal record {at_sequence} does not match its digest")
            }
            JournalError::UnknownSchemaVersion(version) => {
                write!(f, "journal record schema version {version} is not readable")
            }
            JournalError::Cbor(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for JournalError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn subject(value: &str) -> OpaqueId {
        OpaqueId::new(value).unwrap()
    }

    fn journal_with_three() -> Journal {
        let mut journal = Journal::new();
        journal
            .append(
                JournalRecordKind::PlanStored,
                1_000,
                1,
                subject("PLAN-1"),
                vec![],
            )
            .unwrap();
        journal
            .append(
                JournalRecordKind::ReadOnlyObservationRecorded,
                2_000,
                1,
                subject("OBS-1"),
                vec![(subject("mode"), "hdc-normal".into())],
            )
            .unwrap();
        journal
            .append(
                JournalRecordKind::PreflightObserved,
                3_000,
                1,
                subject("JOB-1"),
                vec![],
            )
            .unwrap();
        journal
    }

    #[test]
    fn a_well_formed_chain_verifies() {
        let journal = journal_with_three();
        journal.verify().unwrap();
        assert_eq!(journal.len(), 3);
        assert_eq!(journal.records()[1].previous_digest, journal.records()[0].record_digest);
    }

    #[test]
    fn editing_a_record_breaks_its_digest() {
        let mut journal = journal_with_three();
        journal.records[1].at_epoch_ms = 9_999;
        assert_eq!(
            journal.verify(),
            Err(JournalError::RecordTampered { at_sequence: 2 })
        );
    }

    #[test]
    fn removing_a_record_breaks_the_chain() {
        let mut journal = journal_with_three();
        journal.records.remove(1);
        // The sequence check fires first, which is the more specific fact.
        assert_eq!(
            journal.verify(),
            Err(JournalError::SequenceBroken {
                expected: 2,
                found: 3
            })
        );
    }

    #[test]
    fn re_linking_a_record_after_an_edit_still_fails() {
        // An attacker who fixes the sequence must also produce a matching
        // digest, and the digest covers the previous link.
        let mut journal = journal_with_three();
        journal.records[2].previous_digest = journal.records[0].record_digest;
        journal.records[2].record_digest = journal.records[2].recompute_digest().unwrap();
        assert_eq!(
            journal.verify(),
            Err(JournalError::ChainBroken { at_sequence: 3 })
        );
    }

    #[test]
    fn the_first_record_is_linked_to_the_chain_origin() {
        let journal = journal_with_three();
        assert_eq!(
            journal.records()[0].previous_digest,
            Sha256Digest::from_bytes(CHAIN_ORIGIN)
        );
    }

    #[test]
    fn an_unreadable_schema_version_fails_closed() {
        let mut journal = journal_with_three();
        journal.records[0].schema_version = 99;
        journal.records[0].record_digest = journal.records[0].recompute_digest().unwrap();
        assert_eq!(
            journal.verify(),
            Err(JournalError::UnknownSchemaVersion(99))
        );
    }
}
