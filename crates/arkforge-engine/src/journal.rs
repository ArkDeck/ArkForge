//! The operational journal.
//!
//! architecture.md 13.2. Every record carries a schema version, a sequence, the
//! previous record's digest, its fsync policy and its own digest, so a
//! truncated or edited journal is detectable rather than merely suspicious.
//!
//! The file-backed side lives in [`crate::durable`]; what is here is the record
//! model and the chain check it writes and replays.

use arkforge_core::Sha256Digest;
use arkforge_core::digest::{
    CanonicalCbor, CborError, CborValue, Domain, decode_canonical, digest_canonical,
};
use arkforge_core::ids::OpaqueId;
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

/// When a record has to be on stable storage.
///
/// architecture.md 13.2 requires every record to carry this, and 13.3 is what
/// makes it a safety property rather than a tuning knob: if `StepIntentRecorded`
/// were lost across a crash, recovery could not tell "never dispatched" from
/// "dispatched and the receipt is missing", and the only safe reading of that
/// is the expensive one. So the policy is a function of the kind, not a
/// setting, and a record that claims a weaker policy than its kind requires is
/// treated as tampering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FsyncPolicy {
    /// On stable storage before anything outside this process can observe an
    /// effect that depends on it.
    Durable,
    /// Written and ordered behind the next durable record. Losing it costs
    /// detail in the record, never a decision.
    Buffered,
}

impl FsyncPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            FsyncPolicy::Durable => "durable",
            FsyncPolicy::Buffered => "buffered",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "durable" => Some(FsyncPolicy::Durable),
            "buffered" => Some(FsyncPolicy::Buffered),
            _ => None,
        }
    }
}

impl JournalRecordKind {
    /// The policy this kind demands.
    ///
    /// `Durable` covers every record whose loss would let ArkForge dispatch a
    /// second time, or forget that it dispatched a first. `Buffered` covers
    /// observations: a lost `PreflightObserved` costs an operator some context,
    /// and costs the recovery reasoning nothing.
    pub fn fsync_policy(self) -> FsyncPolicy {
        use JournalRecordKind::*;
        match self {
            PreflightObserved
            | StepAdmissionRequested
            | TransportEvidenceRecorded
            | RebindObserved
            | ReadOnlyObservationRecorded => FsyncPolicy::Buffered,
            PlanStored
            | JobCreated
            | LocalExecutorLeaseAcquired
            | StepPermitAccepted
            | StepIntentRecorded
            | PermitConsuming
            | ExternalDispatchStarted
            | SemanticReceiptRecorded
            | PermitConsumed
            | StepCheckpointed
            | CancellationRequested
            | PostflightRecorded
            | OutcomeClassified
            | PossibleEffectSetRecorded
            | RecoveryAssessmentPublished
            | RecoveryGuidePublished => FsyncPolicy::Durable,
        }
    }

    /// Every kind, so exhaustiveness is testable rather than assumed.
    pub const ALL: [JournalRecordKind; 21] = [
        JournalRecordKind::PlanStored,
        JournalRecordKind::JobCreated,
        JournalRecordKind::LocalExecutorLeaseAcquired,
        JournalRecordKind::PreflightObserved,
        JournalRecordKind::StepAdmissionRequested,
        JournalRecordKind::StepPermitAccepted,
        JournalRecordKind::StepIntentRecorded,
        JournalRecordKind::PermitConsuming,
        JournalRecordKind::ExternalDispatchStarted,
        JournalRecordKind::TransportEvidenceRecorded,
        JournalRecordKind::SemanticReceiptRecorded,
        JournalRecordKind::PermitConsumed,
        JournalRecordKind::StepCheckpointed,
        JournalRecordKind::RebindObserved,
        JournalRecordKind::CancellationRequested,
        JournalRecordKind::PostflightRecorded,
        JournalRecordKind::OutcomeClassified,
        JournalRecordKind::PossibleEffectSetRecorded,
        JournalRecordKind::RecoveryAssessmentPublished,
        JournalRecordKind::RecoveryGuidePublished,
        JournalRecordKind::ReadOnlyObservationRecorded,
    ];

    pub fn parse(text: &str) -> Option<Self> {
        JournalRecordKind::ALL
            .into_iter()
            .find(|kind| kind.as_str() == text)
    }

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
    pub fsync_policy: FsyncPolicy,
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
            ("fsyncPolicy", CborValue::text(self.fsync_policy.as_str())),
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

    /// The record as it is stored: the digested body plus the digest itself.
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, CborError> {
        let CborValue::Map(mut entries) = self.body() else {
            unreachable!("a record body is a map")
        };
        entries.push((
            CborValue::text("recordDigest"),
            self.record_digest.to_cbor(),
        ));
        CborValue::Map(entries).to_canonical_bytes()
    }

    /// Reads a record back. Every field is required; a missing or mistyped one
    /// is a malformed record, never a defaulted one.
    pub fn from_canonical_bytes(input: &[u8]) -> Result<JournalRecord, JournalError> {
        let value = decode_canonical(input).map_err(JournalError::Cbor)?;
        let CborValue::Map(entries) = value else {
            return Err(JournalError::RecordMalformed("record is not a map"));
        };
        let field = |name: &str| -> Option<&CborValue> {
            entries
                .iter()
                .find(|(key, _)| matches!(key, CborValue::Text(text) if text == name))
                .map(|(_, value)| value)
        };
        let unsigned = |name: &'static str| -> Result<u64, JournalError> {
            match field(name) {
                Some(CborValue::Unsigned(value)) => Ok(*value),
                _ => Err(JournalError::RecordMalformed(name)),
            }
        };
        let text = |name: &'static str| -> Result<&str, JournalError> {
            match field(name) {
                Some(CborValue::Text(value)) => Ok(value.as_str()),
                _ => Err(JournalError::RecordMalformed(name)),
            }
        };
        let digest = |name: &'static str| -> Result<Sha256Digest, JournalError> {
            match field(name) {
                Some(CborValue::Bytes(bytes)) if bytes.len() == 32 => {
                    let mut array = [0u8; 32];
                    array.copy_from_slice(bytes);
                    Ok(Sha256Digest::from_bytes(array))
                }
                _ => Err(JournalError::RecordMalformed(name)),
            }
        };

        let schema_version = u32::try_from(unsigned("schemaVersion")?)
            .map_err(|_| JournalError::RecordMalformed("schemaVersion"))?;
        let kind =
            JournalRecordKind::parse(text("kind")?).ok_or(JournalError::RecordMalformed("kind"))?;
        let fsync_policy = FsyncPolicy::parse(text("fsyncPolicy")?)
            .ok_or(JournalError::RecordMalformed("fsyncPolicy"))?;
        let subject = OpaqueId::new(text("subject")?)
            .map_err(|_| JournalError::RecordMalformed("subject"))?;
        let facts = match field("facts") {
            Some(CborValue::Map(pairs)) => pairs
                .iter()
                .map(|(key, value)| match (key, value) {
                    (CborValue::Text(key), CborValue::Text(value)) => OpaqueId::new(key)
                        .map(|key| (key, value.clone()))
                        .map_err(|_| JournalError::RecordMalformed("facts")),
                    _ => Err(JournalError::RecordMalformed("facts")),
                })
                .collect::<Result<Vec<_>, _>>()?,
            _ => return Err(JournalError::RecordMalformed("facts")),
        };

        Ok(JournalRecord {
            schema_version,
            sequence: unsigned("sequence")?,
            job_revision: unsigned("jobRevision")?,
            kind,
            fsync_policy,
            at_epoch_ms: unsigned("atEpochMs")?,
            subject,
            facts,
            previous_digest: digest("previousDigest")?,
            record_digest: digest("recordDigest")?,
        })
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
            fsync_policy: kind.fsync_policy(),
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

    /// Adopts a record read back from storage, after checking it belongs here.
    pub(crate) fn adopt(&mut self, record: JournalRecord) -> Result<(), JournalError> {
        let expected_sequence = self.records.len() as u64 + 1;
        if record.sequence != expected_sequence {
            return Err(JournalError::SequenceBroken {
                expected: expected_sequence,
                found: record.sequence,
            });
        }
        if record.previous_digest != self.head_digest() {
            return Err(JournalError::ChainBroken {
                at_sequence: record.sequence,
            });
        }
        check_record(&record)?;
        self.records.push(record);
        Ok(())
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
            check_record(record)?;
            previous = record.record_digest;
        }
        Ok(())
    }
}

/// Schema version, declared durability and digest, for one record.
fn check_record(record: &JournalRecord) -> Result<(), JournalError> {
    if record.schema_version != SCHEMA_VERSION {
        return Err(JournalError::UnknownSchemaVersion(record.schema_version));
    }
    // The policy is a function of the kind. A record that claims a weaker one
    // is a record whose author wanted a dispatch-relevant fact to be losable.
    if record.fsync_policy != record.kind.fsync_policy() {
        return Err(JournalError::FsyncPolicyMisdeclared {
            at_sequence: record.sequence,
            kind: record.kind,
            declared: record.fsync_policy,
        });
    }
    let recomputed = record.recompute_digest().map_err(JournalError::Cbor)?;
    if recomputed != record.record_digest {
        return Err(JournalError::RecordTampered {
            at_sequence: record.sequence,
        });
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalError {
    SequenceBroken {
        expected: u64,
        found: u64,
    },
    ChainBroken {
        at_sequence: u64,
    },
    RecordTampered {
        at_sequence: u64,
    },
    /// The record declares a durability its kind does not permit.
    FsyncPolicyMisdeclared {
        at_sequence: u64,
        kind: JournalRecordKind,
        declared: FsyncPolicy,
    },
    UnknownSchemaVersion(u32),
    /// A field is absent or has the wrong shape. Named, so a corrupt journal
    /// says which field rather than "invalid".
    RecordMalformed(&'static str),
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
            JournalError::FsyncPolicyMisdeclared {
                at_sequence,
                kind,
                declared,
            } => write!(
                f,
                "journal record {at_sequence} of kind {} declares fsync policy {}, but that kind \
                 requires {}",
                kind.as_str(),
                declared.as_str(),
                kind.fsync_policy().as_str()
            ),
            JournalError::UnknownSchemaVersion(version) => {
                write!(f, "journal record schema version {version} is not readable")
            }
            JournalError::RecordMalformed(field) => {
                write!(f, "journal record field {field:?} is missing or malformed")
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
        assert_eq!(
            journal.records()[1].previous_digest,
            journal.records()[0].record_digest
        );
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
    fn every_kind_is_in_the_all_table_and_round_trips_through_its_name() {
        // `ALL` is what the fsync-policy audit below iterates. A kind missing
        // from it would be a kind whose durability nobody ever checked.
        assert_eq!(
            JournalRecordKind::ALL.len(),
            JournalRecordKind::ALL
                .iter()
                .map(|kind| kind.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .len()
        );
        for kind in JournalRecordKind::ALL {
            assert_eq!(JournalRecordKind::parse(kind.as_str()), Some(kind));
        }
    }

    /// The durability table, asserted rather than described. Buffered is the
    /// short list, and every entry on it is an observation: losing one costs
    /// context in the record and changes no decision in
    /// [`crate::recovery::CrashDisposition`].
    #[test]
    fn only_observations_are_buffered() {
        let buffered: Vec<&str> = JournalRecordKind::ALL
            .into_iter()
            .filter(|kind| kind.fsync_policy() == FsyncPolicy::Buffered)
            .map(|kind| kind.as_str())
            .collect();
        assert_eq!(
            buffered,
            vec![
                "preflightObserved",
                "stepAdmissionRequested",
                "transportEvidenceRecorded",
                "rebindObserved",
                "readOnlyObservationRecorded",
            ]
        );
    }

    #[test]
    fn a_record_that_downgrades_its_own_durability_is_refused() {
        let mut journal = journal_with_three();
        // Sequence 1 is `PlanStored`, which is durable.
        journal.records[0].fsync_policy = FsyncPolicy::Buffered;
        journal.records[0].record_digest = journal.records[0].recompute_digest().unwrap();
        assert_eq!(
            journal.verify(),
            Err(JournalError::FsyncPolicyMisdeclared {
                at_sequence: 1,
                kind: JournalRecordKind::PlanStored,
                declared: FsyncPolicy::Buffered,
            })
        );
    }

    #[test]
    fn a_record_round_trips_through_its_stored_bytes() {
        let journal = journal_with_three();
        for record in journal.records() {
            let bytes = record.to_canonical_bytes().unwrap();
            assert_eq!(
                &JournalRecord::from_canonical_bytes(&bytes).unwrap(),
                record
            );
        }
    }

    #[test]
    fn a_record_missing_a_field_is_named_rather_than_defaulted() {
        let journal = journal_with_three();
        let bytes = journal.records()[0].to_canonical_bytes().unwrap();
        let CborValue::Map(entries) = decode_canonical(&bytes).unwrap() else {
            panic!("a record is a map")
        };
        let without_policy: Vec<_> = entries
            .into_iter()
            .filter(|(key, _)| !matches!(key, CborValue::Text(text) if text == "fsyncPolicy"))
            .collect();
        let bytes = CborValue::Map(without_policy).to_canonical_bytes().unwrap();
        assert_eq!(
            JournalRecord::from_canonical_bytes(&bytes),
            Err(JournalError::RecordMalformed("fsyncPolicy"))
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
