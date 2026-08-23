//! What a replayed journal says about a job that did not finish.
//!
//! architecture.md 13.3 is a table of crash windows and the one correct
//! handling for each. This module derives the row from the journal rather than
//! leaving it to a caller's judgement, because the rows differ mainly in what
//! is *forbidden*, and a caller reasoning from "the last thing I remember"
//! reliably picks the cheap row over the correct one.
//!
//! The rule underneath all of them: never replay a dispatch (architecture.md
//! 14.1). Nothing here ever concludes "try that step again".

use crate::JobState;
use crate::journal::{Journal, JournalRecord, JournalRecordKind};
use arkforge_core::ids::OpaqueId;
use core::fmt;
use std::collections::BTreeMap;

/// Fact keys the engine writes and recovery reads. Shared so a writer and a
/// reader cannot drift into two spellings of the same fact.
pub mod fact {
    pub const JOB_ID: &str = "jobId";
    pub const PLAN_ID: &str = "planId";
    pub const STEP_ID: &str = "stepId";
    pub const ATTEMPT_ID: &str = "attemptId";
    pub const PERMIT_ID: &str = "permitId";
    pub const RECEIPT_DIGEST: &str = "receiptDigest";
}

/// Where a single permit stands.
///
/// architecture.md 8.5: a permit is single-use, and "single" has to survive a
/// crash or it means nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermitDisposition {
    /// Nothing in the journal mentions it. Admission may proceed.
    Unseen,
    /// Accepted, but no step intent is durable. Dispatch is forbidden: record
    /// the same intent under the same permit id, or let the permit expire.
    /// Creating a second intent for one permit is the failure this prevents.
    AcceptedIntentNotDurable,
    /// An intent is durable and consumption has not started. This permit may be
    /// consumed exactly once.
    IntentDurable,
    /// Consumption started and no receipt is durable. Whether the device was
    /// touched is unknown, and no amount of retrying makes it known
    /// (architecture.md 14.1).
    ConsumingOutcomeUnknown,
    /// Consumed, with a receipt. Return that receipt; do not dispatch again.
    Consumed { receipt_digest: String },
}

impl PermitDisposition {
    /// Whether a fresh external dispatch may be made under this permit.
    pub fn permits_dispatch(&self) -> bool {
        matches!(self, PermitDisposition::IntentDurable)
    }
}

/// Every permit the journal has ever mentioned.
#[derive(Debug, Clone, Default)]
pub struct PermitLedger {
    permits: BTreeMap<String, PermitDisposition>,
}

impl PermitLedger {
    /// Replays the journal into per-permit dispositions.
    pub fn from_journal(journal: &Journal) -> Self {
        Self::from_records(journal.records().iter())
    }

    fn from_records<'a>(records: impl IntoIterator<Item = &'a JournalRecord>) -> Self {
        let mut permits: BTreeMap<String, PermitDisposition> = BTreeMap::new();
        for record in records {
            let Some(permit_id) = fact_value(record.facts.as_slice(), fact::PERMIT_ID) else {
                continue;
            };
            let entry = permits
                .entry(permit_id.to_string())
                .or_insert(PermitDisposition::Unseen);
            match record.kind {
                JournalRecordKind::StepPermitAccepted => {
                    if matches!(entry, PermitDisposition::Unseen) {
                        *entry = PermitDisposition::AcceptedIntentNotDurable;
                    }
                }
                JournalRecordKind::StepIntentRecorded => {
                    if matches!(
                        entry,
                        PermitDisposition::Unseen
                            | PermitDisposition::AcceptedIntentNotDurable
                            | PermitDisposition::IntentDurable
                    ) {
                        *entry = PermitDisposition::IntentDurable;
                    }
                }
                JournalRecordKind::PermitConsuming | JournalRecordKind::ExternalDispatchStarted => {
                    if !matches!(entry, PermitDisposition::Consumed { .. }) {
                        *entry = PermitDisposition::ConsumingOutcomeUnknown;
                    }
                }
                // The semantic receipt is the durable proof that settles the
                // external effect. `permitConsumed` is the following marker,
                // but a crash between those two durable appends must still be
                // recovered from the receipt rather than called unknown.
                JournalRecordKind::SemanticReceiptRecorded | JournalRecordKind::PermitConsumed
                    if !matches!(entry, PermitDisposition::Consumed { .. }) =>
                {
                    match fact_value(record.facts.as_slice(), fact::RECEIPT_DIGEST)
                        .filter(|digest| !digest.is_empty())
                    {
                        Some(receipt) => {
                            *entry = PermitDisposition::Consumed {
                                receipt_digest: receipt.to_string(),
                            };
                        }
                        None => {
                            // A record that claims a receipt but cannot
                            // name it is not proof of a settled effect.
                            *entry = PermitDisposition::ConsumingOutcomeUnknown;
                        }
                    }
                }
                _ => {}
            }
        }
        PermitLedger { permits }
    }

    pub fn disposition(&self, permit_id: &str) -> PermitDisposition {
        self.permits
            .get(permit_id)
            .cloned()
            .unwrap_or(PermitDisposition::Unseen)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &PermitDisposition)> {
        self.permits.iter()
    }

    /// Permits whose durable intent may have reached an external effect and
    /// whose outcome the journal cannot settle.
    ///
    /// Acceptance without an intent is not an external effect. Conversely, a
    /// durable intent is unresolved even before `permitConsuming`: recovery
    /// cannot use process memory to prove that dispatch had not started.
    pub fn unresolved(&self) -> Vec<&String> {
        self.permits
            .iter()
            .filter(|(_, disposition)| {
                matches!(
                    disposition,
                    PermitDisposition::IntentDurable | PermitDisposition::ConsumingOutcomeUnknown
                )
            })
            .map(|(id, _)| id)
            .collect()
    }
}

/// The row of architecture.md 13.3 that applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrashDisposition {
    /// No job was created. Plan and start again; nothing happened.
    NoJob,
    /// Before any permit. Safe to pause, safe to cancel.
    SafeToCancel,
    /// A permit was accepted and no intent is durable. Dispatch is forbidden.
    /// The same permit id may be re-recorded; a second intent may not be
    /// created for it.
    DispatchForbiddenUntilIntentDurable { permit_id: String },
    /// An intent is durable, or a dispatch started, and no semantic receipt
    /// followed. Whether the device was touched is unknown.
    OutcomeUnknown { permit_id: String },
    /// A receipt is durable and no checkpoint followed. The permit is settled,
    /// but the job is not terminal: replay a validated receipt and reconcile;
    /// never infer a checkpoint, continuation, or success from the marker alone.
    ReceiptDurableCheckpointMissing { permit_id: String },
    /// Checkpointed but not concluded. Replay events to the authority and carry
    /// on with read-only postflight. Do not re-execute.
    ReplayFromCheckpoint,
    /// The job reached a terminal state before the crash.
    Concluded(JobState),
}

impl CrashDisposition {
    /// Whether this disposition permits any new external effect at all.
    ///
    /// Only one row does, and even that one is "finish recording the intent",
    /// not "dispatch again".
    pub fn permits_external_effect(&self) -> bool {
        false
    }

    /// Derives the row for `job_id` from the journal.
    pub fn derive(journal: &Journal, job_id: &str) -> CrashDisposition {
        // Current daemon journals are one file per job, but older records did
        // not all carry a jobId fact. In a single-job journal those untagged
        // records belong to that job. In a combined journal an explicit jobId
        // is authoritative and must never be overridden by a coincidentally
        // matching subject (the subject of a step-scoped record is often the
        // step id, not the job id).
        let created_records: Vec<_> = journal
            .records()
            .iter()
            .filter(|record| record.kind == JournalRecordKind::JobCreated)
            .collect();
        let is_single_job_journal = created_records.len() == 1
            && match fact_value(created_records[0].facts.as_slice(), fact::JOB_ID) {
                Some(created_job_id) => created_job_id == job_id,
                None => created_records[0].subject.as_str() == job_id,
            };
        let records: Vec<_> = journal
            .records()
            .iter()
            .filter(
                |record| match fact_value(record.facts.as_slice(), fact::JOB_ID) {
                    Some(record_job_id) => record_job_id == job_id,
                    None => record.subject.as_str() == job_id || is_single_job_journal,
                },
            )
            .collect();

        if !records
            .iter()
            .any(|record| record.kind == JournalRecordKind::JobCreated)
        {
            return CrashDisposition::NoJob;
        }

        // A terminal classification is immutable. If an older buggy build
        // appended a second opinion, replay keeps the first durable terminal
        // fact rather than allowing history to be rewritten.
        if let Some(state) = records.iter().find_map(|record| match record.kind {
            JournalRecordKind::OutcomeClassified => {
                fact_value(record.facts.as_slice(), "outcome").and_then(terminal_state)
            }
            JournalRecordKind::CancellationRequested => None,
            _ => None,
        }) {
            return CrashDisposition::Concluded(state);
        }

        // Jobs admit steps serially, so the permit named by the newest record
        // is the current step and decides the row. Earlier permits must already
        // have checkpointed before a later permit can exist.
        let ledger = PermitLedger::from_records(records.iter().copied());
        let mut newest_permit: Option<&str> = None;
        for record in records.iter().rev() {
            if let Some(permit_id) = fact_value(record.facts.as_slice(), fact::PERMIT_ID) {
                newest_permit = Some(permit_id);
                break;
            }
        }

        let Some(permit_id) = newest_permit else {
            return CrashDisposition::SafeToCancel;
        };

        let checkpointed = records.iter().any(|record| {
            record.kind == JournalRecordKind::StepCheckpointed
                && fact_value(record.facts.as_slice(), fact::PERMIT_ID) == Some(permit_id)
        });

        match ledger.disposition(permit_id) {
            PermitDisposition::Unseen => CrashDisposition::SafeToCancel,
            PermitDisposition::AcceptedIntentNotDurable => {
                CrashDisposition::DispatchForbiddenUntilIntentDurable {
                    permit_id: permit_id.to_string(),
                }
            }
            // An intent that is durable and never consumed is the same crash
            // window as one consumed without a receipt: from the journal alone,
            // "about to dispatch" and "dispatched" are indistinguishable.
            PermitDisposition::IntentDurable | PermitDisposition::ConsumingOutcomeUnknown => {
                CrashDisposition::OutcomeUnknown {
                    permit_id: permit_id.to_string(),
                }
            }
            PermitDisposition::Consumed { .. } if checkpointed => {
                CrashDisposition::ReplayFromCheckpoint
            }
            PermitDisposition::Consumed { .. } => {
                CrashDisposition::ReceiptDurableCheckpointMissing {
                    permit_id: permit_id.to_string(),
                }
            }
        }
    }
}

impl fmt::Display for CrashDisposition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CrashDisposition::NoJob => f.write_str("no job was created; plan and start again"),
            CrashDisposition::SafeToCancel => {
                f.write_str("no permit was accepted; the job is safe to cancel")
            }
            CrashDisposition::DispatchForbiddenUntilIntentDurable { permit_id } => write!(
                f,
                "permit {permit_id} was accepted but no step intent is durable; dispatch is \
                 forbidden and a second intent must not be created"
            ),
            CrashDisposition::OutcomeUnknown { permit_id } => write!(
                f,
                "permit {permit_id} has a durable intent and no semantic receipt; the outcome is \
                 unknown and must be reconciled, never replayed"
            ),
            CrashDisposition::ReceiptDurableCheckpointMissing { permit_id } => write!(
                f,
                "permit {permit_id} has a durable receipt and no checkpoint; replay only a \
                 validated receipt and reconcile without re-executing or inferring success"
            ),
            CrashDisposition::ReplayFromCheckpoint => f.write_str(
                "the step is checkpointed; replay events to the authority without re-executing",
            ),
            CrashDisposition::Concluded(state) => {
                write!(f, "the job concluded as {}", state.as_str())
            }
        }
    }
}

fn fact_value<'a>(facts: &'a [(OpaqueId, String)], key: &str) -> Option<&'a str> {
    facts
        .iter()
        .find(|(fact_key, _)| fact_key.as_str() == key)
        .map(|(_, value)| value.as_str())
}

fn terminal_state(text: &str) -> Option<JobState> {
    match text {
        "succeeded" => Some(JobState::Succeeded),
        "confirmedFailed" => Some(JobState::ConfirmedFailed),
        "cancelledSafe" => Some(JobState::CancelledSafe),
        "recoveryAssessable" => Some(JobState::RecoveryAssessable),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> OpaqueId {
        OpaqueId::new(value).unwrap()
    }

    struct Builder {
        journal: Journal,
        clock: u64,
    }

    impl Builder {
        fn new() -> Self {
            Builder {
                journal: Journal::new(),
                clock: 1_000,
            }
        }

        fn record(mut self, kind: JournalRecordKind, facts: &[(&str, &str)]) -> Self {
            self.clock += 10;
            let facts = facts
                .iter()
                .map(|(key, value)| (id(key), (*value).to_string()))
                .collect();
            self.journal
                .append(kind, self.clock, 1, id("JOB-1"), facts)
                .unwrap();
            self
        }

        fn created(self) -> Self {
            self.record(JournalRecordKind::JobCreated, &[])
        }

        fn accepted(self) -> Self {
            self.record(
                JournalRecordKind::StepPermitAccepted,
                &[(fact::PERMIT_ID, "PERMIT-1"), (fact::STEP_ID, "STEP-1")],
            )
        }

        fn intent(self) -> Self {
            self.record(
                JournalRecordKind::StepIntentRecorded,
                &[(fact::PERMIT_ID, "PERMIT-1"), (fact::STEP_ID, "STEP-1")],
            )
        }

        fn consuming(self) -> Self {
            self.record(
                JournalRecordKind::PermitConsuming,
                &[(fact::PERMIT_ID, "PERMIT-1")],
            )
        }

        fn receipt(self) -> Self {
            self.record(
                JournalRecordKind::SemanticReceiptRecorded,
                &[
                    (fact::PERMIT_ID, "PERMIT-1"),
                    (fact::RECEIPT_DIGEST, "abc123"),
                ],
            )
        }

        fn consumed(self) -> Self {
            self.record(
                JournalRecordKind::PermitConsumed,
                &[
                    (fact::PERMIT_ID, "PERMIT-1"),
                    (fact::RECEIPT_DIGEST, "abc123"),
                ],
            )
        }

        fn checkpointed(self) -> Self {
            self.record(
                JournalRecordKind::StepCheckpointed,
                &[(fact::PERMIT_ID, "PERMIT-1")],
            )
        }

        fn disposition(&self) -> CrashDisposition {
            CrashDisposition::derive(&self.journal, "JOB-1")
        }
    }

    #[test]
    fn a_journal_with_no_job_says_nothing_happened() {
        let builder = Builder::new();
        assert_eq!(builder.disposition(), CrashDisposition::NoJob);
    }

    #[test]
    fn a_job_with_no_permit_is_safe_to_cancel() {
        let builder = Builder::new().created();
        assert_eq!(builder.disposition(), CrashDisposition::SafeToCancel);
    }

    #[test]
    fn an_accepted_permit_without_a_durable_intent_forbids_dispatch() {
        let builder = Builder::new().created().accepted();
        assert_eq!(
            builder.disposition(),
            CrashDisposition::DispatchForbiddenUntilIntentDurable {
                permit_id: "PERMIT-1".into()
            }
        );
        assert!(
            !builder
                .journal
                .records()
                .iter()
                .any(|record| record.kind == JournalRecordKind::ExternalDispatchStarted)
        );
    }

    #[test]
    fn a_durable_intent_without_a_receipt_is_an_unknown_outcome() {
        for builder in [
            Builder::new().created().accepted().intent(),
            Builder::new().created().accepted().intent().consuming(),
        ] {
            assert_eq!(
                builder.disposition(),
                CrashDisposition::OutcomeUnknown {
                    permit_id: "PERMIT-1".into()
                }
            );
        }
    }

    #[test]
    fn a_durable_receipt_without_a_checkpoint_is_settled_but_not_terminal() {
        for builder in [
            Builder::new()
                .created()
                .accepted()
                .intent()
                .consuming()
                .receipt(),
            Builder::new()
                .created()
                .accepted()
                .intent()
                .consuming()
                .receipt()
                .consumed(),
        ] {
            assert_eq!(
                builder.disposition(),
                CrashDisposition::ReceiptDurableCheckpointMissing {
                    permit_id: "PERMIT-1".into()
                }
            );
        }
    }

    #[test]
    fn a_checkpointed_step_is_replayed_not_re_executed() {
        let builder = Builder::new()
            .created()
            .accepted()
            .intent()
            .consuming()
            .receipt()
            .consumed()
            .checkpointed();
        assert_eq!(
            builder.disposition(),
            CrashDisposition::ReplayFromCheckpoint
        );
    }

    #[test]
    fn the_first_durable_terminal_classification_is_immutable() {
        let builder = Builder::new()
            .created()
            .record(
                JournalRecordKind::OutcomeClassified,
                &[("outcome", "succeeded")],
            )
            .record(
                JournalRecordKind::OutcomeClassified,
                &[("outcome", "cancelledSafe")],
            );
        assert_eq!(
            builder.disposition(),
            CrashDisposition::Concluded(JobState::Succeeded)
        );
    }

    #[test]
    fn no_disposition_permits_a_new_external_effect() {
        for disposition in [
            CrashDisposition::NoJob,
            CrashDisposition::SafeToCancel,
            CrashDisposition::DispatchForbiddenUntilIntentDurable {
                permit_id: "PERMIT-1".into(),
            },
            CrashDisposition::OutcomeUnknown {
                permit_id: "PERMIT-1".into(),
            },
            CrashDisposition::ReceiptDurableCheckpointMissing {
                permit_id: "PERMIT-1".into(),
            },
            CrashDisposition::ReplayFromCheckpoint,
            CrashDisposition::Concluded(JobState::Succeeded),
        ] {
            assert!(!disposition.permits_external_effect(), "{disposition:?}");
        }
    }

    #[test]
    fn a_consumed_permit_is_never_dispatchable_again() {
        let builder = Builder::new()
            .created()
            .accepted()
            .intent()
            .consuming()
            .receipt()
            .consumed();
        let ledger = PermitLedger::from_journal(&builder.journal);
        assert_eq!(
            ledger.disposition("PERMIT-1"),
            PermitDisposition::Consumed {
                receipt_digest: "abc123".into()
            }
        );
        assert!(!ledger.disposition("PERMIT-1").permits_dispatch());
        assert!(ledger.unresolved().is_empty());
    }

    #[test]
    fn a_permit_caught_mid_consumption_is_reported_as_unresolved() {
        let builder = Builder::new().created().accepted().intent().consuming();
        let ledger = PermitLedger::from_journal(&builder.journal);
        assert_eq!(
            ledger.disposition("PERMIT-1"),
            PermitDisposition::ConsumingOutcomeUnknown
        );
        assert!(!ledger.disposition("PERMIT-1").permits_dispatch());
        assert_eq!(ledger.unresolved(), vec![&"PERMIT-1".to_string()]);
    }

    #[test]
    fn acceptance_without_intent_is_not_an_unresolved_external_effect() {
        let builder = Builder::new().created().accepted();
        let ledger = PermitLedger::from_journal(&builder.journal);
        assert_eq!(
            ledger.disposition("PERMIT-1"),
            PermitDisposition::AcceptedIntentNotDurable
        );
        assert!(ledger.unresolved().is_empty());
    }

    #[test]
    fn durable_intent_is_unresolved_before_consumption_is_recorded() {
        let builder = Builder::new().created().accepted().intent();
        let ledger = PermitLedger::from_journal(&builder.journal);
        assert_eq!(
            ledger.disposition("PERMIT-1"),
            PermitDisposition::IntentDurable
        );
        assert_eq!(ledger.unresolved(), vec![&"PERMIT-1".to_string()]);
    }

    #[test]
    fn a_semantic_receipt_settles_the_ledger_before_permit_consumed() {
        let builder = Builder::new()
            .created()
            .accepted()
            .intent()
            .consuming()
            .receipt();
        let ledger = PermitLedger::from_journal(&builder.journal);
        assert_eq!(
            ledger.disposition("PERMIT-1"),
            PermitDisposition::Consumed {
                receipt_digest: "abc123".into()
            }
        );
        assert!(ledger.unresolved().is_empty());
    }

    #[test]
    fn a_receipt_marker_without_its_digest_fails_closed_as_unresolved() {
        let builder = Builder::new()
            .created()
            .accepted()
            .intent()
            .consuming()
            .record(
                JournalRecordKind::SemanticReceiptRecorded,
                &[(fact::PERMIT_ID, "PERMIT-1")],
            );
        let ledger = PermitLedger::from_journal(&builder.journal);
        assert_eq!(
            ledger.disposition("PERMIT-1"),
            PermitDisposition::ConsumingOutcomeUnknown
        );
        assert_eq!(ledger.unresolved(), vec![&"PERMIT-1".to_string()]);
    }

    #[test]
    fn a_consumed_permit_cannot_be_downgraded_by_later_records() {
        let builder = Builder::new()
            .created()
            .accepted()
            .intent()
            .consuming()
            .receipt()
            .consumed()
            .record(
                JournalRecordKind::StepIntentRecorded,
                &[(fact::PERMIT_ID, "PERMIT-1")],
            );
        let ledger = PermitLedger::from_journal(&builder.journal);
        assert_eq!(
            ledger.disposition("PERMIT-1"),
            PermitDisposition::Consumed {
                receipt_digest: "abc123".into()
            }
        );
    }

    #[test]
    fn explicit_job_id_overrides_a_matching_subject() {
        let mut journal = Journal::new();
        journal
            .append(
                JournalRecordKind::JobCreated,
                1_010,
                1,
                id("JOB-1"),
                vec![(id(fact::JOB_ID), "JOB-1".into())],
            )
            .unwrap();
        journal
            .append(
                JournalRecordKind::StepPermitAccepted,
                1_020,
                1,
                id("JOB-1"),
                vec![
                    (id(fact::JOB_ID), "JOB-2".into()),
                    (id(fact::PERMIT_ID), "PERMIT-OTHER".into()),
                ],
            )
            .unwrap();

        assert_eq!(
            CrashDisposition::derive(&journal, "JOB-1"),
            CrashDisposition::SafeToCancel
        );
    }

    #[test]
    fn untagged_legacy_step_records_belong_to_the_sole_job_file() {
        let mut journal = Journal::new();
        journal
            .append(
                JournalRecordKind::JobCreated,
                1_010,
                1,
                id("JOB-1"),
                vec![(id(fact::JOB_ID), "JOB-1".into())],
            )
            .unwrap();
        journal
            .append(
                JournalRecordKind::StepPermitAccepted,
                1_020,
                1,
                id("STEP-1"),
                vec![(id(fact::PERMIT_ID), "PERMIT-1".into())],
            )
            .unwrap();

        assert_eq!(
            CrashDisposition::derive(&journal, "JOB-1"),
            CrashDisposition::DispatchForbiddenUntilIntentDurable {
                permit_id: "PERMIT-1".into()
            }
        );
    }

    #[test]
    fn an_unseen_permit_is_the_only_thing_admission_may_treat_as_fresh() {
        let ledger = PermitLedger::from_journal(&Builder::new().created().journal);
        assert_eq!(ledger.disposition("PERMIT-9"), PermitDisposition::Unseen);
        assert!(!PermitDisposition::Unseen.permits_dispatch());
    }
}
