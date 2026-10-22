impl JobRegistry {

    /// Records what the dispatcher observed and advances the job.
    ///
    /// An outcome of `OutcomeUnknown` stops the job there. It does not retry,
    /// and there is no path in this type that could: the permit is spent and
    /// architecture.md 14.1 forbids replaying the intent.
    pub fn complete_dispatch(
        &mut self,
        job_id: &str,
        outcome: DispatchOutcome,
        envelope: &FlashPlanEnvelope,
        _private_plan: &StoredProviderPlan,
        now_epoch_ms: u64,
    ) -> Result<(), JobError> {
        let job = self.jobs.get_mut(job_id).ok_or(JobError::UnknownJob)?;
        let Some(InFlightDispatch { work, transaction }) = job.in_flight.take() else {
            return Err(JobError::NoDispatchInFlight);
        };
        let step_id = work.step_id.clone();

        if let Err(error) = record_transport_evidence(
            &mut job.journal,
            &transaction,
            now_epoch_ms,
            outcome.facts.clone(),
        ) {
            job.in_flight = Some(InFlightDispatch { work, transaction });
            return Err(JobError::from_step(error));
        }

        if outcome.disposition == ActionDisposition::OutcomeUnknown {
            let event_sequence = job.next_sequence();
            let digest = match job.journal.append(
                JournalRecordKind::OutcomeClassified,
                now_epoch_ms,
                1,
                id(&step_id)?,
                vec![
                    (id("outcome")?, outcome.disposition.as_str().to_string()),
                    (id("eventSequence")?, event_sequence.to_string()),
                ],
            ) {
                Ok(digest) => digest,
                Err(error) => {
                    job.in_flight = Some(InFlightDispatch { work, transaction });
                    return Err(JobError::Journal(error));
                }
            };
            job.move_to(JobState::OutcomeUnknown)?;
            job.stopped = Some(JobStop::DispatchOutcomeUnknown {
                step_id,
                disposition: outcome.disposition,
            });
            job.publish(
                JobEventKind::OutcomeClassified,
                now_epoch_ms,
                digest,
                |event| {
                    event.facts.push(KeyValue {
                        key: "outcome".into(),
                        value: outcome.disposition.as_str().to_string(),
                    });
                    // The dispatch facts name what actually happened — the tool's
                    // exit, its duration, the tail of what it printed. Publishing
                    // only "unknown" made the authority's timeline end in a word
                    // while the explanation sat in this daemon's CBOR journal.
                    for (key, value) in &outcome.facts {
                        event.facts.push(KeyValue {
                            key: key.clone(),
                            value: value.clone(),
                        });
                    }
                },
            );
            return Ok(());
        }

        let mut receipt = ActionReceiptSummary {
            job_id: transaction.job_id().to_string(),
            plan_id: transaction.plan_id().to_string(),
            step_id: step_id.clone(),
            action_id: work
                .actions
                .last()
                .map(|action| action.action_id.to_string())
                .unwrap_or_default(),
            attempt_id: transaction.attempt_id().to_string(),
            permit_id: transaction.permit_id().to_string(),
            disposition: outcome.disposition.as_str().to_string(),
            evidence_sha256: outcome.evidence_digest.as_bytes().to_vec(),
            facts: outcome
                .facts
                .iter()
                .map(|(key, value)| KeyValue {
                    key: key.clone(),
                    value: value.clone(),
                })
                .collect(),
            ..ActionReceiptSummary::default()
        };
        // A typed skip is never any grade of verified, so the strength field
        // stays empty for anything that is not `Verified` (architecture.md 16.4).
        match &outcome.verification {
            Some(VerificationOutcome::Verified { strength, range }) => {
                receipt.verification_outcome = "verified".into();
                receipt.verification_strength = strength.as_str().to_string();
                receipt.verified_range_start = range.start;
                receipt.verified_range_length = range.length;
            }
            Some(VerificationOutcome::TypedSkip { range, reason, .. }) => {
                receipt.verification_outcome = "typedSkip".into();
                receipt.verified_range_start = range.start;
                receipt.verified_range_length = range.length;
                receipt.typed_skip_reason = reason.as_str().to_string();
            }
            Some(VerificationOutcome::Failed {
                range,
                classification,
            }) => {
                receipt.verification_outcome = "failed".into();
                receipt.verified_range_start = range.start;
                receipt.verified_range_length = range.length;
                receipt.failure_classification = classification.as_str().to_string();
            }
            None => {}
        }

        // `receiptDigest` identifies the whole semantic receipt. Transport
        // evidence remains a distinct field within that receipt and is never
        // substituted for its identity.
        let receipt_digest = receipt
            .receipt_digest()
            .map_err(|error| JobError::Core(error.to_string()))?;
        let receipt_body = receipt
            .to_canonical_bytes()
            .map_err(|error| JobError::Core(error.to_string()))?;
        let receipt_event_sequence = job.next_sequence();
        let completed = record_receipt_with_facts(
            &mut job.journal,
            transaction,
            receipt_digest,
            vec![
                ("receiptBodyHex".into(), encode_hex(&receipt_body)),
                (
                    "receiptEventSequence".into(),
                    receipt_event_sequence.to_string(),
                ),
            ],
            now_epoch_ms,
        )
        .map_err(JobError::from_step)?;
        job.move_to(JobState::ReceiptDurable)?;
        let receipt_record = completed.receipt_record_digest();

        let checkpoint =
            checkpoint(&mut job.journal, completed, now_epoch_ms).map_err(JobError::from_step)?;
        job.move_to(JobState::Checkpointed)?;
        job.publish(
            JobEventKind::ActionReceipt,
            now_epoch_ms,
            receipt_record,
            |event| event.receipt = Some(receipt),
        );
        job.publish(
            JobEventKind::StepCheckpointed,
            now_epoch_ms,
            checkpoint,
            |_| {},
        );

        if matches!(
            outcome.disposition,
            ActionDisposition::ConfirmedNoEffect | ActionDisposition::ConfirmedPartialEffect
        ) {
            let reason = match outcome.disposition {
                ActionDisposition::ConfirmedNoEffect => {
                    format!("{step_id} conclusively failed before taking effect")
                }
                ActionDisposition::ConfirmedPartialEffect => {
                    format!("{step_id} conclusively failed after taking a partial effect")
                }
                _ => unreachable!("the settled failure variants were matched above"),
            };
            let event_sequence = job.next_sequence();
            let digest = job.journal.append(
                JournalRecordKind::OutcomeClassified,
                now_epoch_ms,
                1,
                id(&step_id)?,
                vec![
                    (id("outcome")?, "confirmedFailed".into()),
                    (
                        id("actionDisposition")?,
                        outcome.disposition.as_str().to_string(),
                    ),
                    (id("reason")?, reason.clone()),
                    (id("eventSequence")?, event_sequence.to_string()),
                ],
            )?;
            job.move_to(JobState::ConfirmedFailed)?;
            job.stopped = Some(JobStop::ConfirmedFailure {
                reason: reason.clone(),
            });
            job.publish(
                JobEventKind::OutcomeClassified,
                now_epoch_ms,
                digest,
                |event| {
                    event.facts.push(KeyValue {
                        key: "outcome".into(),
                        value: "confirmedFailed".into(),
                    });
                    event.facts.push(KeyValue {
                        key: "actionDisposition".into(),
                        value: outcome.disposition.as_str().to_string(),
                    });
                    event.facts.push(KeyValue {
                        key: "reason".into(),
                        value: reason,
                    });
                },
            );
            return Ok(());
        }

        if let Some(VerificationOutcome::Failed { classification, .. }) = &outcome.verification {
            let reason = format!(
                "{step_id} verification failed conclusively: {}",
                classification.as_str()
            );
            let event_sequence = job.next_sequence();
            let digest = job.journal.append(
                JournalRecordKind::OutcomeClassified,
                now_epoch_ms,
                1,
                id(&step_id)?,
                vec![
                    (id("outcome")?, "confirmedFailed".into()),
                    (id("reason")?, reason.clone()),
                    (id("failureClassification")?, classification.as_str().into()),
                    (id("eventSequence")?, event_sequence.to_string()),
                ],
            )?;
            job.move_to(JobState::ConfirmedFailed)?;
            job.stopped = Some(JobStop::ConfirmedFailure {
                reason: reason.clone(),
            });
            job.publish(
                JobEventKind::OutcomeClassified,
                now_epoch_ms,
                digest,
                |event| {
                    event.facts.push(KeyValue {
                        key: "outcome".into(),
                        value: "confirmedFailed".into(),
                    });
                    event.facts.push(KeyValue {
                        key: "reason".into(),
                        value: reason,
                    });
                    event.facts.push(KeyValue {
                        key: "failureClassification".into(),
                        value: classification.as_str().into(),
                    });
                },
            );
            return Ok(());
        }

        if job.cancellation_requested {
            finish_queued_cancellation(job, now_epoch_ms, checkpoint)
        } else {
            advance(job, envelope, now_epoch_ms, checkpoint)
        }
    }

    /// Records what the authority's own control channel observed.
    pub fn submit_control_receipt(
        &mut self,
        request: &SubmitManagedControlReceiptRequest,
        envelope: &FlashPlanEnvelope,
        _private_plan: &StoredProviderPlan,
        now_epoch_ms: u64,
    ) -> Result<(), JobError> {
        validate_receipt_fact_keys(&request.facts)?;
        let forbidden = request.forbidden_facts();
        if !forbidden.is_empty() {
            // The port exists so ArkForge never learns these. Refuse the whole
            // receipt rather than dropping the field and carrying on.
            return Err(JobError::ReceiptCarriesForbiddenFacts(
                forbidden.into_iter().map(str::to_string).collect(),
            ));
        }

        let job = self
            .jobs
            .get_mut(&request.job_id)
            .ok_or(JobError::UnknownJob)?;
        let (expected, asked) = match job.pending.as_ref() {
            Some(Pending::Control {
                request_id,
                request,
                ..
            }) => (request_id.clone(), request.clone()),
            _ => return Err(JobError::NoControlPending),
        };
        if expected != request.request_id {
            return Err(JobError::WrongRequest {
                expected,
                found: request.request_id.clone(),
            });
        }
        if asked.action != request.action {
            return Err(JobError::WrongControlAction {
                expected: asked.action,
                found: request.action,
            });
        }
        if request.accepted {
            let missing = missing_expected_control_facts(&asked.expected_facts, &request.facts);
            if !missing.is_empty() {
                return Err(JobError::ControlFactsIncomplete(missing));
            }
        }

        let evidence = if request.accepted {
            // An accepted receipt's evidence digest is defined, not opaque:
            // the canonical digest of its own facts.
            let evidence = digest_from(&request.evidence_sha256).ok_or(JobError::TagNotADigest)?;
            let computed = canonical_facts_digest(&request.facts);
            if computed != evidence {
                return Err(JobError::ControlEvidenceMismatch);
            }
            Some(evidence)
        } else {
            None
        };

        // Validation above borrows the pending control. Only after it succeeds
        // do we move the one linear transaction token out of the job.
        let Some(Pending::Control { transaction, .. }) = job.pending.take() else {
            unreachable!("the control shape was checked above")
        };

        let step_id = asked.step_id.clone();
        if let Err(error) = record_transport_evidence(
            &mut job.journal,
            &transaction,
            now_epoch_ms,
            request
                .facts
                .iter()
                .map(|fact| (fact.key.clone(), fact.value.clone()))
                .collect(),
        ) {
            job.pending = Some(Pending::Control {
                request_id: expected,
                request: asked,
                transaction,
            });
            return Err(JobError::from_step(error));
        }

        if !request.accepted {
            // Not "nothing happened". The device may have changed and the
            // authority simply did not observe it (architecture.md 14.1).
            //
            // A refusal carries no evidence digest, and none is demanded: the
            // refusal made no observation, so there is nothing to digest.
            // Demanding one here forced the authority to either invent bytes or
            // be rejected — and a rejected refusal left this job parked at
            // `permitConsuming` with both sides waiting on the other, which is
            // the deadlock that held the bench for three days.
            let event_sequence = job.next_sequence();
            let digest = match job.journal.append(
                JournalRecordKind::OutcomeClassified,
                now_epoch_ms,
                1,
                id(&step_id)?,
                vec![
                    (id("outcome")?, "outcomeUnknown".to_string()),
                    (id("reason")?, request.failure_reason.clone()),
                    (id("eventSequence")?, event_sequence.to_string()),
                ],
            ) {
                Ok(digest) => digest,
                Err(error) => {
                    job.pending = Some(Pending::Control {
                        request_id: expected,
                        request: asked,
                        transaction,
                    });
                    return Err(JobError::Journal(error));
                }
            };
            job.move_to(JobState::OutcomeUnknown)?;
            job.stopped = Some(JobStop::ControlOutcomeUnknown {
                step_id,
                reason: request.failure_reason.clone(),
            });
            job.publish(
                JobEventKind::OutcomeClassified,
                now_epoch_ms,
                digest,
                |event| {
                    event.facts.push(KeyValue {
                        key: "outcome".into(),
                        value: "outcomeUnknown".into(),
                    });
                    // The journal keeps the reason; the event must carry it too,
                    // or the authority ends its job knowing only "unknown" while
                    // the one line naming the cause sits in a CBOR file.
                    event.facts.push(KeyValue {
                        key: "reason".into(),
                        value: request.failure_reason.clone(),
                    });
                },
            );
            return Ok(());
        }

        let evidence = evidence.expect("accepted control validated an evidence digest");

        let receipt = ActionReceiptSummary {
            job_id: transaction.job_id().to_string(),
            plan_id: transaction.plan_id().to_string(),
            step_id,
            action_id: String::new(),
            attempt_id: transaction.attempt_id().to_string(),
            permit_id: transaction.permit_id().to_string(),
            disposition: "semanticSuccess".into(),
            evidence_sha256: evidence.as_bytes().to_vec(),
            facts: request.facts.clone(),
            ..ActionReceiptSummary::default()
        };
        let receipt_digest = receipt
            .receipt_digest()
            .map_err(|error| JobError::Core(error.to_string()))?;
        let receipt_body = receipt
            .to_canonical_bytes()
            .map_err(|error| JobError::Core(error.to_string()))?;
        let receipt_event_sequence = job.next_sequence();
        let completed = record_receipt_with_facts(
            &mut job.journal,
            transaction,
            receipt_digest,
            vec![
                ("receiptBodyHex".into(), encode_hex(&receipt_body)),
                (
                    "receiptEventSequence".into(),
                    receipt_event_sequence.to_string(),
                ),
            ],
            now_epoch_ms,
        )
        .map_err(JobError::from_step)?;
        job.move_to(JobState::ReceiptDurable)?;
        let receipt_record = completed.receipt_record_digest();
        let checkpoint =
            checkpoint(&mut job.journal, completed, now_epoch_ms).map_err(JobError::from_step)?;
        job.move_to(JobState::Checkpointed)?;
        job.publish(
            JobEventKind::ActionReceipt,
            now_epoch_ms,
            receipt_record,
            |event| event.receipt = Some(receipt),
        );
        job.publish(
            JobEventKind::StepCheckpointed,
            now_epoch_ms,
            checkpoint,
            |_| {},
        );

        if job.cancellation_requested {
            finish_queued_cancellation(job, now_epoch_ms, checkpoint)
        } else {
            advance(job, envelope, now_epoch_ms, checkpoint)
        }
    }

    /// Cancels a job, if cancelling is still safe.
    ///
    /// architecture.md 13.4: before a permit, `CancelledSafe`. Once an intent
    /// is durable there is an unresolved effect, and a job with one may not
    /// return `CancelledSafe` — the honest answer is an unknown outcome.
    pub fn cancel(
        &mut self,
        job_id: &str,
        now_epoch_ms: u64,
    ) -> Result<CancellationDisposition, JobError> {
        let job = self.jobs.get_mut(job_id).ok_or(JobError::UnknownJob)?;
        if job.state == JobState::OutcomeUnknown {
            return Ok(CancellationDisposition::OutcomeUnknown);
        }
        if job.state.is_terminal() {
            return Ok(CancellationDisposition::AlreadyTerminal);
        }
        let digest = job.journal.append(
            JournalRecordKind::CancellationRequested,
            now_epoch_ms,
            1,
            id(job_id)?,
            Vec::new(),
        )?;
        job.cancellation_requested = true;

        // A dispatch still sitting in the daemon queue has not crossed the
        // process boundary. Drop it and conclude safely; an in-flight or
        // authority-owned action must be allowed to reach its receipt first.
        if matches!(job.pending, Some(Pending::Dispatch { .. })) && job.in_flight.is_none() {
            conclude_cancellation(job, now_epoch_ms, digest, "cancelled before dispatch")?;
            return Ok(CancellationDisposition::CancelledSafe);
        }

        if job.in_flight.is_some()
            || matches!(job.pending, Some(Pending::Control { .. }))
            || matches!(
                job.state,
                JobState::StepIntentDurable | JobState::Dispatching
            )
        {
            job.publish(JobEventKind::StateChanged, now_epoch_ms, digest, |event| {
                event.facts.push(KeyValue {
                    key: "cancellation".into(),
                    value: "queued-at-safe-boundary".into(),
                });
            });
            return Ok(CancellationDisposition::QueuedAtSafeBoundary);
        }

        conclude_cancellation(job, now_epoch_ms, digest, "cancelled before any permit")?;
        Ok(CancellationDisposition::CancelledSafe)
    }
}
