use super::*;

impl Service {
    /// Dispatches one request.
    ///
    /// `artifact_stream` is present only for `importArtifact`, which is the one
    /// call that carries bulk content. The daemon never opens a path the caller
    /// named (architecture.md 10.1).
    pub fn handle(
        &mut self,
        session: SessionKind,
        request: &Request,
        artifact_stream: Option<&mut dyn Read>,
    ) -> Response {
        if !session.may_call(request.api) {
            return self.refuse(
                request,
                Status::Refused,
                "SESSION_NOT_PERMITTED",
                &format!(
                    "{} is not available on the {:?} socket",
                    request.api, session
                ),
            );
        }

        match request.api {
            Api::ImportArtifact => self.import_artifact(request, artifact_stream),
            Api::InspectArtifact => self.inspect_artifact(request),
            Api::DiscoverDevices => self.discover_devices(request),
            Api::ProbeDevice => self.probe_device(request),
            Api::MaterializePlan => self.materialize_plan(session, request),
            Api::StartExecution => self.start_execution(request),
            Api::WatchJob => self.watch_job(request),
            Api::GetJob => self.get_job(request),
            Api::ListJobs => self.list_jobs(request),
            Api::CancelJob => self.cancel_job(request),
            Api::SubmitStepPermit => self.submit_step_permit(request),
            Api::SubmitManagedControlReceipt => self.submit_control_receipt(request),
            Api::ReconcileJob => self.reconcile_job(request),
            Api::PlanSupersedingRecovery => self.plan_superseding_recovery(request),
            Api::GetRecoveryGuide => self.get_recovery_guide(request),
        }
    }

    pub(super) fn refuse(
        &self,
        request: &Request,
        status: Status,
        code: &str,
        message: &str,
    ) -> Response {
        Response {
            request_id: request.request_id.clone(),
            api: request.api,
            status,
            payload: ErrorBody {
                code: code.to_string(),
                message: message.to_string(),
            }
            .encode(),
            stream_sequence: 0,
            stream_end: true,
        }
    }

    pub(super) fn ok(&self, request: &Request, payload: Vec<u8>) -> Response {
        Response {
            request_id: request.request_id.clone(),
            api: request.api,
            status: Status::Ok,
            payload,
            stream_sequence: 0,
            stream_end: true,
        }
    }

    pub(super) fn import_artifact(
        &mut self,
        request: &Request,
        artifact_stream: Option<&mut dyn Read>,
    ) -> Response {
        let Some(stream) = artifact_stream else {
            return self.refuse(
                request,
                Status::InvalidArgument,
                "NO_CONTENT_STREAM",
                "importArtifact carries its content on the controller stream",
            );
        };
        let payload = match decode_import_request(&request.payload) {
            Ok(payload) => payload,
            Err(message) => {
                return self.refuse(
                    request,
                    Status::InvalidArgument,
                    "MALFORMED_REQUEST",
                    &message,
                );
            }
        };
        let expected_digest = match payload.expected_sha256.as_deref() {
            None => None,
            Some(hex) => match Sha256Digest::parse_hex(hex) {
                Ok(digest) => Some(digest),
                Err(error) => {
                    return self.refuse(
                        request,
                        Status::InvalidArgument,
                        "MALFORMED_DIGEST",
                        &error.to_string(),
                    );
                }
            },
        };

        match self
            .store
            .import(stream, payload.expected_size_bytes, expected_digest)
        {
            Ok(imported) => {
                let mut out = Vec::new();
                arkforge_ipc::wire::write_string(&mut out, 1, &imported.digest.to_hex());
                arkforge_ipc::wire::write_string(&mut out, 2, &imported.digest.to_hex());
                arkforge_ipc::wire::write_uint64(&mut out, 3, imported.size_bytes);
                arkforge_ipc::wire::write_bool(&mut out, 4, imported.deduplicated);
                self.ok(request, out)
            }
            Err(error) => self.refuse(
                request,
                Status::Refused,
                "IMPORT_REFUSED",
                &error.to_string(),
            ),
        }
    }

    pub(super) fn inspect_artifact(&mut self, request: &Request) -> Response {
        let artifact_id = match first_string_field(&request.payload, 1) {
            Some(value) => value,
            None => {
                return self.refuse(
                    request,
                    Status::InvalidArgument,
                    "MISSING_ARTIFACT_ID",
                    "inspectArtifact requires an artifact id",
                );
            }
        };
        let digest = match Sha256Digest::parse_hex(&artifact_id) {
            Ok(digest) => digest,
            Err(error) => {
                return self.refuse(
                    request,
                    Status::InvalidArgument,
                    "MALFORMED_ARTIFACT_ID",
                    &error.to_string(),
                );
            }
        };

        let manifest = match self.manifests.get(&artifact_id) {
            Some(manifest) => manifest.clone(),
            None => {
                let object = match self.store.open_object(&digest) {
                    Ok(object) => object,
                    Err(error) => {
                        return self.refuse(
                            request,
                            Status::NotFound,
                            "ARTIFACT_NOT_FOUND",
                            &error.to_string(),
                        );
                    }
                };
                drop(object);
                match inspect_stored_container(&self.store, &digest) {
                    Ok(manifest) => {
                        self.manifests.insert(artifact_id.clone(), manifest.clone());
                        manifest
                    }
                    Err(error) => {
                        return self.refuse(request, Status::Refused, "ARTIFACT_REJECTED", &error);
                    }
                }
            }
        };

        self.ok(request, manifest_response(&manifest).encode())
    }

    pub(super) fn discover_devices(&mut self, request: &Request) -> Response {
        let filter = TypedDiscoveryFilter::default();
        let mut out = Vec::new();
        for transport in &self.transports {
            let observations = match transport.discover(&filter, self.clock.now_epoch_ms()) {
                Ok(observations) => observations,
                Err(error) => {
                    return self.refuse(
                        request,
                        Status::Internal,
                        "DISCOVERY_FAILED",
                        &error.to_string(),
                    );
                }
            };
            for observation in observations {
                arkforge_ipc::wire::write_message(&mut out, 1, &encode_observation(&observation));
            }
        }
        self.ok(request, out)
    }

    pub(super) fn probe_device(&mut self, request: &Request) -> Response {
        let observation_id = first_string_field(&request.payload, 1).unwrap_or_default();
        let profile_id = first_string_field(&request.payload, 2).unwrap_or_default();
        let Some(profile) = self.profiles.get(&profile_id) else {
            return self.refuse(
                request,
                Status::NotFound,
                "PROFILE_NOT_FOUND",
                &format!("no loaded profile {profile_id}"),
            );
        };

        for transport in &self.transports {
            let Ok(observations) =
                transport.discover(&TypedDiscoveryFilter::default(), self.clock.now_epoch_ms())
            else {
                continue;
            };
            let Some(observation) = observations
                .iter()
                .find(|candidate| candidate.observation_id.as_str() == observation_id)
            else {
                continue;
            };
            let provider = provider_for(profile, &self.rockchip, &self.unisoc);
            let Some(provider) = provider else {
                return self.refuse(
                    request,
                    Status::NotFound,
                    "NO_PROVIDER_FOR_PROFILE",
                    &format!(
                        "no registered provider handles the artifact formats profile {} declares",
                        profile.id
                    ),
                );
            };
            return match provider.probe(&ProbeContext {
                transport: transport.as_ref(),
                observation,
                profile,
            }) {
                Ok(probe) => {
                    let mut out = Vec::new();
                    arkforge_ipc::wire::write_message(
                        &mut out,
                        1,
                        &encode_observation(&probe.observation),
                    );
                    for (key, value) in &probe.protocol_facts {
                        arkforge_ipc::wire::write_message(
                            &mut out,
                            2,
                            &KeyValue {
                                key: key.to_string(),
                                value: value.clone(),
                            }
                            .encode(),
                        );
                    }
                    arkforge_ipc::wire::write_string(&mut out, 3, profile.id.as_str());
                    arkforge_ipc::wire::write_string(&mut out, 4, &probe.facts_digest.to_hex());
                    self.ok(request, out)
                }
                Err(error) => self.refuse(
                    request,
                    Status::Refused,
                    "PROBE_REFUSED",
                    &error.to_string(),
                ),
            };
        }

        self.refuse(
            request,
            Status::NotFound,
            "OBSERVATION_NOT_FOUND",
            &format!("no observation {observation_id}"),
        )
    }

    // -----------------------------------------------------------------------
    // The controller execution/admission surface (architecture.md 8, 13, 15.3)
    // -----------------------------------------------------------------------

    /// Creates a job for a plan this daemon materialized.
    ///
    /// Everything a caller can supply is an internal identifier. There is no
    /// partition, address, tool, timeout or effect override in the request,
    /// and the plan is resolved out of the daemon's own store rather than
    /// rebuilt from anything the caller sent (architecture.md 15.3).
    pub(super) fn start_execution(&mut self, request: &Request) -> Response {
        // Standing blockers first, because they are facts about this daemon
        // and the payload is a fact about one request. A daemon with no
        // authority answering "unknown plan" would send an operator to fix a
        // plan that could not have run either way.
        let standing = self.readiness.standing_blockers();
        if !standing.is_empty() {
            return self.refuse(
                request,
                Status::Unavailable,
                standing[0].code(),
                &blocker_list(&standing),
            );
        }
        let plan_id = first_string_field(&request.payload, 1).unwrap_or_default();
        let plan_sha256 = first_string_field(&request.payload, 2).unwrap_or_default();
        let execution_purpose = first_string_field(&request.payload, 3).unwrap_or_default();
        let controller_session_id = first_string_field(&request.payload, 4).unwrap_or_default();

        let Ok(plan_id) = PlanId::new(&plan_id) else {
            return self.refuse(
                request,
                Status::InvalidArgument,
                "BAD_PLAN_ID",
                "planId is not a usable identifier",
            );
        };
        let Ok(expected) = Sha256Digest::parse_hex(&plan_sha256) else {
            return self.refuse(
                request,
                Status::InvalidArgument,
                "BAD_PLAN_DIGEST",
                "planSha256 is not 64 hex characters",
            );
        };
        let Ok(controller_session_id) =
            arkforge_core::ids::ControllerSessionId::new(&controller_session_id)
        else {
            return self.refuse(
                request,
                Status::InvalidArgument,
                "BAD_CONTROLLER_SESSION_ID",
                "controllerSessionId is not a usable identifier",
            );
        };

        let readiness = self.readiness.clone();
        let stored = match self.engine.start_execution(&plan_id, expected, &readiness) {
            Ok(stored) => stored.clone(),
            Err(arkforge_engine::EngineError::ExecutionDisabled(blockers)) => {
                let code = blockers
                    .first()
                    .map(|blocker| blocker.code())
                    .unwrap_or("EXECUTION_DISABLED");
                return self.refuse(request, Status::Unavailable, code, &blocker_list(&blockers));
            }
            Err(error) => {
                return self.refuse(
                    request,
                    Status::NotFound,
                    "PLAN_NOT_STARTABLE",
                    &error.to_string(),
                );
            }
        };
        if execution_purpose != stored.envelope.execution_purpose.as_str() {
            return self.refuse(
                request,
                Status::Refused,
                "EXECUTION_PURPOSE_MISMATCH",
                "executionPurpose does not match the sealed plan",
            );
        }

        let Some(observation_context) = self
            .plan_observations
            .get(&stored.envelope.plan_digest.to_hex())
            .cloned()
        else {
            return self.refuse(
                request,
                Status::Unavailable,
                "PLAN_OBSERVATION_UNAVAILABLE",
                "the executable plan has no sealed device observation context",
            );
        };
        let Some(transport) = self.transports.get(observation_context.transport_index) else {
            return self.refuse(
                request,
                Status::Unavailable,
                "PLAN_TRANSPORT_UNAVAILABLE",
                "the transport used to materialize this plan is no longer loaded",
            );
        };
        let mut session = match transport.open_exact(&observation_context.observation) {
            Ok(session) => session,
            Err(error) => {
                return self.refuse(
                    request,
                    Status::Unavailable,
                    "EXACT_DEVICE_UNAVAILABLE",
                    &error.to_string(),
                );
            }
        };
        let now = self.clock.now_epoch_ms();
        let mut observation = match session.reread_identity() {
            Ok(observation) => observation,
            Err(error) => {
                return self.refuse(
                    request,
                    Status::Unavailable,
                    "DEVICE_REREAD_FAILED",
                    &error.to_string(),
                );
            }
        };
        observation.observed_at_epoch_ms = now;
        let admission =
            match admission_facts(&stored.envelope, observation, session.session_digest()) {
                Ok(admission) => admission,
                Err(error) => {
                    return self.refuse(
                        request,
                        Status::Internal,
                        "ADMISSION_FACTS_FAILED",
                        &error,
                    );
                }
            };

        let Some(preparation_profile) = self
            .profiles
            .get(&profile_key(
                &stored.envelope.profile.id,
                stored.envelope.profile.version,
            ))
            .cloned()
        else {
            return self.refuse(
                request,
                Status::Unavailable,
                "PROFILE_NOT_LOADED",
                "the sealed plan's profile is no longer loaded",
            );
        };

        match self.jobs.start(
            &stored.envelope,
            &stored.private_plan,
            controller_session_id,
            &admission,
            now,
        ) {
            Ok(job_id) => {
                self.job_sessions.insert(
                    job_id.clone(),
                    JobObservationSession {
                        transport_index: observation_context.transport_index,
                        session,
                    },
                );
                self.pending_preparations.push_back(PendingPreparation {
                    job_id: job_id.clone(),
                    plan_actions: stored.private_plan.actions.clone(),
                    profile: preparation_profile,
                    artifact_digest: stored.envelope.artifact.content_digest,
                });
                let mut payload = Vec::new();
                arkforge_ipc::wire::write_string(&mut payload, 1, &job_id);
                self.ok(request, payload)
            }
            Err(error) => self.refuse(request, Status::Internal, error.code(), &error.to_string()),
        }
    }

    /// Returns the events a job has published since `from_sequence`.
    ///
    /// A poll rather than a push, because the daemon serves every connection
    /// under one lock and a handler that parked waiting for the next event
    /// would stop every other call — including the one that produces it.
    pub(super) fn watch_job(&mut self, request: &Request) -> Response {
        let Ok(watch) = WatchJobRequest::decode(&request.payload) else {
            return self.refuse(
                request,
                Status::InvalidArgument,
                "BAD_WATCH_REQUEST",
                "watchJob payload does not decode",
            );
        };
        let Some(job) = self.jobs.job(&watch.job_id) else {
            return self.refuse(
                request,
                Status::NotFound,
                "UNKNOWN_JOB",
                &format!("no job {}", watch.job_id),
            );
        };
        let events = job.events_from(watch.from_sequence);
        let mut payload = Vec::new();
        for event in &events {
            arkforge_ipc::wire::write_message(&mut payload, 1, &event.encode());
        }
        self.ok(request, payload)
    }

    pub(super) fn get_job(&self, request: &Request) -> Response {
        let job_id = first_string_field(&request.payload, 1).unwrap_or_default();
        let Some(job) = self.jobs.job(&job_id) else {
            return self.refuse(
                request,
                Status::NotFound,
                "UNKNOWN_JOB",
                &format!("no job {job_id}"),
            );
        };
        let mut payload = Vec::new();
        arkforge_ipc::wire::write_message(&mut payload, 1, &encode_job_summary(job).encode());
        self.ok(request, payload)
    }

    pub(super) fn list_jobs(&self, request: &Request) -> Response {
        let mut payload = Vec::new();
        for job in self.jobs.all_jobs() {
            arkforge_ipc::wire::write_message(&mut payload, 1, &encode_job_summary(job).encode());
        }
        self.ok(request, payload)
    }
}
