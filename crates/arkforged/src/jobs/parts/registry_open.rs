
impl JobRegistry {

    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self::open(root).expect("job registry must be readable")
    }

    /// Opens the registry and classifies every journal left by an earlier
    /// daemon before serving requests. It never resumes an unresolved external
    /// intent: that is `outcomeUnknown`, not work to replay.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, JobError> {
        let root = root.into();
        std::fs::create_dir_all(&root)
            .map_err(|error| JobError::RegistryIo(format!("{}: {error}", root.display())))?;
        let mut registry = JobRegistry {
            root: root.clone(),
            jobs: BTreeMap::new(),
            counter: 0,
        };
        let entries = std::fs::read_dir(&root)
            .map_err(|error| JobError::RegistryIo(format!("{}: {error}", root.display())))?;
        for entry in entries {
            let entry = entry.map_err(|error| JobError::RegistryIo(error.to_string()))?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("journal") {
                continue;
            }
            let job = recover_job(&path)?;
            registry.counter = registry.counter.saturating_add(1);
            registry.jobs.insert(job.job_id.clone(), job);
        }
        Ok(registry)
    }

    pub fn job(&self, job_id: &str) -> Option<&Job> {
        self.jobs.get(job_id)
    }

    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    pub fn jobs_needing_admission(&self) -> Vec<String> {
        self.jobs
            .values()
            .filter(|job| job.needs_admission())
            .map(|job| job.job_id.clone())
            .collect()
    }

    pub fn all_jobs(&self) -> impl Iterator<Item = &Job> {
        self.jobs.values()
    }

    /// Creates a job and publishes the first admission it needs.
    ///
    /// The plan is walked from step zero. A plan with no steps is refused
    /// rather than completing instantly: a job that did nothing and reported
    /// success is the least useful possible answer.
    pub fn start(
        &mut self,
        envelope: &FlashPlanEnvelope,
        private_plan: &StoredProviderPlan,
        controller_session_id: ControllerSessionId,
        admission_facts: &AdmissionFacts,
        now_epoch_ms: u64,
    ) -> Result<String, JobError> {
        if envelope.public_steps.is_empty() {
            return Err(JobError::PlanHasNoSteps);
        }
        self.counter += 1;
        let job_id = format!("JOB-{:016X}-{:04}", now_epoch_ms, self.counter);
        let path = self.root.join(format!("{job_id}.journal"));
        let (journal, _) = DurableJournal::open(&path).map_err(JobError::Journal)?;

        let mut job = Job {
            job_id: job_id.clone(),
            controller_session_id,
            plan_id: envelope.plan_id.as_str().to_string(),
            plan_digest: envelope.plan_digest,
            state: JobState::Planned,
            step_index: 0,
            total_steps: envelope.public_steps.len(),
            journal,
            events: Vec::new(),
            pending: None,
            in_flight: None,
            stopped: None,
            cancellation_requested: false,
        };

        let digest = job.journal.append(
            JournalRecordKind::JobCreated,
            now_epoch_ms,
            1,
            id(&job_id)?,
            vec![
                (id(fact::JOB_ID)?, job_id.clone()),
                (id(fact::PLAN_ID)?, job.plan_id.clone()),
                (id("planDigest")?, job.plan_digest.to_hex()),
                (id("totalSteps")?, job.total_steps.to_string()),
                (
                    id("controllerSessionId")?,
                    job.controller_session_id.to_string(),
                ),
            ],
        )?;
        job.move_to(JobState::AwaitingStart)?;
        job.move_to(JobState::Preflight)?;
        job.publish(JobEventKind::StateChanged, now_epoch_ms, digest, |_| {});

        request_admission(
            &mut job,
            envelope,
            private_plan,
            admission_facts,
            now_epoch_ms,
        )?;
        self.jobs.insert(job_id.clone(), job);
        Ok(job_id)
    }
}
