use super::*;

impl JobRegistry {
    /// Enters the read-only reconciliation state after durably recording the
    /// exact possible-effect set that will be observed.
    pub fn begin_reconcile(
        &mut self,
        job_id: &str,
        possible_effects_digest: Sha256Digest,
        completeness: &str,
        effect_count: usize,
        now_epoch_ms: u64,
    ) -> Result<(), JobError> {
        let job = self.jobs.get_mut(job_id).ok_or(JobError::UnknownJob)?;
        if job.state != JobState::OutcomeUnknown {
            return Err(JobError::ReconcileNotAvailable(job.state));
        }
        let digest = job.journal.append(
            JournalRecordKind::PossibleEffectSetRecorded,
            now_epoch_ms,
            1,
            id(job_id)?,
            vec![
                (id(fact::JOB_ID)?, job_id.to_string()),
                (
                    id("possibleEffectSetDigest")?,
                    possible_effects_digest.to_hex(),
                ),
                (id("completeness")?, completeness.to_string()),
                (id("effectCount")?, effect_count.to_string()),
                (id("readOnly")?, "true".into()),
            ],
        )?;
        job.move_to(JobState::Reconciling)?;
        job.stopped = None;
        job.publish(JobEventKind::StateChanged, now_epoch_ms, digest, |event| {
            event.facts.push(KeyValue {
                key: "reconcile".into(),
                value: "read-only-started".into(),
            });
        });
        Ok(())
    }

    /// Reduces fresh read-only observations and durably classifies the result.
    ///
    /// `terminal_scope` is deliberately explicit. Observing one interrupted
    /// effect does not prove that later steps in the original plan completed;
    /// only a reconciliation plan covering the complete terminal contract may
    /// turn an observation-level `succeeded` into job success.
    #[allow(clippy::too_many_arguments)]
    pub fn complete_reconcile(
        &mut self,
        job_id: &str,
        observations: &[(PersistentEffect, EffectObservation)],
        observation_facts: Vec<(String, String)>,
        evidence_digest: Sha256Digest,
        terminal_scope: bool,
        now_epoch_ms: u64,
    ) -> Result<ReconcileVerdict, JobError> {
        let job = self.jobs.get_mut(job_id).ok_or(JobError::UnknownJob)?;
        if job.state != JobState::Reconciling {
            return Err(JobError::NoReconcileInFlight);
        }

        let mut facts = vec![
            (id(fact::JOB_ID)?, job_id.to_string()),
            (id("evidenceDigest")?, evidence_digest.to_hex()),
            (id("observationCount")?, observations.len().to_string()),
        ];
        facts.extend(
            observation_facts
                .into_iter()
                .map(|(key, value)| Ok((id(&key)?, value)))
                .collect::<Result<Vec<_>, JobError>>()?,
        );
        job.journal.append(
            JournalRecordKind::ReadOnlyObservationRecorded,
            now_epoch_ms,
            1,
            id(job_id)?,
            facts,
        )?;

        let observed_verdict = reconcile(observations);
        let verdict = match observed_verdict {
            ReconcileVerdict::Succeeded if !terminal_scope => ReconcileVerdict::StillUnknown {
                reason: "the interrupted effect is present, but the read-only scope does not prove every later step in the original plan completed".into(),
            },
            other => other,
        };
        let (wire_verdict, next_state, reason) = match &verdict {
            ReconcileVerdict::Succeeded => (
                "succeeded",
                JobState::Succeeded,
                "read-only observations prove the complete terminal contract".to_string(),
            ),
            ReconcileVerdict::ConfirmedNotExecuted => (
                "confirmedNotExecuted",
                JobState::ConfirmedFailed,
                "read-only observations prove the unresolved effect was not executed".to_string(),
            ),
            ReconcileVerdict::ConfirmedPartial { present, absent } => (
                "confirmedPartial",
                JobState::ConfirmedFailed,
                format!(
                    "read-only observations prove a partial outcome ({present} present, {absent} absent)"
                ),
            ),
            ReconcileVerdict::StillUnknown { reason } => {
                ("stillUnknown", JobState::OutcomeUnknown, reason.clone())
            }
        };
        job.journal.append(
            JournalRecordKind::RecoveryAssessmentPublished,
            now_epoch_ms,
            1,
            id(job_id)?,
            vec![
                (id(fact::JOB_ID)?, job_id.to_string()),
                (id("reconcileVerdict")?, wire_verdict.into()),
                (id("reason")?, reason.clone()),
                (id("evidenceDigest")?, evidence_digest.to_hex()),
                (id("readOnly")?, "true".into()),
            ],
        )?;
        let event_sequence = job.next_sequence();
        let outcome = match next_state {
            JobState::Succeeded => "succeeded",
            JobState::ConfirmedFailed => "confirmedFailed",
            JobState::OutcomeUnknown => "outcomeUnknown",
            _ => unreachable!("reconcile reducer only emits conclusion states"),
        };
        let outcome_digest = job.journal.append(
            JournalRecordKind::OutcomeClassified,
            now_epoch_ms,
            1,
            id(job_id)?,
            vec![
                (id(fact::JOB_ID)?, job_id.to_string()),
                (id("outcome")?, outcome.into()),
                (id("reconcileVerdict")?, wire_verdict.into()),
                (id("reason")?, reason.clone()),
                (id("eventSequence")?, event_sequence.to_string()),
            ],
        )?;
        job.move_to(next_state)?;
        job.stopped = match next_state {
            JobState::Succeeded => Some(JobStop::Completed),
            JobState::ConfirmedFailed => Some(JobStop::ConfirmedFailure {
                reason: reason.clone(),
            }),
            JobState::OutcomeUnknown => Some(JobStop::ReconcileStillUnknown {
                reason: reason.clone(),
            }),
            _ => unreachable!("reconcile reducer only emits conclusion states"),
        };
        job.publish(
            JobEventKind::OutcomeClassified,
            now_epoch_ms,
            outcome_digest,
            |event| {
                event.facts.push(KeyValue {
                    key: "outcome".into(),
                    value: outcome.into(),
                });
                event.facts.push(KeyValue {
                    key: "reconcileVerdict".into(),
                    value: wire_verdict.into(),
                });
                event.facts.push(KeyValue {
                    key: "reason".into(),
                    value: reason,
                });
            },
        );
        Ok(verdict)
    }
}
