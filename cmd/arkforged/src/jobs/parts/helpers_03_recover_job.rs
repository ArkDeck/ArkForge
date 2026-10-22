use super::*;

pub(super) fn recover_job(path: &Path) -> Result<Job, JobError> {
    let (mut journal, _) = DurableJournal::open(path)?;
    let records = journal.journal().records();
    let created = records
        .iter()
        .find(|record| record.kind == JournalRecordKind::JobCreated)
        .ok_or_else(|| {
            JobError::RegistryIo(format!("{} has no jobCreated record", path.display()))
        })?;
    let fact_value = |name: &str| -> Option<String> {
        created
            .facts
            .iter()
            .find(|(key, _)| key.as_str() == name)
            .map(|(_, value)| value.clone())
    };
    let job_id = fact_value(fact::JOB_ID)
        .ok_or_else(|| JobError::RegistryIo(format!("{} has no job id", path.display())))?;
    let recovered_receipts = recover_receipt_events(records, &job_id);
    let last_recovered_receipt_sequence = recovered_receipts
        .iter()
        .map(|event| event.sequence)
        .max()
        .unwrap_or(0);
    let plan_id = fact_value(fact::PLAN_ID).unwrap_or_else(|| "PLAN-RECOVERED".to_string());
    let plan_digest = fact_value("planDigest")
        .and_then(|value| Sha256Digest::parse_hex(&value).ok())
        .unwrap_or_else(|| sha256(b"arkforge/recovered/unknown-plan-digest"));
    let controller_session_id = fact_value("controllerSessionId")
        .and_then(|value| ControllerSessionId::new(value).ok())
        .unwrap_or_else(|| {
            ControllerSessionId::new("RECOVERED-SESSION").expect("literal identifier")
        });
    let step_index = records
        .iter()
        .filter(|record| record.kind == JournalRecordKind::StepCheckpointed)
        .count();
    let total_steps = fact_value("totalSteps")
        .and_then(|value| value.parse().ok())
        .unwrap_or(step_index);
    // The first durable terminal classification wins. A terminal result is an
    // immutable fact; a later daemon must never replace it with a restart
    // opinion, even if an older buggy build appended one.
    let first_terminal_classification = records.iter().find(|record| {
        record.kind == JournalRecordKind::OutcomeClassified
            && record.facts.iter().any(|(key, value)| {
                key.as_str() == "outcome"
                    && matches!(
                        value.as_str(),
                        "succeeded" | "cancelledSafe" | "recoveryAssessable" | "confirmedFailed"
                    )
            })
    });
    let first_classification_record = first_terminal_classification.or_else(|| {
        records
            .iter()
            .find(|record| record.kind == JournalRecordKind::OutcomeClassified)
    });
    let first_classification = first_classification_record.and_then(|record| {
        let outcome = record
            .facts
            .iter()
            .find(|(key, _)| key.as_str() == "outcome")
            .map(|(_, value)| value.clone())?;
        let reason = record
            .facts
            .iter()
            .find(|(key, _)| key.as_str() == "reason")
            .map(|(_, value)| value.clone());
        let event_sequence = record
            .facts
            .iter()
            .find(|(key, _)| key.as_str() == "eventSequence")
            .and_then(|(_, value)| value.parse::<u64>().ok())
            .filter(|sequence| *sequence > 0);
        Some((
            outcome,
            reason,
            event_sequence,
            record.record_digest,
            record.at_epoch_ms,
        ))
    });

    let (state, outcome, reason, record_digest, at_epoch_ms, event_sequence) = if let Some((
        outcome,
        reason,
        event_sequence,
        digest,
        at,
    )) =
        first_classification
    {
        let state = match outcome.as_str() {
            "succeeded" => JobState::Succeeded,
            "cancelledSafe" => JobState::CancelledSafe,
            "recoveryAssessable" => JobState::RecoveryAssessable,
            "confirmedFailed" => JobState::ConfirmedFailed,
            _ => JobState::OutcomeUnknown,
        };
        (
            state,
            outcome,
            reason.unwrap_or_else(|| "replayed from durable journal".to_string()),
            digest,
            at,
            event_sequence.unwrap_or_else(|| {
                (journal.len() as u64)
                    .max(last_recovered_receipt_sequence)
                    .saturating_add(1)
            }),
        )
    } else {
        let all_steps_checkpointed = total_steps > 0 && step_index >= total_steps;
        let (state, outcome, reason) = match CrashDisposition::derive(journal.journal(), &job_id) {
            CrashDisposition::NoJob => {
                return Err(JobError::RegistryIo(format!(
                    "{} has no jobCreated record for {job_id}",
                    path.display()
                )));
            }
            CrashDisposition::SafeToCancel => (
                JobState::CancelledSafe,
                "cancelledSafe".to_string(),
                "daemon restarted before any durable external intent existed".to_string(),
            ),
            CrashDisposition::DispatchForbiddenUntilIntentDurable { permit_id } => (
                JobState::CancelledSafe,
                "cancelledSafe".to_string(),
                format!(
                    "daemon restarted after accepting permit {permit_id}, but before its intent became durable; no external effect was possible"
                ),
            ),
            CrashDisposition::OutcomeUnknown { permit_id } => (
                JobState::OutcomeUnknown,
                "outcomeUnknown".to_string(),
                format!(
                    "daemon restarted with durable external intent for permit {permit_id} and no settled receipt"
                ),
            ),
            CrashDisposition::ReceiptDurableCheckpointMissing { permit_id } => (
                JobState::OutcomeUnknown,
                "outcomeUnknown".to_string(),
                format!(
                    "daemon restarted with a durable receipt for permit {permit_id}, but no checkpoint; dispatch will not be replayed"
                ),
            ),
            CrashDisposition::ReplayFromCheckpoint => (
                JobState::OutcomeUnknown,
                "outcomeUnknown".to_string(),
                if all_steps_checkpointed {
                    "daemon restarted after every declared checkpoint, but no durable terminal classification exists; checkpoint count alone cannot distinguish semantic success from a settled failure"
                                .to_string()
                } else {
                    "daemon restarted after a durable checkpoint, but cannot reconstruct the remaining continuation; dispatch will not be replayed"
                                .to_string()
                },
            ),
            CrashDisposition::Concluded(state) => (
                state,
                state.as_str().to_string(),
                "replayed from durable journal".to_string(),
            ),
        };
        let at = records
            .last()
            .map(|record| record.at_epoch_ms.saturating_add(1))
            .unwrap_or(0);
        let event_sequence = (journal.len() as u64)
            .max(last_recovered_receipt_sequence)
            .saturating_add(1);
        let digest = journal.append(
            JournalRecordKind::OutcomeClassified,
            at,
            1,
            id(&job_id)?,
            vec![
                (id("outcome")?, outcome.clone()),
                (id("reason")?, reason.clone()),
                (id("eventSequence")?, event_sequence.to_string()),
            ],
        )?;
        (state, outcome, reason, digest, at, event_sequence)
    };

    let stopped = match state {
        JobState::Succeeded => Some(JobStop::Completed),
        JobState::ConfirmedFailed => Some(JobStop::ConfirmedFailure {
            reason: reason.clone(),
        }),
        JobState::CancelledSafe => Some(JobStop::RefusedByAuthority {
            step_id: "daemon-restart".to_string(),
            reason: reason.clone(),
        }),
        JobState::OutcomeUnknown | JobState::RecoveryAssessable => {
            Some(JobStop::DispatchOutcomeUnknown {
                step_id: "daemon-restart".to_string(),
                disposition: ActionDisposition::OutcomeUnknown,
            })
        }
        _ => None,
    };
    let event = JobEvent {
        job_id: job_id.clone(),
        sequence: event_sequence,
        kind: JobEventKind::OutcomeClassified,
        at_epoch_ms,
        journal_record_sha256: record_digest.as_bytes().to_vec(),
        job_state: state.as_str().to_string(),
        facts: vec![
            KeyValue {
                key: "outcome".to_string(),
                value: outcome,
            },
            KeyValue {
                key: "reason".to_string(),
                value: reason,
            },
        ],
        ..JobEvent::default()
    };
    let mut events = recovered_receipts
        .into_iter()
        .filter(|receipt| receipt.sequence < event_sequence)
        .collect::<Vec<_>>();
    events.push(event);
    events.sort_by_key(|event| event.sequence);
    Ok(Job {
        job_id,
        controller_session_id,
        plan_id,
        plan_digest,
        state,
        step_index,
        total_steps,
        journal,
        events,
        pending: None,
        in_flight: None,
        stopped,
        cancellation_requested: false,
    })
}

#[derive(Debug)]
pub enum JobError {
    UnknownJob,
    PlanHasNoSteps,
    StepHasNoAction(String),
    NoAdmissionPending,
    NoControlPending,
    NoDispatchInFlight,
    NoReconcileInFlight,
    ReconcileNotAvailable(JobState),
    WrongRequest {
        expected: String,
        found: String,
    },
    WrongControlAction {
        expected: ManagedControlAction,
        found: ManagedControlAction,
    },
    SnapshotExpired,
    CurrentFactsUnavailable,
    ContinuityBroken(String),
    AdmissionNotReady,
    ObservedModeMismatch {
        expected: String,
        found: String,
    },
    TagNotADigest,
    ControlEvidenceMismatch,
    ControlFactsIncomplete(Vec<String>),
    InvalidReceiptFactKey(String),
    DuplicateReceiptFactKey(String),
    ReceiptCarriesForbiddenFacts(Vec<String>),
    UnknownControlAction(String),
    CancelWouldHideAnUnresolvedEffect,
    IllegalTransition {
        from: JobState,
        to: JobState,
    },
    Verification(PermitVerificationError),
    Step(StepError),
    Journal(DurableJournalError),
    UnusableIdentifier(String),
    Core(String),
    RegistryIo(String),
}
