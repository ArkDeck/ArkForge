impl JobRegistry {

    /// Answers an admission the daemon asked for.
    ///
    /// The permit is verified against the durable ledger, not against memory: a
    /// daemon that restarted has no memory, and a permit consumed before the
    /// restart must still be refused (architecture.md 8.5).
    #[allow(clippy::too_many_arguments)]
    pub fn submit_permit(
        &mut self,
        job_id: &str,
        request_id: &str,
        permit: Option<(StepPermit, Vec<u8>, u64)>,
        refusal: &str,
        secret: &ControllerPairingSecret,
        envelope: &FlashPlanEnvelope,
        private_plan: &StoredProviderPlan,
        profile: &DeviceProfile,
        current_facts: Option<CurrentFacts>,
        now_epoch_ms: u64,
    ) -> Result<(), JobError> {
        let artifact_digest = envelope.artifact.content_digest;
        let job = self.jobs.get_mut(job_id).ok_or(JobError::UnknownJob)?;
        let (expected, snapshot, freshness) = match job.pending.as_ref() {
            Some(Pending::Admission {
                request_id,
                snapshot,
                freshness,
            }) => (request_id.clone(), snapshot.clone(), freshness.clone()),
            _ => return Err(JobError::NoAdmissionPending),
        };
        if expected != request_id {
            return Err(JobError::WrongRequest {
                expected,
                found: request_id.to_string(),
            });
        }
        let Some((mut permit, tag, epoch)) = permit else {
            // A refusal is an answer. Nothing was recorded, so this is safe.
            job.journal.append(
                JournalRecordKind::CancellationRequested,
                now_epoch_ms,
                1,
                id(&snapshot.step_id)?,
                vec![(id("refusal")?, refusal.to_string())],
            )?;
            let event_sequence = job.next_sequence();
            let digest = job.journal.append(
                JournalRecordKind::OutcomeClassified,
                now_epoch_ms,
                1,
                id(&snapshot.step_id)?,
                vec![
                    (id("outcome")?, "cancelledSafe".into()),
                    (id("reason")?, refusal.to_string()),
                    (id("eventSequence")?, event_sequence.to_string()),
                ],
            )?;
            // Keep memory behind stable storage: a failed append leaves the
            // admission answerable rather than manufacturing a terminal that
            // the journal cannot explain.
            job.move_to(JobState::AwaitingPermit)?;
            job.move_to(JobState::CancelledSafe)?;
            job.pending = None;
            job.stopped = Some(JobStop::RefusedByAuthority {
                step_id: snapshot.step_id.clone(),
                reason: refusal.to_string(),
            });
            job.publish(
                JobEventKind::OutcomeClassified,
                now_epoch_ms,
                digest,
                |event| {
                    event.facts.push(KeyValue {
                        key: "outcome".into(),
                        value: "cancelledSafe".into(),
                    });
                    event.facts.push(KeyValue {
                        key: "refusal".into(),
                        value: refusal.to_string(),
                    });
                },
            );
            return Ok(());
        };

        let current_facts = current_facts.ok_or(JobError::CurrentFactsUnavailable)?;
        match evaluate_freshness(&freshness, &current_facts) {
            FreshnessVerdict::Fresh => {}
            FreshnessVerdict::StaleSnapshot { .. } => {
                // The authority's answer is not consumed. The service will
                // take and publish a new same-device snapshot.
                job.pending = None;
                return Err(JobError::SnapshotExpired);
            }
            FreshnessVerdict::ContinuityBroken(reason) => {
                job.pending = None;
                return Err(JobError::ContinuityBroken(format!("{reason:?}")));
            }
        }

        permit.integrity_tag = PermitIntegrityTag {
            epoch: PairingEpoch(epoch),
            tag: digest_from(&tag).ok_or(JobError::TagNotADigest)?,
        };

        // The permit must authorize the action this job is about to take, not
        // merely be a valid permit.
        let action = private_action_digest(&snapshot)?;
        let intent = DispatchIntent {
            controller_session_id: job.controller_session_id.clone(),
            job_id: JobId::new(&job.job_id)
                .map_err(|_| JobError::UnusableIdentifier(job.job_id.clone()))?,
            plan_id: PlanId::new(&job.plan_id)
                .map_err(|_| JobError::UnusableIdentifier(job.plan_id.clone()))?,
            plan_digest: job.plan_digest,
            step_id: StepId::new(&snapshot.step_id)
                .map_err(|_| JobError::UnusableIdentifier(snapshot.step_id.clone()))?,
            attempt_id: AttemptId::new(&snapshot.attempt_id)
                .map_err(|_| JobError::UnusableIdentifier(snapshot.attempt_id.clone()))?,
            public_step_digest: digest_from(&snapshot.public_step_sha256)
                .ok_or(JobError::TagNotADigest)?,
            private_action_digest: action,
            effect_set_digest: digest_from(&snapshot.effect_set_sha256)
                .ok_or(JobError::TagNotADigest)?,
            authority_binding: envelope.authority_binding.clone(),
            admitted_device_facts_digest: digest_from(&snapshot.admitted_device_facts_sha256)
                .ok_or(JobError::TagNotADigest)?,
            now_epoch_ms,
        };
        // This is the sole production admission writer. Verification, durable
        // ledger replay, permit acceptance and intent recording all happen in
        // the engine's linear transaction path.
        let admitted =
            admit_step(&mut job.journal, &permit, secret, &intent).map_err(JobError::from_step)?;

        job.move_to(JobState::AwaitingPermit)?;
        let permit_id = admitted.permit().permit().permit_id.as_str().to_string();
        let step_id = snapshot.step_id.clone();
        let intent_digest = admitted.intent_digest();
        job.move_to(JobState::StepIntentDurable)?;
        job.pending = None;
        job.publish(
            JobEventKind::StateChanged,
            now_epoch_ms,
            intent_digest,
            |_| {},
        );

        // Whose step is this? The plan says. A step the authority performs goes
        // back out as a control request; anything else needs a dispatch.
        match managed_control_for(private_plan, &step_id)? {
            Some((action, expect)) => {
                let control = ManagedControlRequest {
                    job_id: job.job_id.clone(),
                    step_id: step_id.clone(),
                    request_id: format!("{}-control", snapshot.request_id),
                    action,
                    permit_id,
                    expected_facts: expect
                        .into_iter()
                        .map(|(key, value)| KeyValue { key, value })
                        .collect(),
                    deadline_epoch_ms: now_epoch_ms.saturating_add(120_000),
                };
                let transaction = begin_dispatch_with_facts(
                    &mut job.journal,
                    admitted,
                    now_epoch_ms,
                    vec![
                        // The journal names what this permit is waiting on. A
                        // job parked here used to be indistinguishable from one
                        // that asked for nothing — the record ended at
                        // `permitConsuming` and the wait lived only in memory —
                        // so the request is now a recorded fact beside the
                        // permit that spent itself asking.
                        ("controlRequestId".into(), control.request_id.clone()),
                        ("controlAction".into(), control.action.as_str().to_string()),
                        (
                            "controlDeadlineEpochMs".into(),
                            control.deadline_epoch_ms.to_string(),
                        ),
                    ],
                )
                .map_err(JobError::from_step)?;
                let digest = transaction.dispatch_digest();
                job.move_to(JobState::Dispatching)?;
                job.pending = Some(Pending::Control {
                    request_id: control.request_id.clone(),
                    request: control.clone(),
                    transaction,
                });
                job.publish(
                    JobEventKind::ManagedControlRequested,
                    now_epoch_ms,
                    digest,
                    |event| event.control_request = Some(control),
                );
            }
            None => {
                let actions = ordered_actions(private_plan, &step_id)?;
                let transaction = begin_dispatch(&mut job.journal, admitted, now_epoch_ms)
                    .map_err(JobError::from_step)?;
                let digest = transaction.dispatch_digest();
                job.move_to(JobState::Dispatching)?;
                job.pending = Some(Pending::Dispatch {
                    work: Box::new(PendingDispatch {
                        job_id: job.job_id.clone(),
                        step_id: step_id.clone(),
                        actions,
                        plan_actions: private_plan.actions.clone(),
                        profile: profile.clone(),
                        artifact_digest,
                    }),
                    transaction,
                });
                job.publish(JobEventKind::StateChanged, now_epoch_ms, digest, |_| {});
            }
        }
        Ok(())
    }

    /// Hands the dispatcher the next piece of work, if there is one.
    ///
    /// Takes it rather than lending it: the work leaves the lock, and a job
    /// that could hand the same action to two dispatchers would dispatch twice.
    pub fn take_pending_dispatch(&mut self) -> Option<PendingDispatch> {
        for job in self.jobs.values_mut() {
            if matches!(job.pending, Some(Pending::Dispatch { .. })) {
                // Marked in-flight by clearing it. Whether the device changed
                // from here on is unknown until a receipt says otherwise, which
                // is exactly what the journal already records.
                let Some(Pending::Dispatch { work, transaction }) = job.pending.take() else {
                    unreachable!("the dispatch shape was checked above")
                };
                let work = *work;
                let runner_work = work.clone();
                job.in_flight = Some(InFlightDispatch { work, transaction });
                return Some(runner_work);
            }
        }
        None
    }

    /// Publishes the next admission after a checkpoint/rebind established a
    /// new exact session. No caller can supply device facts through IPC; this
    /// is invoked only by the service after transport observation.
    pub fn request_next_admission(
        &mut self,
        job_id: &str,
        envelope: &FlashPlanEnvelope,
        private_plan: &StoredProviderPlan,
        admission_facts: &AdmissionFacts,
        now_epoch_ms: u64,
    ) -> Result<(), JobError> {
        let job = self.jobs.get_mut(job_id).ok_or(JobError::UnknownJob)?;
        if job.pending.is_some() || !matches!(job.state, JobState::Preflight | JobState::RebindWait)
        {
            return Err(JobError::AdmissionNotReady);
        }
        if job.state == JobState::RebindWait {
            let observation_digest = admission_facts
                .observation
                .facts_digest()
                .map_err(|error| JobError::Core(error.to_string()))?;
            let next_step_id = envelope
                .public_steps
                .get(job.step_index)
                .ok_or(JobError::PlanHasNoSteps)?
                .step_id
                .to_string();
            let digest = job.journal.append(
                JournalRecordKind::RebindObserved,
                now_epoch_ms,
                1,
                id(&next_step_id)?,
                vec![
                    (id(fact::JOB_ID)?, job.job_id.clone()),
                    (id(fact::PLAN_ID)?, job.plan_id.clone()),
                    (id(fact::STEP_ID)?, next_step_id),
                    (id("observationDigest")?, observation_digest.to_hex()),
                    (
                        id("transportSessionDigest")?,
                        admission_facts.transport_session_digest.to_hex(),
                    ),
                    (
                        id("observedMode")?,
                        admission_facts.observation.mode.as_str().to_string(),
                    ),
                ],
            )?;
            job.move_to(JobState::Preflight)?;
            job.publish(JobEventKind::StateChanged, now_epoch_ms, digest, |_| {});
        }
        request_admission(job, envelope, private_plan, admission_facts, now_epoch_ms)
    }

    /// Classifies every control request left unanswered past its deadline.
    ///
    /// The request names its deadline, and the design lets the authority
    /// answer, refuse, or say nothing — but "nothing" used to leave the job
    /// parked at `permitConsuming` until an operator dug the journal out of a
    /// CBOR file. Enforced here, by the same sweep that feeds the dispatcher,
    /// silence now costs one deadline instead of a bench.
    ///
    /// The classification is the refused-receipt one, for the refused-receipt
    /// reason: an unanswered request is not "nothing happened" — the authority
    /// may have acted and never reported (architecture.md 14.1).
    pub fn expire_stale_controls(&mut self, now_epoch_ms: u64) -> Vec<String> {
        let mut expired = Vec::new();
        for (job_id, job) in self.jobs.iter_mut() {
            let Some(Pending::Control { request, .. }) = &job.pending else {
                continue;
            };
            if now_epoch_ms <= request.deadline_epoch_ms {
                continue;
            }
            let step_id = request.step_id.clone();
            let reason = format!(
                "managed control {} request {} expired unanswered (deadline {} ms, now {} ms)",
                request.action.as_str(),
                request.request_id,
                request.deadline_epoch_ms,
                now_epoch_ms
            );
            let (Ok(subject), Ok(outcome_key), Ok(reason_key), Ok(event_sequence_key)) = (
                id(&step_id),
                id("outcome"),
                id("reason"),
                id("eventSequence"),
            ) else {
                continue;
            };
            // Durable first, state after: an expiry the journal cannot record
            // is left pending and retried on the next sweep, rather than moved
            // to a state the journal never explains.
            let event_sequence = job.next_sequence();
            let Ok(digest) = job.journal.append(
                JournalRecordKind::OutcomeClassified,
                now_epoch_ms,
                1,
                subject,
                vec![
                    (outcome_key, "outcomeUnknown".to_string()),
                    (reason_key, reason.clone()),
                    (event_sequence_key, event_sequence.to_string()),
                ],
            ) else {
                continue;
            };
            if job.move_to(JobState::OutcomeUnknown).is_err() {
                continue;
            }
            job.pending = None;
            job.stopped = Some(JobStop::ControlOutcomeUnknown {
                step_id,
                reason: reason.clone(),
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
                    event.facts.push(KeyValue {
                        key: "reason".into(),
                        value: reason.clone(),
                    });
                },
            );
            expired.push(job_id.clone());
        }
        expired
    }
}
