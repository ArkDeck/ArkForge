impl Service {

    /// Answers an admission the daemon asked for on the watchJob stream.
    fn submit_step_permit(&mut self, request: &Request) -> Response {
        let Ok(submission) = SubmitStepPermitRequest::decode(&request.payload) else {
            return self.refuse(
                request,
                Status::InvalidArgument,
                "BAD_SUBMISSION",
                "submitStepPermit payload does not decode",
            );
        };
        if !submission.is_well_formed() {
            return self.refuse(
                request,
                Status::InvalidArgument,
                "SUBMISSION_AMBIGUOUS",
                "a submission carries a permit or a refusal, never both and never neither",
            );
        }
        let Some(secret) = self.pairing.clone() else {
            return self.refuse(
                request,
                Status::Unavailable,
                arkforge_engine::ExecutionBlocker::NoPairedAuthority.code(),
                &arkforge_engine::ExecutionBlocker::NoPairedAuthority.to_string(),
            );
        };
        let Some(stored) = self.stored_plan_for_job(&submission.job_id) else {
            return self.refuse(
                request,
                Status::NotFound,
                "UNKNOWN_JOB",
                &format!("no job {}", submission.job_id),
            );
        };

        let permit = if submission.permit_cbor.is_empty() {
            None
        } else {
            match StepPermit::from_canonical_bytes(&submission.permit_cbor) {
                Ok(permit) => Some((
                    permit,
                    submission.integrity_tag.clone(),
                    submission.pairing_epoch,
                )),
                Err(error) => {
                    return self.refuse(
                        request,
                        Status::InvalidArgument,
                        "PERMIT_NOT_DECODABLE",
                        &error.to_string(),
                    );
                }
            }
        };

        let Some(profile) = self
            .profiles
            .get(&profile_key(
                &stored.envelope.profile.id,
                stored.envelope.profile.version,
            ))
            .cloned()
        else {
            return self.refuse(
                request,
                Status::NotFound,
                "PROFILE_NOT_LOADED",
                &format!(
                    "the plan names profile {}, which this daemon has not loaded",
                    stored.envelope.profile.id
                ),
            );
        };
        let current_facts = if permit.is_some() {
            match self.current_facts_for_job(&submission.job_id) {
                Ok(facts) => Some(facts),
                Err(error) => {
                    return self.ok(
                        request,
                        SubmissionOutcome::rejected("CURRENT_FACTS_UNAVAILABLE", error).encode(),
                    );
                }
            }
        } else {
            None
        };
        let result = self.jobs.submit_permit(
            &submission.job_id,
            &submission.request_id,
            permit,
            &submission.refusal,
            &secret,
            &stored.envelope,
            &stored.private_plan,
            &profile,
            current_facts,
            self.clock.now_epoch_ms(),
        );
        if matches!(
            result,
            Err(crate::jobs::JobError::SnapshotExpired)
                | Err(crate::jobs::JobError::ContinuityBroken(_))
        ) {
            let _ = self.publish_live_admission(&submission.job_id, false);
        }
        let outcome = match result {
            Ok(()) => SubmissionOutcome::accepted(),
            Err(error) => SubmissionOutcome::rejected(error.code(), error.to_string()),
        };
        self.ok(request, outcome.encode())
    }

    /// Records what the authority's own control channel observed.
    fn submit_control_receipt(&mut self, request: &Request) -> Response {
        let Ok(receipt) = SubmitManagedControlReceiptRequest::decode(&request.payload) else {
            return self.refuse(
                request,
                Status::InvalidArgument,
                "BAD_RECEIPT",
                "submitManagedControlReceipt payload does not decode",
            );
        };
        if self.pairing.is_none() {
            return self.refuse(
                request,
                Status::Unavailable,
                arkforge_engine::ExecutionBlocker::NoPairedAuthority.code(),
                &arkforge_engine::ExecutionBlocker::NoPairedAuthority.to_string(),
            );
        }
        let Some(stored) = self.stored_plan_for_job(&receipt.job_id) else {
            return self.refuse(
                request,
                Status::NotFound,
                "UNKNOWN_JOB",
                &format!("no job {}", receipt.job_id),
            );
        };
        let completed_step_id = self
            .jobs
            .job(&receipt.job_id)
            .map(crate::jobs::Job::current_step_id)
            .unwrap_or_default();
        let opens_exact_rebind = receipt.accepted
            && stored
                .envelope
                .public_steps
                .iter()
                .find(|step| step.step_id.as_str() == completed_step_id)
                .is_some_and(|step| {
                    successful_dispatch_requires_rebind(
                        ActionDisposition::SemanticSuccess,
                        step.expected_mode_before.as_ref(),
                        step.expected_mode_after.as_ref(),
                    )
                });
        let result = self.jobs.submit_control_receipt(
            &receipt,
            &stored.envelope,
            &stored.private_plan,
            self.clock.now_epoch_ms(),
        );
        if result.is_ok() && receipt.accepted {
            // Only a sealed mode-changing control opens a rebind. Read-only
            // property controls keep the exact session and re-read it for the
            // next admission.
            if opens_exact_rebind {
                self.job_sessions.remove(&receipt.job_id);
                self.authorized_rebinds.insert(receipt.job_id.clone());
            }
            let allow_unique_rebind = self.authorized_rebinds.contains(&receipt.job_id);
            let _ = self.publish_live_admission(&receipt.job_id, allow_unique_rebind);
        }
        let outcome = match result {
            Ok(()) => SubmissionOutcome::accepted(),
            Err(error) => SubmissionOutcome::rejected(error.code(), error.to_string()),
        };
        self.ok(request, outcome.encode())
    }

    /// The stored plan a job is running.
    ///
    /// Looked up through the job rather than taken from the caller: a
    /// submission that could name its own plan could answer one job's
    /// admission with another job's permit.
    fn stored_plan_for_job(&self, job_id: &str) -> Option<StoredPlan> {
        let job = self.jobs.job(job_id)?;
        let plan_id = PlanId::new(job.plan_id()).ok()?;
        self.engine
            .plans()
            .get(&plan_id, job.plan_digest())
            .ok()
            .cloned()
    }
}
