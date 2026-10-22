
impl Service {

    /// Builds the service.
    ///
    /// `campaign` is the acceptance campaign this daemon is running, if any.
    /// It is a parameter with no default because it decides whether any
    /// DAYU200 plan can be executable at all: `None` publishes `HardwareGated`
    /// and the daemon can materialize assessments only, which is what every
    /// ordinary run wants. See [`publish_dayu200_maturity`].
    pub fn new(
        store_root: &Path,
        profiles: Vec<DeviceProfile>,
        transcripts: Vec<String>,
        clock: Clock,
        campaign: Option<&str>,
    ) -> Result<Self, String> {
        let store = ContentAddressedStore::open(store_root, CasQuota::dayu200_default())
            .map_err(|error| error.to_string())?;
        let rockchip = RockchipProvider::new();
        let unisoc = UnisocProvider::new();

        let mut maturity = MaturityRegistry::new();
        let mut profile_map = BTreeMap::new();
        for profile in profiles {
            // Every profile publishes its maturity as it is loaded, so a
            // combination is never merely absent from the registry. Which
            // provider is published for a profile follows the profile's own
            // declared artifact formats — the daemon does not decide that a
            // device belongs to a vendor.
            if profile
                .artifact_formats
                .iter()
                .any(|format| format.as_str() == dayu200::FORMAT_ID)
            {
                for toolchain in [
                    unbound_native_toolchain_identity(),
                    replay_toolchain_identity(),
                ] {
                    publish_dayu200_maturity(
                        &mut maturity,
                        &rockchip,
                        &profile,
                        &toolchain,
                        &HostPlatform::current(),
                        driver_facts_digest(),
                        evidence_set_digest(),
                        campaign,
                    )
                    .map_err(|error| error.to_string())?;
                }
            }
            if profile
                .artifact_formats
                .iter()
                .any(|format| format.as_str() == pac::FORMAT_ID)
            {
                publish_af_v3_maturity(
                    &mut maturity,
                    &unisoc,
                    &profile,
                    &research_toolchain_identity(),
                    &HostPlatform::current(),
                    driver_facts_digest(),
                    evidence_set_digest(),
                )
                .map_err(|error| error.to_string())?;
            }
            let key = profile_key(&profile.id, profile.version);
            if let Some(existing) = profile_map.get(&key) {
                if existing != &profile {
                    return Err(format!(
                        "profile {key} conflicts with already loaded bytes; shipped profiles cannot be overridden"
                    ));
                }
                continue;
            }
            profile_map.insert(key, profile);
        }

        let mut loaded: Vec<Box<dyn DeviceTransport>> = Vec::new();
        for source in transcripts {
            let parsed = transcript::parse(&source).map_err(|error| error.to_string())?;
            loaded.push(Box::new(TranscriptTransport::new(parsed)));
        }
        // One USB transport per loaded profile, because the transport reads
        // its identity table from the profile: which vendor/product pairs in
        // which mode are this device, rather than "any Rockchip in Loader".
        // Native read-only IOKit enumeration opens no HDC server and claims no
        // interface, so it coexists with whatever else owns the board without
        // paying for an `ioreg` process and parser on every rebind poll.
        for profile in profile_map.values() {
            loaded.push(Box::new(UsbTransport::new(
                profile,
                Box::new(NativeIokitEnumerator),
            )));
        }

        Ok(Service {
            store,
            engine: Engine::open(crate::plan_storage::FilePlanStorage::new(
                store_root.join("plans"),
            ))
            .map_err(|error| error.to_string())?,
            rockchip,
            unisoc,
            maturity,
            profiles: profile_map,
            manifests: BTreeMap::new(),
            transports: loaded,
            plan_observations: BTreeMap::new(),
            job_sessions: BTreeMap::new(),
            authorized_rebinds: BTreeSet::new(),
            pending_preparations: VecDeque::new(),
            pending_reconciliations: VecDeque::new(),
            clock,
            jobs: JobRegistry::open(store_root.join("jobs")).map_err(|error| error.to_string())?,
            pairing: None,
            readiness: ExecutionReadiness::default(),
            rockchip_toolchain: None,
            campaign: campaign.map(str::to_string),
        })
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn jobs(&self) -> &JobRegistry {
        &self.jobs
    }

    /// Pairs an authority with this daemon.
    ///
    /// Consuming the secret by value is the point: the caller cannot keep a
    /// copy to hand to something else, and there is no getter to read it back.
    pub fn pair_authority(&mut self, secret: ControllerPairingSecret) {
        self.pairing = Some(secret);
        self.readiness.authority_paired = true;
    }

    /// Binds ArkForge's in-process RockUSB implementation.
    ///
    /// Its backend digest is the currently running `arkforged` executable,
    /// not a vendor binary and not a source-revision string. This makes every
    /// build a distinct maturity combination and prevents an acceptance result
    /// for one native build from authorizing another.
    pub fn bind_native_dispatcher(&mut self, toolchain: BoundToolchain) {
        let identity = ToolchainIdentity {
            id: toolchain.id.clone(),
            ..native_toolchain_identity(toolchain.backend_digest)
        };
        self.bind_rockchip_dispatcher(toolchain, identity);
    }

    fn bind_rockchip_dispatcher(&mut self, toolchain: BoundToolchain, identity: ToolchainIdentity) {
        self.readiness.dispatcher = Some(toolchain);
        self.rockchip_toolchain = Some(identity.clone());
        for profile in self.profiles.values() {
            if !profile
                .artifact_formats
                .iter()
                .any(|format| format.as_str() == dayu200::FORMAT_ID)
            {
                continue;
            }
            // Ignored deliberately: a profile whose identity cannot be formed
            // published nothing at construction either, and a binding is not
            // the place to discover a malformed profile.
            let _ = publish_dayu200_maturity(
                &mut self.maturity,
                &self.rockchip,
                profile,
                &identity,
                &HostPlatform::current(),
                driver_facts_digest(),
                evidence_set_digest(),
                self.campaign.as_deref(),
            );
        }
    }

    /// The RockUSB identity of the backend this daemon actually bound.
    ///
    /// Falls back to a non-executable native placeholder when nothing is bound, which is
    /// the read-only daemon: it materializes assessments, and an assessment
    /// names the combination it was assessed against rather than one it
    /// could run.
    fn bound_rockchip_toolchain_identity(&self) -> ToolchainIdentity {
        self.rockchip_toolchain
            .clone()
            .unwrap_or_else(unbound_native_toolchain_identity)
    }

    /// What this daemon can do. Standing facts, established at startup.
    pub fn readiness(&self) -> &ExecutionReadiness {
        &self.readiness
    }

    pub fn authority_paired(&self) -> bool {
        self.pairing.is_some()
    }

    /// Hands a dispatcher the next piece of work.
    ///
    /// Separate from [`Self::complete_dispatch`] on purpose: the caller takes
    /// the work, **releases this service's lock**, runs it, and comes back.
    /// A single call that did both would hold the lock across a partition
    /// write, and the event stream reporting on that write runs through the
    /// same lock.
    pub fn take_pending_dispatch(&mut self) -> Option<crate::jobs::PendingDispatch> {
        self.jobs.take_pending_dispatch()
    }

    pub fn take_pending_preparation(&mut self) -> Option<PendingPreparation> {
        self.pending_preparations.pop_front()
    }

    /// Hands a read-only reconciliation to the native runner. Like ordinary
    /// dispatch, the slow device reads happen only after the service lock is
    /// released.
    pub fn take_pending_reconcile(&mut self) -> Option<PendingReconcile> {
        self.pending_reconciliations.pop_front()
    }

    pub fn complete_reconcile(
        &mut self,
        work: &PendingReconcile,
        outcome: ReconcileDispatchOutcome,
    ) -> Result<(), crate::jobs::JobError> {
        self.jobs.complete_reconcile(
            &work.job_id,
            &outcome.observations,
            outcome.facts,
            outcome.evidence_digest,
            work.terminal_scope,
            self.clock.now_epoch_ms(),
        )?;
        Ok(())
    }

    /// Classifies control requests whose deadline passed unanswered.
    ///
    /// Runs on the dispatcher's sweep, beside [`Self::take_pending_dispatch`],
    /// so a silent authority costs one deadline rather than leaving the job
    /// parked at `permitConsuming` forever. Returns the ids it classified,
    /// for the caller's log line.
    pub fn expire_stale_controls(&mut self) -> Vec<String> {
        self.jobs.expire_stale_controls(self.clock.now_epoch_ms())
    }

    /// Retries only jobs parked before admission. A missing device leaves the
    /// job parked; ambiguity or an identity/mode mismatch never chooses one.
    pub fn refresh_pending_admissions(&mut self) {
        for job_id in self.jobs.jobs_needing_admission() {
            let allow_unique_rebind = self.authorized_rebinds.contains(&job_id);
            let _ = self.publish_live_admission(&job_id, allow_unique_rebind);
        }
    }

    /// Records what a dispatcher observed.
    pub fn complete_dispatch(
        &mut self,
        job_id: &str,
        outcome: crate::jobs::DispatchOutcome,
    ) -> Result<(), String> {
        let stored = self
            .stored_plan_for_job(job_id)
            .ok_or_else(|| format!("no job {job_id}"))?;
        let completed_step_id = self
            .jobs
            .job(job_id)
            .map(crate::jobs::Job::current_step_id)
            .ok_or_else(|| format!("no job {job_id}"))?;
        let opens_exact_rebind = stored
            .envelope
            .public_steps
            .iter()
            .find(|step| step.step_id.as_str() == completed_step_id)
            .is_some_and(|step| {
                successful_dispatch_requires_rebind(
                    outcome.disposition,
                    step.expected_mode_before.as_ref(),
                    step.expected_mode_after.as_ref(),
                )
            });
        self.jobs
            .complete_dispatch(
                job_id,
                outcome,
                &stored.envelope,
                &stored.private_plan,
                self.clock.now_epoch_ms(),
            )
            .map_err(|error| error.to_string())?;
        if opens_exact_rebind
            && self
                .jobs
                .job(job_id)
                .is_some_and(crate::jobs::Job::needs_admission)
        {
            // DEVICE_RESET has a durable semantic receipt at this point. Its
            // sealed mode transition is therefore as real as an accepted
            // managed-control transition, and the old Loader session must not
            // be reused for the normal-mode postflight. Grant exactly one
            // expected-mode rebind; discovery ambiguity still refuses.
            self.job_sessions.remove(job_id);
            self.authorized_rebinds.insert(job_id.to_string());
        }
        // Same-mode steps reuse and re-read the open session. A proven mode
        // transition instead waits for the one authorized exact rebind; the
        // dispatcher sweep retries while the device is still booting.
        let allow_unique_rebind = self.authorized_rebinds.contains(job_id);
        let _ = self.publish_live_admission(job_id, allow_unique_rebind);
        Ok(())
    }

    fn publish_live_admission(
        &mut self,
        job_id: &str,
        allow_unique_rebind: bool,
    ) -> Result<(), String> {
        let stored = self
            .stored_plan_for_job(job_id)
            .ok_or_else(|| format!("no job {job_id}"))?;
        if !self
            .jobs
            .job(job_id)
            .is_some_and(crate::jobs::Job::needs_admission)
        {
            return Ok(());
        }

        let now = self.clock.now_epoch_ms();
        let existing = if let Some(context) = self.job_sessions.get_mut(job_id) {
            match context.session.reread_identity() {
                Ok(mut observation) => {
                    observation.observed_at_epoch_ms = now;
                    Some((
                        observation,
                        context.session.session_digest(),
                        context.transport_index,
                    ))
                }
                Err(_) => None,
            }
        } else {
            None
        };

        let (observation, session_digest, transport_index) = match existing {
            Some(facts) => facts,
            None if allow_unique_rebind => {
                let plan_context = self
                    .plan_observations
                    .get(&stored.envelope.plan_digest.to_hex())
                    .cloned()
                    .ok_or_else(|| "the plan has no sealed observation context".to_string())?;
                let expected_mode = next_expected_mode(&stored, self.jobs.job(job_id))?;
                let transport = self
                    .transports
                    .get(plan_context.transport_index)
                    .ok_or_else(|| "the plan transport is no longer loaded".to_string())?;
                let mut observations = transport
                    .discover(
                        &TypedDiscoveryFilter {
                            modes: expected_mode.into_iter().collect(),
                            ..TypedDiscoveryFilter::default()
                        },
                        now,
                    )
                    .map_err(|error| error.to_string())?;
                if observations.len() != 1 {
                    return Err(format!(
                        "expected exactly one device for rebind, observed {}",
                        observations.len()
                    ));
                }
                let mut observation = observations.remove(0);
                observation.observed_at_epoch_ms = now;
                let mut session = transport
                    .open_exact(&observation)
                    .map_err(|error| error.to_string())?;
                let mut reread = session
                    .reread_identity()
                    .map_err(|error| error.to_string())?;
                reread.observed_at_epoch_ms = now;
                let digest = session.session_digest();
                self.job_sessions.insert(
                    job_id.to_string(),
                    JobObservationSession {
                        transport_index: plan_context.transport_index,
                        session,
                    },
                );
                // The one authorization granted by an accepted managed
                // control receipt is consumed by opening this exact session.
                self.authorized_rebinds.remove(job_id);
                (reread, digest, plan_context.transport_index)
            }
            None => return Err("the exact admission session no longer observes the device".into()),
        };

        let admission = admission_facts(&stored.envelope, observation, session_digest)?;
        self.jobs
            .request_next_admission(
                job_id,
                &stored.envelope,
                &stored.private_plan,
                &admission,
                now,
            )
            .map_err(|error| error.to_string())?;
        if let Some(context) = self.job_sessions.get_mut(job_id) {
            context.transport_index = transport_index;
        }
        Ok(())
    }

    fn current_facts_for_job(&mut self, job_id: &str) -> Result<CurrentFacts, String> {
        let stored = self
            .stored_plan_for_job(job_id)
            .ok_or_else(|| format!("no job {job_id}"))?;
        let now = self.clock.now_epoch_ms();
        let context = self
            .job_sessions
            .get_mut(job_id)
            .ok_or_else(|| "the job has no open admission session".to_string())?;
        let mut observation = context
            .session
            .reread_identity()
            .map_err(|error| error.to_string())?;
        observation.observed_at_epoch_ms = now;
        let admission = admission_facts(
            &stored.envelope,
            observation,
            context.session.session_digest(),
        )?;
        Ok(CurrentFacts {
            now_epoch_ms: now,
            device_facts_digest: admission
                .observation
                .admission_facts_digest()
                .map_err(|error| error.to_string())?,
            transport_session_digest: Some(admission.transport_session_digest),
            saw_detach_since_snapshot: context.session.saw_detach(),
            provider_facts_digest: admission.provider_facts_digest,
            toolchain_facts_digest: admission.toolchain_facts_digest,
            artifact_facts_digest: admission.artifact_facts_digest,
        })
    }
}
