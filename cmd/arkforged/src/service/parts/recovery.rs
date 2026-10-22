impl Service {

    /// Returns the strongest read-only conclusion currently supported.
    ///
    /// DAYU200's measured RockUSB read face does not cover most writable
    /// partitions, so an unresolved write normally remains unknown. Returning
    /// that typed verdict is still materially different from replaying it or
    /// pretending the recovery API does not exist.
    fn reconcile_job(&mut self, request: &Request) -> Response {
        let job_id = first_string_field(&request.payload, 1).unwrap_or_default();
        let Some(job) = self.jobs.job(&job_id) else {
            return self.refuse(
                request,
                Status::NotFound,
                "UNKNOWN_JOB",
                &format!("no job {job_id}"),
            );
        };
        let initial_state = job.state();
        let previous_assessment = job.last_reconcile_assessment();
        let context = self.recovery_surface_context(&job_id);
        let recovered_possible = context
            .is_none()
            .then(|| recovered_managed_control_possible_effects(job.journal()))
            .flatten();
        let possible = context
            .as_ref()
            .map(|context| context.possible.clone())
            .or(recovered_possible)
            .unwrap_or_else(|| PossibleEffectSet {
                effects: arkforge_core::EffectSet::read_only(),
                completeness: EffectSetCompleteness::Unbounded,
                source_action_ids: Vec::new(),
            });

        let (verdict, detail, state) = if initial_state == arkforge_engine::JobState::OutcomeUnknown
        {
            let possible_digest = match possible.digest() {
                Ok(digest) => digest,
                Err(error) => {
                    return self.refuse(
                        request,
                        Status::Internal,
                        "RECONCILE_MODEL_INVALID",
                        &error.to_string(),
                    );
                }
            };
            let completeness = completeness_name(possible.completeness);

            let selected: Result<Option<(ReadOnlyReconcilePlan, DeviceProfile)>, String> =
                match self.stored_plan_for_job(&job_id) {
                    Some(stored) => {
                        let profile = self
                            .profiles
                            .get(&profile_key(
                                &stored.envelope.profile.id,
                                stored.envelope.profile.version,
                            ))
                            .cloned()
                            .ok_or_else(|| {
                                "the stored plan's device profile is not loaded".to_string()
                            });
                        profile.and_then(|profile| {
                            let request = ReconcileRequest {
                                private_plan: &stored.private_plan,
                                possible_effects: &possible.effects.persistent,
                            };
                            let plan = match stored.envelope.provider.id.as_str() {
                                arkforge_provider::rockchip::PROVIDER_ID => {
                                    self.rockchip.reconcile_read_only(&request)
                                }
                                arkforge_provider::unisoc::PROVIDER_ID => {
                                    self.unisoc.reconcile_read_only(&request)
                                }
                                provider => Err(arkforge_provider::ProviderError::Unsupported(
                                    format!("provider {provider} has no reconcile port"),
                                )),
                            }
                            .map_err(|error| error.to_string())?;
                            Ok(Some((plan, profile)))
                        })
                    }
                    None => Ok(None),
                };

            if let Err(error) = self.jobs.begin_reconcile(
                &job_id,
                possible_digest,
                completeness,
                possible.effects.persistent.len(),
                self.clock.now_epoch_ms(),
            ) {
                return self.refuse(request, Status::Refused, error.code(), &error.to_string());
            }

            match selected {
                Ok(Some((plan, profile))) if !plan.actions.is_empty() => {
                    self.pending_reconciliations.push_back(PendingReconcile {
                        job_id: job_id.clone(),
                        actions: plan.actions,
                        profile,
                        possible_effects: possible.effects.persistent.clone(),
                        // The current Rockchip surface observes the interrupted
                        // persistent effect only. It cannot prove later reset/
                        // postflight steps, so it must not conclude job success.
                        terminal_scope: false,
                    });
                    (
                        "stillUnknown".to_string(),
                        "read-only reconciliation was scheduled; watch the exact job for its durable assessment"
                            .to_string(),
                        "reconciling".to_string(),
                    )
                }
                selection => {
                    // No private plan (common after daemon restart), no
                    // observable persistent effect, or a Provider refusal can
                    // be reduced immediately without touching a device.
                    let reason = match selection {
                        Ok(Some(_)) => {
                            "the possible effect set has no provider-selected read-only observation"
                                .to_string()
                        }
                        Ok(None) => {
                            "the durable job was recovered without its private plan".to_string()
                        }
                        Err(error) => format!("the provider refused read-only reconcile: {error}"),
                    };
                    let observations = possible
                        .effects
                        .persistent
                        .iter()
                        .cloned()
                        .map(|effect| (effect, EffectObservation::Indeterminate))
                        .collect::<Vec<_>>();
                    if let Err(error) = self.jobs.complete_reconcile(
                        &job_id,
                        &observations,
                        vec![
                            ("readOnly".into(), "true".into()),
                            ("reconcileUnavailable".into(), reason.clone()),
                        ],
                        sha256(reason.as_bytes()),
                        false,
                        self.clock.now_epoch_ms(),
                    ) {
                        return self.refuse(
                            request,
                            Status::Internal,
                            error.code(),
                            &error.to_string(),
                        );
                    }
                    let (verdict, durable_reason) = self
                        .jobs
                        .job(&job_id)
                        .and_then(|job| job.last_reconcile_assessment())
                        .unwrap_or_else(|| ("stillUnknown".into(), reason));
                    (
                        verdict,
                        durable_reason,
                        self.jobs
                            .job(&job_id)
                            .map(|job| job.state().as_str().to_string())
                            .unwrap_or_else(|| "outcomeUnknown".into()),
                    )
                }
            }
        } else if initial_state == arkforge_engine::JobState::Reconciling {
            (
                "stillUnknown".into(),
                "a read-only reconciliation is already in progress".into(),
                initial_state.as_str().into(),
            )
        } else if let Some((verdict, reason)) = previous_assessment {
            (verdict, reason, initial_state.as_str().into())
        } else {
            (
                "nothingToReconcile".into(),
                "the job has no unresolved outcome".into(),
                initial_state.as_str().into(),
            )
        };

        let mut payload = Vec::new();
        arkforge_ipc::wire::write_string(&mut payload, 1, &job_id);
        arkforge_ipc::wire::write_string(&mut payload, 2, &verdict);
        arkforge_ipc::wire::write_string(&mut payload, 3, &detail);
        arkforge_ipc::wire::write_string(&mut payload, 4, completeness_name(possible.completeness));
        for effect in &possible.effects.persistent {
            arkforge_ipc::wire::write_string(&mut payload, 5, &persistent_effect_name(effect));
        }
        arkforge_ipc::wire::write_string(&mut payload, 6, &state);
        self.ok(request, payload)
    }

    /// Assesses whether a distinct complete-overwrite plan may supersede an
    /// unknown outcome. This never starts or replays either plan.
    fn plan_superseding_recovery(&self, request: &Request) -> Response {
        use arkforge_ipc::recovery::SupersedingRecoveryAssessment as Assessment;
        let job_id = first_string_field(&request.payload, 1).unwrap_or_default();
        let Some(job) = self.jobs.job(&job_id) else {
            return self.refuse(
                request,
                Status::NotFound,
                "UNKNOWN_JOB",
                &format!("no job {job_id}"),
            );
        };
        let now = self.clock.now_epoch_ms();
        let context = self.recovery_surface_context(&job_id);
        let source = crate::recovery_assessment::source(job, now);
        let closed = source
            .as_ref()
            .is_some_and(|source| source.closed_control.is_some());
        let read_only_tail = self
            .stored_plan_for_job(job.job_id())
            .is_some_and(|stored| {
                crate::recovery_assessment::read_only_postflight_tail(job, &stored, now).is_some()
            });
        let original = if job.state() != arkforge_engine::JobState::OutcomeUnknown {
            SupersedingRecoveryAssessment::Ineligible(RecoveryBlocker::NothingToRecover)
        } else if let Some(context) = &context {
            assess_superseding_recovery(&context.possible, &context.declaration)
        } else if closed {
            SupersedingRecoveryAssessment::Ineligible(RecoveryBlocker::NothingToRecover)
        } else {
            SupersedingRecoveryAssessment::Ineligible(RecoveryBlocker::EffectsUnbounded)
        };
        let mut out = Assessment {
            job_id,
            source,
            ..Assessment::default()
        };
        if job.journal().records().iter().any(|record| {
            record.kind == arkforge_engine::journal::JournalRecordKind::CancellationRequested
        }) {
            out.blocker_code = "recoveryCancellationPending".into();
            out.blocker_reason =
                "A cancellation on the unresolved source job forbids new recovery dispatch.".into();
            return self.ok(request, out.encode());
        }
        match original {
            SupersedingRecoveryAssessment::Eligible { covers } => {
                out.eligible = true;
                out.covered_effects = covers.iter().map(persistent_effect_name).collect();
            }
            SupersedingRecoveryAssessment::Ineligible(RecoveryBlocker::NothingToRecover)
                if closed || read_only_tail =>
            {
                out.blocker_code = "managedControlRecoveryNeedsPlan".into();
                out.blocker_reason = if read_only_tail {
                    "The exact stored plan and journal bound one expired read-only postflight after all prior semantic receipts. A distinct complete recovery plan and fresh authority target proof are required; the old outcome remains unknown."
                } else {
                    "The original journal bounds one expired initial enter-updater request. A distinct complete recovery plan and fresh authority target proof are required; the old outcome remains unknown."
                }.into();
            }
            SupersedingRecoveryAssessment::Ineligible(blocker) => {
                out.blocker_code = recovery_blocker_code(&blocker).into();
                out.blocker_reason = blocker.to_string();
            }
        }
        if let Some(contract) = context
            .as_ref()
            .and_then(|context| context.contract.as_ref())
        {
            out.recovery_contract_id = contract.id.to_string();
            out.recovery_contract_version = contract.version.to_string();
            out.recovery_contract_sha256 = contract.digest.to_hex();
        }
        let candidate_id = first_string_field(&request.payload, 2).unwrap_or_default();
        let candidate_digest = first_string_field(&request.payload, 3).unwrap_or_default();
        if candidate_id.is_empty() && candidate_digest.is_empty() {
            return self.ok(request, out.encode());
        }
        if !out.eligible && out.blocker_code != "managedControlRecoveryNeedsPlan" {
            return self.ok(request, out.encode());
        }
        out.eligible = false;
        let validation = (|| -> Result<_, String> {
            let source = out
                .source
                .as_ref()
                .ok_or("missing original durable source evidence")?;
            let digest = crate::recovery_assessment::parse_digest(&candidate_digest)
                .ok_or("candidate plan digest is malformed")?;
            let id = PlanId::new(&candidate_id).map_err(|error| error.to_string())?;
            let candidate = self
                .engine
                .plans()
                .get(&id, digest)
                .map_err(|error| error.to_string())?;
            let profile = self
                .profiles
                .get(&profile_key(
                    &candidate.envelope.profile.id,
                    candidate.envelope.profile.version,
                ))
                .ok_or("the candidate's profile is not loaded")?;
            if let Some(original) = self.stored_plan_for_job(job.job_id())
                && (original.envelope.profile != candidate.envelope.profile
                    || original.envelope.artifact != candidate.envelope.artifact)
            {
                return Err(
                    "the replacement must retain the source profile and immutable artifact".into(),
                );
            }
            let possible = context
                .as_ref()
                .map(|context| context.possible.effects.persistent.as_slice())
                .unwrap_or(&[]);
            crate::recovery_assessment::validate_candidate(
                candidate, profile, source, possible, now,
            )
            .map_err(str::to_string)?;
            Ok(candidate)
        })();
        match validation {
            Ok(candidate) => {
                out.eligible = true;
                out.blocker_code.clear();
                out.blocker_reason.clear();
                out.candidate_plan_id = candidate.envelope.plan_id.to_string();
                out.candidate_plan_sha256 = candidate.envelope.plan_digest.to_hex();
                out.candidate_profile_sha256 = candidate.envelope.profile.digest.to_hex();
                out.candidate_artifact_sha256 = candidate.envelope.artifact.content_digest.to_hex();
                let contract = candidate
                    .envelope
                    .recovery_contract
                    .as_ref()
                    .expect("candidate validation requires a contract");
                out.recovery_contract_id = contract.id.to_string();
                out.recovery_contract_version = contract.version.to_string();
                out.recovery_contract_sha256 = contract.digest.to_hex();
                out.covered_effects = candidate
                    .envelope
                    .effect_set
                    .persistent
                    .iter()
                    .map(persistent_effect_name)
                    .collect();
            }
            Err(reason) => {
                out.blocker_code = "recoveryCandidateInvalid".into();
                out.blocker_reason = reason;
            }
        }
        self.ok(request, out.encode())
    }

    /// A typed operator/agent guide available on the public read-only socket.
    fn get_recovery_guide(&self, request: &Request) -> Response {
        let job_id = first_string_field(&request.payload, 1).unwrap_or_default();
        let Some(job) = self.jobs.job(&job_id) else {
            return self.refuse(
                request,
                Status::NotFound,
                "UNKNOWN_JOB",
                &format!("no job {job_id}"),
            );
        };
        let context = self.recovery_surface_context(&job_id);
        let mut payload = Vec::new();
        arkforge_ipc::wire::write_string(&mut payload, 1, &job_id);
        arkforge_ipc::wire::write_string(&mut payload, 2, job.state().as_str());
        arkforge_ipc::wire::write_bool(&mut payload, 3, true);
        arkforge_ipc::wire::write_bool(&mut payload, 4, true);
        arkforge_ipc::wire::write_string(
            &mut payload,
            5,
            "do not replay the original intent or reuse its permit",
        );
        arkforge_ipc::wire::write_string(
            &mut payload,
            5,
            "ask the paired authority to validate the same target, artifact, toolchain and \
             uncertain-effect set",
        );
        arkforge_ipc::wire::write_string(
            &mut payload,
            5,
            "if eligible, materialize and authorize a distinct complete-overwrite execution",
        );
        let supported = context
            .as_ref()
            .is_some_and(|context| context.declaration.supports_complete_overwrite);
        arkforge_ipc::wire::write_bool(&mut payload, 6, supported);
        if let Some(context) = context
            && let Some(contract) = context.contract
        {
            arkforge_ipc::wire::write_string(&mut payload, 7, contract.id.as_str());
            arkforge_ipc::wire::write_string(&mut payload, 8, &contract.version.to_string());
            arkforge_ipc::wire::write_string(&mut payload, 9, &contract.digest.to_hex());
        }
        self.ok(request, payload)
    }

    fn recovery_surface_context(&self, job_id: &str) -> Option<RecoverySurfaceContext> {
        let stored = self.stored_plan_for_job(job_id)?;
        let profile = self.profiles.get(&profile_key(
            &stored.envelope.profile.id,
            stored.envelope.profile.version,
        ))?;
        // Same name/version is insufficient after a restart: coverage and
        // data-impact must still be the exact profile sealed by this plan.
        if profile.identity().ok()? != stored.envelope.profile {
            return None;
        }
        let job = self.jobs.job(job_id)?;
        Some(RecoverySurfaceContext {
            possible: assess_possible_effects(
                job.journal(),
                &stored.private_plan,
                &profile.data_impact,
            ),
            declaration: profile.recovery.clone(),
            contract: stored.envelope.recovery_contract,
        })
    }

    fn cancel_job(&mut self, request: &Request) -> Response {
        let job_id = first_string_field(&request.payload, 1).unwrap_or_default();
        let Some(expected_sequence) = first_u64_field(&request.payload, 2) else {
            return self.refuse(
                request,
                Status::InvalidArgument,
                "EXPECTED_SEQUENCE_REQUIRED",
                "cancelJob requires the caller's last observed journal sequence",
            );
        };
        let Some(job) = self.jobs.job(&job_id) else {
            return self.refuse(
                request,
                Status::NotFound,
                "UNKNOWN_JOB",
                &format!("no job {job_id}"),
            );
        };
        if job.last_sequence() != expected_sequence {
            return self.refuse(
                request,
                Status::Refused,
                "STALE_JOB_SEQUENCE",
                &format!(
                    "job {job_id} is at sequence {}, not caller expectation {expected_sequence}",
                    job.last_sequence()
                ),
            );
        }
        match self.jobs.cancel(&job_id, self.clock.now_epoch_ms()) {
            Ok(disposition) => {
                let mut payload = Vec::new();
                arkforge_ipc::wire::write_string(&mut payload, 1, disposition.as_str());
                self.ok(request, payload)
            }
            // "No such job" and "cancelling this job would hide an unresolved
            // effect" are different answers to different questions, and an
            // operator needs to tell them apart.
            Err(crate::jobs::JobError::UnknownJob) => self.refuse(
                request,
                Status::NotFound,
                "UNKNOWN_JOB",
                &format!("no job {job_id}"),
            ),
            Err(error) => self.refuse(request, Status::Refused, error.code(), &error.to_string()),
        }
    }
}
