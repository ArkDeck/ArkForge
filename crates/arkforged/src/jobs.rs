//! The controller execution/admission surface.
//!
//! architecture.md 8, 13, 15.3. This is the half of execution that faces the
//! authority: a job is created, it asks for a permit, the authority answers,
//! the intent becomes durable, and — for the steps the authority itself
//! performs — it reports back what its own control channel observed.
//!
//! # The shape, and why it is this shape
//!
//! The daemon never calls out. It publishes what it needs on the `watchJob`
//! stream and waits for the authority to call back in. Every message stays
//! client-initiated, which means the authority is free to answer, to refuse, or
//! to stop — and those are three different things to a job, not one.
//!
//! Nothing here blocks. A job advances only when a request arrives, so the
//! service lock is never held across anything slow. That matters because the
//! daemon serves every connection under one mutex: a handler that waited for a
//! device would stop the event stream that was supposed to report on it.
//!
//! # Where dispatch is
//!
//! Next door, in [`crate::dispatch`], and deliberately not here. A step whose
//! private action this daemon runs itself becomes a [`PendingDispatch`] that a
//! dispatcher **takes** — the work leaves the service lock before it runs,
//! comes back through [`JobRegistry::complete_dispatch`], and the lock is held
//! only for the two short journal writes at either end.
//!
//! Between taking the work and reporting on it, the job holds it `in_flight`
//! and nothing else may hand it out. Whether the device changed in that window
//! is unknown, which is exactly what the durable intent already records.

use arkforge_authority_api::{
    ControllerPairingSecret, CurrentFacts, DispatchIntent, FreshnessVerdict, PairingEpoch,
    PermitIntegrityTag, PermitVerificationError, StepAdmissionSnapshot as AuthoritySnapshot,
    StepPermit, evaluate_freshness, verify_permit,
};
use arkforge_core::Sha256Digest;
use arkforge_core::digest::sha256;
use arkforge_core::ids::{AttemptId, ControllerSessionId, JobId, OpaqueId, PlanId, StepId};
use arkforge_core::outcome::ActionDisposition;
use arkforge_core::plan::FlashPlanEnvelope;
use arkforge_core::profile::DeviceProfile;
use arkforge_core::projection::{PrivateActionRecord, PrivateActionRole, StoredProviderPlan};
use arkforge_core::verification::VerificationOutcome;
use arkforge_engine::JobState;
use arkforge_engine::durable::{DurableJournal, DurableJournalError};
use arkforge_engine::journal::JournalRecordKind;
use arkforge_engine::recovery::{PermitDisposition, PermitLedger, fact};
use arkforge_ipc::messages::{
    ActionReceiptSummary, JobEvent, JobEventKind, KeyValue, ManagedControlAction,
    ManagedControlRequest, StepAdmissionSnapshot, SubmitManagedControlReceiptRequest,
};
use arkforge_provider::rockchip_execute::StoredAction;
use arkforge_transport::{DeviceObservation, SerialEvidence};
use core::fmt;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// How long an admission snapshot may be signed against.
///
/// architecture.md 8.3 budgets the snapshot → re-verify → permit → dispatch
/// round trip per step. Sixty seconds is generous for a local UDS round trip
/// and short enough that a permit signed against stale device facts expires
/// rather than arriving late.
pub const SNAPSHOT_LIFETIME_MS: u64 = 60_000;

/// Live facts captured from one exact transport session for one admission.
///
/// The three non-device digests bind the other mutable inputs that could make
/// an otherwise identical private action mean something different. The raw
/// observation is carried to the authority so it can recompute and check the
/// device digest against its own binding.
#[derive(Debug, Clone)]
pub struct AdmissionFacts {
    pub observation: DeviceObservation,
    pub transport_session_digest: Sha256Digest,
    pub provider_facts_digest: Sha256Digest,
    pub toolchain_facts_digest: Sha256Digest,
    pub artifact_facts_digest: Sha256Digest,
}

impl AdmissionFacts {
    fn authority_snapshot(&self) -> Result<AuthoritySnapshot, JobError> {
        Ok(AuthoritySnapshot {
            captured_at_epoch_ms: self.observation.observed_at_epoch_ms,
            freshness_deadline_epoch_ms: self
                .observation
                .observed_at_epoch_ms
                .saturating_add(SNAPSHOT_LIFETIME_MS),
            device_facts_digest: self
                .observation
                .admission_facts_digest()
                .map_err(|error| JobError::Core(error.to_string()))?,
            transport_session_digest: Some(self.transport_session_digest),
            provider_facts_digest: self.provider_facts_digest,
            toolchain_facts_digest: self.toolchain_facts_digest,
            artifact_facts_digest: self.artifact_facts_digest,
        })
    }
}

/// A job the authority is driving.
#[derive(Debug)]
pub struct Job {
    job_id: String,
    controller_session_id: ControllerSessionId,
    plan_id: String,
    plan_digest: Sha256Digest,
    state: JobState,
    /// Index into the plan's public steps.
    step_index: usize,
    total_steps: usize,
    journal: DurableJournal,
    events: Vec<JobEvent>,
    pending: Option<Pending>,
    /// Work a dispatcher took and has not yet reported on. While this is set,
    /// whether the device changed is unknown.
    in_flight: Option<PendingDispatch>,
    /// Set once the job has stopped for a reason that is not a state.
    stopped: Option<JobStop>,
}

/// What a job is waiting for.
#[derive(Debug, Clone)]
enum Pending {
    /// A permit for the step at `step_index`.
    Admission {
        request_id: String,
        snapshot: Box<StepAdmissionSnapshot>,
        freshness: AuthoritySnapshot,
    },
    /// The authority to perform a control action and report what it observed.
    Control {
        request_id: String,
        request: ManagedControlRequest,
        permit_id: String,
    },
    /// This daemon's own dispatcher to run the step's private action.
    ///
    /// Held rather than run here: dispatch can take minutes, and this registry
    /// is reached under the service lock. The dispatcher takes the work,
    /// releases the lock, runs it, and comes back with a receipt.
    Dispatch { work: Box<PendingDispatch> },
}

/// One private action waiting for this daemon's dispatcher.
#[derive(Debug, Clone)]
pub struct PendingDispatch {
    pub job_id: String,
    pub step_id: String,
    pub permit_id: String,
    /// Every private action this step declares, in the order they must run:
    /// read-only sub-actions first, then the one primary effect
    /// (architecture.md 6.3). A step whose sub-action was skipped would have
    /// its primary run against a measurement nobody took — which is how a
    /// readback ends up classifying filler it has no way to interpret.
    pub actions: Vec<PrivateActionRecord>,
    /// The profile whose allowlist the write is checked against. Carried with
    /// the work so the dispatcher never has to reach back into the service for
    /// it — which is what keeps the lock uncontended while it runs.
    pub profile: DeviceProfile,
    /// The archive the images are staged from.
    pub artifact_digest: Sha256Digest,
    /// The journal record that made this step's intent durable.
    pub intent_digest: Sha256Digest,
}

/// What the dispatcher observed.
#[derive(Debug, Clone)]
pub struct DispatchOutcome {
    pub disposition: ActionDisposition,
    pub facts: Vec<(String, String)>,
    pub evidence_digest: Sha256Digest,
    pub verification: Option<VerificationOutcome>,
}

/// Why a job stopped short of finishing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobStop {
    /// Every step is done.
    Completed,
    /// The authority refused an admission. Safe: no intent was recorded.
    RefusedByAuthority { step_id: String, reason: String },
    /// The dispatcher ran the step and could not establish that it succeeded.
    /// Not "it failed": the permit is spent and the device may have changed
    /// (architecture.md 14.1).
    DispatchOutcomeUnknown {
        step_id: String,
        disposition: ActionDisposition,
    },
    /// The authority's control channel did not observe its own semantic
    /// success. Not "nothing happened": a mode change may have taken effect
    /// unobserved (architecture.md 14.1).
    ControlOutcomeUnknown { step_id: String, reason: String },
}

impl fmt::Display for JobStop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JobStop::Completed => f.write_str("every step completed"),
            JobStop::RefusedByAuthority { step_id, reason } => {
                write!(f, "the authority refused admission for {step_id}: {reason}")
            }
            JobStop::DispatchOutcomeUnknown {
                step_id,
                disposition,
            } => write!(
                f,
                "{step_id} dispatched and reported {}; whether the device changed is not \
                 established, and the intent must not be replayed",
                disposition.as_str()
            ),
            JobStop::ControlOutcomeUnknown { step_id, reason } => write!(
                f,
                "the authority's control channel did not confirm {step_id}: {reason}. Whether the \
                 device changed is unknown"
            ),
        }
    }
}

impl Job {
    pub fn job_id(&self) -> &str {
        &self.job_id
    }

    pub fn plan_id(&self) -> &str {
        &self.plan_id
    }

    pub fn plan_digest(&self) -> Sha256Digest {
        self.plan_digest
    }

    pub fn state(&self) -> JobState {
        self.state
    }

    pub fn stopped(&self) -> Option<&JobStop> {
        self.stopped.as_ref()
    }

    pub fn needs_admission(&self) -> bool {
        self.state == JobState::Preflight && self.pending.is_none()
    }

    pub fn expected_mode(&self, envelope: &FlashPlanEnvelope) -> Option<arkforge_core::DeviceMode> {
        envelope
            .public_steps
            .get(self.step_index)
            .and_then(|step| step.expected_mode_before.clone())
    }

    pub fn completed_steps(&self) -> usize {
        self.step_index.min(self.total_steps)
    }

    pub fn total_steps(&self) -> usize {
        self.total_steps
    }

    pub fn current_step_id(&self) -> String {
        match &self.pending {
            Some(Pending::Admission { snapshot, .. }) => snapshot.step_id.clone(),
            Some(Pending::Control { request, .. }) => request.step_id.clone(),
            Some(Pending::Dispatch { work }) => work.step_id.clone(),
            None => self
                .in_flight
                .as_ref()
                .map(|work| work.step_id.clone())
                .unwrap_or_default(),
        }
    }

    pub fn last_sequence(&self) -> u64 {
        self.events.last().map(|event| event.sequence).unwrap_or(0)
    }

    /// Verified durable history used by the read-only recovery assessment.
    pub fn journal(&self) -> &arkforge_engine::journal::Journal {
        self.journal.journal()
    }

    pub fn events_from(&self, from_sequence: u64) -> Vec<JobEvent> {
        self.events
            .iter()
            .filter(|event| event.sequence > from_sequence)
            .cloned()
            .collect()
    }

    fn next_sequence(&self) -> u64 {
        self.events.len() as u64 + 1
    }

    fn publish(
        &mut self,
        kind: JobEventKind,
        at_epoch_ms: u64,
        record_digest: Sha256Digest,
        build: impl FnOnce(&mut JobEvent),
    ) {
        let mut event = JobEvent {
            job_id: self.job_id.clone(),
            sequence: self.next_sequence(),
            kind,
            at_epoch_ms,
            journal_record_sha256: record_digest.as_bytes().to_vec(),
            job_state: self.state.as_str().to_string(),
            ..JobEvent::default()
        };
        build(&mut event);
        self.events.push(event);
    }

    fn move_to(&mut self, next: JobState) -> Result<(), JobError> {
        if !self.state.may_transition_to(next) {
            return Err(JobError::IllegalTransition {
                from: self.state,
                to: next,
            });
        }
        self.state = next;
        Ok(())
    }
}

/// Every job this daemon is running.
#[derive(Debug)]
pub struct JobRegistry {
    root: PathBuf,
    jobs: BTreeMap<String, Job>,
    /// Monotonic, so two jobs started in the same millisecond differ.
    counter: u64,
}

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
        let Some(Pending::Admission {
            request_id: expected,
            snapshot,
            freshness,
        }) = job.pending.clone()
        else {
            return Err(JobError::NoAdmissionPending);
        };
        if expected != request_id {
            return Err(JobError::WrongRequest {
                expected,
                found: request_id.to_string(),
            });
        }
        let Some((mut permit, tag, epoch)) = permit else {
            // A refusal is an answer. Nothing was recorded, so this is safe.
            job.pending = None;
            job.move_to(JobState::AwaitingPermit).ok();
            job.move_to(JobState::CancelledSafe)?;
            let digest = job.journal.append(
                JournalRecordKind::CancellationRequested,
                now_epoch_ms,
                1,
                id(&snapshot.step_id)?,
                vec![(id("refusal")?, refusal.to_string())],
            )?;
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
        let ledger = PermitLedger::from_journal(job.journal.journal());
        let already_consumed = !matches!(
            ledger.disposition(permit.permit_id.as_str()),
            PermitDisposition::Unseen
        );
        let verified = verify_permit(&permit, secret, &intent, already_consumed)
            .map_err(JobError::Verification)?;

        job.move_to(JobState::AwaitingPermit)?;
        let permit_id = verified.permit().permit_id.as_str().to_string();
        let step_id = snapshot.step_id.clone();
        let facts = vec![
            (id(fact::PERMIT_ID)?, permit_id.clone()),
            (id(fact::JOB_ID)?, job.job_id.clone()),
            (id(fact::STEP_ID)?, step_id.clone()),
            (id(fact::ATTEMPT_ID)?, snapshot.attempt_id.clone()),
        ];
        job.journal.append(
            JournalRecordKind::StepPermitAccepted,
            now_epoch_ms,
            1,
            id(&step_id)?,
            facts.clone(),
        )?;
        // After this returns the intent is on stable storage. Nothing before
        // this line may touch the device; everything after must assume it may
        // have been touched.
        let intent_digest = job.journal.append(
            JournalRecordKind::StepIntentRecorded,
            now_epoch_ms,
            1,
            id(&step_id)?,
            facts,
        )?;
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
                    permit_id: permit_id.clone(),
                    expected_facts: expect
                        .into_iter()
                        .map(|(key, value)| KeyValue { key, value })
                        .collect(),
                    deadline_epoch_ms: now_epoch_ms.saturating_add(120_000),
                };
                let digest = job.journal.append(
                    JournalRecordKind::PermitConsuming,
                    now_epoch_ms,
                    1,
                    id(&step_id)?,
                    vec![
                        (id(fact::PERMIT_ID)?, permit_id.clone()),
                        // The journal names what this permit is waiting on. A
                        // job parked here used to be indistinguishable from one
                        // that asked for nothing — the record ended at
                        // `permitConsuming` and the wait lived only in memory —
                        // so the request is now a recorded fact beside the
                        // permit that spent itself asking.
                        (id("controlRequestId")?, control.request_id.clone()),
                        (id("controlAction")?, control.action.as_str().to_string()),
                        (
                            id("controlDeadlineEpochMs")?,
                            control.deadline_epoch_ms.to_string(),
                        ),
                    ],
                )?;
                job.move_to(JobState::Dispatching)?;
                job.pending = Some(Pending::Control {
                    request_id: control.request_id.clone(),
                    request: control.clone(),
                    permit_id,
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
                let digest = job.journal.append(
                    JournalRecordKind::PermitConsuming,
                    now_epoch_ms,
                    1,
                    id(&step_id)?,
                    vec![(id(fact::PERMIT_ID)?, permit_id.clone())],
                )?;
                job.move_to(JobState::Dispatching)?;
                job.pending = Some(Pending::Dispatch {
                    work: Box::new(PendingDispatch {
                        job_id: job.job_id.clone(),
                        step_id: step_id.clone(),
                        permit_id,
                        actions,
                        profile: profile.clone(),
                        artifact_digest,
                        intent_digest,
                    }),
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
            if let Some(Pending::Dispatch { work }) = job.pending.clone() {
                // Marked in-flight by clearing it. Whether the device changed
                // from here on is unknown until a receipt says otherwise, which
                // is exactly what the journal already records.
                job.pending = None;
                let work = *work;
                job.in_flight = Some(work.clone());
                return Some(work);
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
        if job.pending.is_some() || job.state != JobState::Preflight {
            return Err(JobError::AdmissionNotReady);
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
            let (Ok(subject), Ok(outcome_key), Ok(reason_key)) =
                (id(&step_id), id("outcome"), id("reason"))
            else {
                continue;
            };
            // Durable first, state after: an expiry the journal cannot record
            // is left pending and retried on the next sweep, rather than moved
            // to a state the journal never explains.
            let Ok(digest) = job.journal.append(
                JournalRecordKind::OutcomeClassified,
                now_epoch_ms,
                1,
                subject,
                vec![
                    (outcome_key, "outcomeUnknown".to_string()),
                    (reason_key, reason.clone()),
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
        let Some(work) = job.in_flight.take() else {
            return Err(JobError::NoDispatchInFlight);
        };
        let step_id = work.step_id.clone();

        job.journal.append(
            JournalRecordKind::TransportEvidenceRecorded,
            now_epoch_ms,
            1,
            id(&step_id)?,
            outcome
                .facts
                .iter()
                .map(|(key, value)| Ok((id(key)?, value.clone())))
                .collect::<Result<Vec<_>, JobError>>()?,
        )?;

        if outcome.disposition != ActionDisposition::SemanticSuccess {
            job.move_to(JobState::OutcomeUnknown)?;
            let digest = job.journal.append(
                JournalRecordKind::OutcomeClassified,
                now_epoch_ms,
                1,
                id(&step_id)?,
                vec![(id("outcome")?, outcome.disposition.as_str().to_string())],
            )?;
            job.stopped = Some(JobStop::DispatchOutcomeUnknown {
                step_id: step_id.clone(),
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

        job.journal.append(
            JournalRecordKind::SemanticReceiptRecorded,
            now_epoch_ms,
            1,
            id(&step_id)?,
            vec![
                (id(fact::PERMIT_ID)?, work.permit_id.clone()),
                (
                    id(fact::RECEIPT_DIGEST)?,
                    outcome.evidence_digest.to_string(),
                ),
            ],
        )?;
        job.journal.append(
            JournalRecordKind::PermitConsumed,
            now_epoch_ms,
            1,
            id(&step_id)?,
            vec![
                (id(fact::PERMIT_ID)?, work.permit_id.clone()),
                (
                    id(fact::RECEIPT_DIGEST)?,
                    outcome.evidence_digest.to_string(),
                ),
            ],
        )?;
        job.move_to(JobState::ReceiptDurable)?;

        let mut receipt = ActionReceiptSummary {
            job_id: job.job_id.clone(),
            plan_id: job.plan_id.clone(),
            step_id: step_id.clone(),
            action_id: work
                .actions
                .last()
                .map(|action| action.action_id.to_string())
                .unwrap_or_default(),
            attempt_id: String::new(),
            permit_id: work.permit_id.clone(),
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
            Some(VerificationOutcome::TypedSkip { reason, .. }) => {
                receipt.verification_outcome = "typedSkip".into();
                receipt.typed_skip_reason = reason.as_str().to_string();
            }
            Some(VerificationOutcome::Failed { classification, .. }) => {
                receipt.verification_outcome = "failed".into();
                receipt.failure_classification = classification.as_str().to_string();
            }
            None => {}
        }

        let checkpoint = job.journal.append(
            JournalRecordKind::StepCheckpointed,
            now_epoch_ms,
            1,
            id(&step_id)?,
            vec![
                (id(fact::PERMIT_ID)?, work.permit_id),
                (
                    id(fact::RECEIPT_DIGEST)?,
                    outcome.evidence_digest.to_string(),
                ),
            ],
        )?;
        job.move_to(JobState::Checkpointed)?;
        job.publish(
            JobEventKind::ActionReceipt,
            now_epoch_ms,
            checkpoint,
            |event| event.receipt = Some(receipt),
        );
        job.publish(
            JobEventKind::StepCheckpointed,
            now_epoch_ms,
            checkpoint,
            |_| {},
        );

        advance(job, envelope, now_epoch_ms, checkpoint)
    }

    /// Records what the authority's own control channel observed.
    pub fn submit_control_receipt(
        &mut self,
        request: &SubmitManagedControlReceiptRequest,
        envelope: &FlashPlanEnvelope,
        _private_plan: &StoredProviderPlan,
        now_epoch_ms: u64,
    ) -> Result<(), JobError> {
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
        let Some(Pending::Control {
            request_id: expected,
            request: asked,
            permit_id,
        }) = job.pending.clone()
        else {
            return Err(JobError::NoControlPending);
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

        let step_id = asked.step_id.clone();
        job.journal.append(
            JournalRecordKind::TransportEvidenceRecorded,
            now_epoch_ms,
            1,
            id(&step_id)?,
            request
                .facts
                .iter()
                .map(|fact| Ok((id(&fact.key)?, fact.value.clone())))
                .collect::<Result<Vec<_>, JobError>>()?,
        )?;

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
            job.move_to(JobState::OutcomeUnknown)?;
            let digest = job.journal.append(
                JournalRecordKind::OutcomeClassified,
                now_epoch_ms,
                1,
                id(&step_id)?,
                vec![
                    (id("outcome")?, "outcomeUnknown".to_string()),
                    (id("reason")?, request.failure_reason.clone()),
                ],
            )?;
            job.pending = None;
            job.stopped = Some(JobStop::ControlOutcomeUnknown {
                step_id: step_id.clone(),
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

        // An accepted receipt's evidence digest is defined, not opaque: the
        // canonical digest of its own facts. Recomputing it here means a
        // receipt whose facts and evidence disagree — hand-rolled encoders
        // drifting apart is precisely how this channel failed before — is
        // refused at the boundary with a code that names the drift.
        let evidence = digest_from(&request.evidence_sha256).ok_or(JobError::TagNotADigest)?;
        let computed = canonical_facts_digest(&request.facts);
        if computed != evidence {
            return Err(JobError::ControlEvidenceMismatch);
        }

        job.journal.append(
            JournalRecordKind::SemanticReceiptRecorded,
            now_epoch_ms,
            1,
            id(&step_id)?,
            vec![
                (id(fact::PERMIT_ID)?, permit_id.clone()),
                (id(fact::RECEIPT_DIGEST)?, evidence.to_string()),
            ],
        )?;
        job.journal.append(
            JournalRecordKind::PermitConsumed,
            now_epoch_ms,
            1,
            id(&step_id)?,
            vec![
                (id(fact::PERMIT_ID)?, permit_id.clone()),
                (id(fact::RECEIPT_DIGEST)?, evidence.to_string()),
            ],
        )?;
        job.move_to(JobState::ReceiptDurable)?;

        let receipt = ActionReceiptSummary {
            job_id: job.job_id.clone(),
            plan_id: job.plan_id.clone(),
            step_id: step_id.clone(),
            action_id: String::new(),
            attempt_id: String::new(),
            permit_id: permit_id.clone(),
            disposition: "semanticSuccess".into(),
            evidence_sha256: evidence.as_bytes().to_vec(),
            facts: request.facts.clone(),
            ..ActionReceiptSummary::default()
        };
        let checkpoint = job.journal.append(
            JournalRecordKind::StepCheckpointed,
            now_epoch_ms,
            1,
            id(&step_id)?,
            vec![
                (id(fact::PERMIT_ID)?, permit_id),
                (id(fact::RECEIPT_DIGEST)?, evidence.to_string()),
            ],
        )?;
        job.move_to(JobState::Checkpointed)?;
        job.publish(
            JobEventKind::ActionReceipt,
            now_epoch_ms,
            checkpoint,
            |event| event.receipt = Some(receipt),
        );
        job.publish(
            JobEventKind::StepCheckpointed,
            now_epoch_ms,
            checkpoint,
            |_| {},
        );

        job.pending = None;
        advance(job, envelope, now_epoch_ms, checkpoint)
    }

    /// Cancels a job, if cancelling is still safe.
    ///
    /// architecture.md 13.4: before a permit, `CancelledSafe`. Once an intent
    /// is durable there is an unresolved effect, and a job with one may not
    /// return `CancelledSafe` — the honest answer is an unknown outcome.
    pub fn cancel(&mut self, job_id: &str, now_epoch_ms: u64) -> Result<JobState, JobError> {
        let job = self.jobs.get_mut(job_id).ok_or(JobError::UnknownJob)?;
        let ledger = PermitLedger::from_journal(job.journal.journal());
        if !ledger.unresolved().is_empty() {
            return Err(JobError::CancelWouldHideAnUnresolvedEffect);
        }
        if !matches!(
            job.state,
            JobState::Planned
                | JobState::AwaitingStart
                | JobState::Preflight
                | JobState::AwaitingPermit
        ) {
            return Err(JobError::CancelWouldHideAnUnresolvedEffect);
        }
        let digest = job.journal.append(
            JournalRecordKind::CancellationRequested,
            now_epoch_ms,
            1,
            id(job_id)?,
            Vec::new(),
        )?;
        if job.state != JobState::AwaitingPermit {
            job.state = JobState::AwaitingPermit;
        }
        job.move_to(JobState::CancelledSafe)?;
        job.pending = None;
        job.stopped = Some(JobStop::RefusedByAuthority {
            step_id: String::new(),
            reason: "cancelled before any permit".into(),
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
            },
        );
        Ok(JobState::CancelledSafe)
    }
}

/// Moves to the next step, or concludes.
fn advance(
    job: &mut Job,
    envelope: &FlashPlanEnvelope,
    now_epoch_ms: u64,
    checkpoint: Sha256Digest,
) -> Result<(), JobError> {
    job.step_index += 1;
    if job.step_index >= envelope.public_steps.len() {
        job.move_to(JobState::Postflight)?;
        job.move_to(JobState::Succeeded)?;
        job.stopped = Some(JobStop::Completed);
        job.publish(
            JobEventKind::OutcomeClassified,
            now_epoch_ms,
            checkpoint,
            |event| {
                event.facts.push(KeyValue {
                    key: "outcome".into(),
                    value: "succeeded".into(),
                });
            },
        );
        return Ok(());
    }
    job.move_to(JobState::Preflight)?;
    Ok(())
}

/// Publishes the admission the job's current step needs.
fn request_admission(
    job: &mut Job,
    envelope: &FlashPlanEnvelope,
    private_plan: &StoredProviderPlan,
    admission_facts: &AdmissionFacts,
    now_epoch_ms: u64,
) -> Result<(), JobError> {
    let step = envelope
        .public_steps
        .get(job.step_index)
        .ok_or(JobError::PlanHasNoSteps)?;
    let primary = private_plan
        .actions
        .iter()
        .find(|action| {
            action.step_id == step.step_id && action.role == PrivateActionRole::PrimaryEffect
        })
        .ok_or_else(|| JobError::StepHasNoAction(step.step_id.to_string()))?;

    if let Some(expected_mode) = &step.expected_mode_before
        && expected_mode != &admission_facts.observation.mode
    {
        return Err(JobError::ObservedModeMismatch {
            expected: expected_mode.as_str().to_string(),
            found: admission_facts.observation.mode.as_str().to_string(),
        });
    }
    let freshness = admission_facts.authority_snapshot()?;
    let (serial_evidence_kind, serial_sha256) = match admission_facts.observation.serial_evidence {
        SerialEvidence::Absent => ("absent", Vec::new()),
        SerialEvidence::Descriptor { digest } => ("descriptor", digest.as_bytes().to_vec()),
        SerialEvidence::ProtocolUnique { digest } => ("protocolUnique", digest.as_bytes().to_vec()),
    };
    let snapshot = StepAdmissionSnapshot {
        job_id: job.job_id.clone(),
        plan_id: job.plan_id.clone(),
        plan_sha256: job.plan_digest.as_bytes().to_vec(),
        step_id: step.step_id.to_string(),
        attempt_id: format!("ATTEMPT-{}", job.step_index + 1),
        public_step_sha256: step
            .digest()
            .map_err(|error| JobError::Core(error.to_string()))?
            .as_bytes()
            .to_vec(),
        private_action_sha256: primary
            .digest()
            .map_err(|error| JobError::Core(error.to_string()))?
            .as_bytes()
            .to_vec(),
        effect_set_sha256: envelope
            .effect_set
            .digest()
            .map_err(|error| JobError::Core(error.to_string()))?
            .as_bytes()
            .to_vec(),
        admitted_device_facts_sha256: freshness.device_facts_digest.as_bytes().to_vec(),
        observed_mode: admission_facts.observation.mode.as_str().to_string(),
        observed_at_epoch_ms: admission_facts.observation.observed_at_epoch_ms,
        snapshot_lifetime_ms: SNAPSHOT_LIFETIME_MS,
        request_id: format!("{}-{}", job.job_id, job.step_index + 1),
        topology_sha256: admission_facts
            .observation
            .topology_digest
            .as_bytes()
            .to_vec(),
        descriptor_sha256: admission_facts
            .observation
            .descriptor_digest
            .as_bytes()
            .to_vec(),
        serial_sha256,
        serial_evidence_kind: serial_evidence_kind.to_string(),
        protocol_identity: admission_facts
            .observation
            .protocol_identity
            .iter()
            .map(|fact| KeyValue {
                key: fact.key.to_string(),
                value: fact.value.clone(),
            })
            .collect(),
        identity_strength: admission_facts
            .observation
            .identity_strength
            .as_str()
            .to_string(),
        malformed_descriptor: admission_facts.observation.malformed_descriptor,
        transport_session_sha256: admission_facts.transport_session_digest.as_bytes().to_vec(),
    };

    let digest = job.journal.append(
        JournalRecordKind::StepAdmissionRequested,
        now_epoch_ms,
        1,
        id(&snapshot.step_id)?,
        vec![(id(fact::STEP_ID)?, snapshot.step_id.clone())],
    )?;
    job.pending = Some(Pending::Admission {
        request_id: snapshot.request_id.clone(),
        snapshot: Box::new(snapshot.clone()),
        freshness,
    });
    job.publish(
        JobEventKind::StepAdmissionRequested,
        now_epoch_ms,
        digest,
        |event| event.admission = Some(snapshot),
    );
    Ok(())
}

/// A step's private actions, in the order they must run.
///
/// Read-only sub-actions first, then the single primary effect. The projection
/// validator already guarantees there is exactly one primary; this only puts it
/// last, because a sub-action exists to establish something the primary needs.
fn ordered_actions(
    private_plan: &StoredProviderPlan,
    step_id: &str,
) -> Result<Vec<PrivateActionRecord>, JobError> {
    let mut sub: Vec<PrivateActionRecord> = Vec::new();
    let mut primary: Option<PrivateActionRecord> = None;
    for action in &private_plan.actions {
        if action.step_id.as_str() != step_id {
            continue;
        }
        match action.role {
            PrivateActionRole::PrimaryEffect => primary = Some(action.clone()),
            PrivateActionRole::ReadOnlyTransportSubAction => sub.push(action.clone()),
        }
    }
    let primary = primary.ok_or_else(|| JobError::StepHasNoAction(step_id.to_string()))?;
    sub.push(primary);
    Ok(sub)
}

/// Whether this step belongs to the authority's control port, and what it must
/// confirm.
///
/// Read from the plan's own private action rather than from a table kept here:
/// the provider already writes `via: managed-device-control-port` into the
/// action body, and a second copy of that mapping in the daemon is a second
/// copy that can drift.
type ManagedControlSpec = (ManagedControlAction, Vec<(String, String)>);

fn managed_control_for(
    private_plan: &StoredProviderPlan,
    step_id: &str,
) -> Result<Option<ManagedControlSpec>, JobError> {
    let Some(action) = private_plan
        .actions
        .iter()
        .find(|action| action.step_id.as_str() == step_id)
    else {
        return Err(JobError::StepHasNoAction(step_id.to_string()));
    };
    match StoredAction::decode(action) {
        Ok(StoredAction::ManagedControl {
            control_action,
            expect,
        }) => {
            let action = match control_action.as_str() {
                "enter-updater" => ManagedControlAction::EnterUpdater,
                "reboot-to-normal" => ManagedControlAction::RebootToNormal,
                "read-product-facts" => ManagedControlAction::ReadProductFacts,
                "read-build-facts" => ManagedControlAction::ReadBuildFacts,
                other => return Err(JobError::UnknownControlAction(other.to_string())),
            };
            Ok(Some((action, expect)))
        }
        Ok(_) => Ok(None),
        // An action this daemon cannot decode is not a control action, and
        // guessing that it is would hand it to the wrong side.
        Err(_) => Ok(None),
    }
}

fn private_action_digest(snapshot: &StepAdmissionSnapshot) -> Result<Sha256Digest, JobError> {
    digest_from(&snapshot.private_action_sha256).ok_or(JobError::TagNotADigest)
}

/// The defined evidence digest of an accepted control receipt: SHA-256 over
/// `key=value\n` lines with the keys in byte order.
///
/// Both sides compute this — the authority when it builds an accepted receipt,
/// this daemon before taking one — so the facts and their evidence cannot
/// drift apart. Every fact key the port publishes is ASCII and unique, so byte
/// order is total and the lines cannot collide.
pub fn canonical_facts_digest(facts: &[KeyValue]) -> Sha256Digest {
    let mut ordered: Vec<(&str, &str)> = facts
        .iter()
        .map(|fact| (fact.key.as_str(), fact.value.as_str()))
        .collect();
    ordered.sort();
    let mut bytes = Vec::new();
    for (key, value) in ordered {
        bytes.extend_from_slice(key.as_bytes());
        bytes.push(b'=');
        bytes.extend_from_slice(value.as_bytes());
        bytes.push(b'\n');
    }
    sha256::sha256(&bytes)
}

fn digest_from(bytes: &[u8]) -> Option<Sha256Digest> {
    if bytes.len() != 32 {
        return None;
    }
    let mut array = [0u8; 32];
    array.copy_from_slice(bytes);
    Some(Sha256Digest::from_bytes(array))
}

fn id(value: &str) -> Result<OpaqueId, JobError> {
    OpaqueId::new(value).map_err(|_| JobError::UnusableIdentifier(value.to_string()))
}

fn recover_job(path: &Path) -> Result<Job, JobError> {
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
    let last_classification = records
        .iter()
        .rev()
        .find(|record| record.kind == JournalRecordKind::OutcomeClassified)
        .and_then(|record| {
            record
                .facts
                .iter()
                .find(|(key, _)| key.as_str() == "outcome")
                .map(|(_, value)| (value.clone(), record.record_digest, record.at_epoch_ms))
        });

    let (state, outcome, reason, record_digest, at_epoch_ms) =
        if let Some((outcome, digest, at)) = last_classification {
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
                "replayed from durable journal".to_string(),
                digest,
                at,
            )
        } else {
            let unresolved = !PermitLedger::from_journal(journal.journal())
                .unresolved()
                .is_empty();
            let (state, outcome, reason) = if unresolved {
                (
                    JobState::OutcomeUnknown,
                    "outcomeUnknown",
                    "daemon restarted with a durable external intent and no settled receipt",
                )
            } else {
                (
                    JobState::CancelledSafe,
                    "cancelledSafe",
                    "daemon restarted before any unresolved external intent existed",
                )
            };
            let at = records
                .last()
                .map(|record| record.at_epoch_ms.saturating_add(1))
                .unwrap_or(0);
            let digest = journal.append(
                JournalRecordKind::OutcomeClassified,
                at,
                1,
                id(&job_id)?,
                vec![
                    (id("outcome")?, outcome.to_string()),
                    (id("reason")?, reason.to_string()),
                ],
            )?;
            (state, outcome.to_string(), reason.to_string(), digest, at)
        };

    let stopped = match state {
        JobState::Succeeded => Some(JobStop::Completed),
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
        sequence: journal.len() as u64,
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
    Ok(Job {
        job_id,
        controller_session_id,
        plan_id,
        plan_digest,
        state,
        step_index,
        total_steps,
        journal,
        events: vec![event],
        pending: None,
        in_flight: None,
        stopped,
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
    ReceiptCarriesForbiddenFacts(Vec<String>),
    UnknownControlAction(String),
    CancelWouldHideAnUnresolvedEffect,
    IllegalTransition {
        from: JobState,
        to: JobState,
    },
    Verification(PermitVerificationError),
    Journal(DurableJournalError),
    UnusableIdentifier(String),
    Core(String),
    RegistryIo(String),
}

impl JobError {
    /// A stable code for the IPC error body.
    pub fn code(&self) -> &'static str {
        match self {
            JobError::UnknownJob => "UNKNOWN_JOB",
            JobError::PlanHasNoSteps => "PLAN_HAS_NO_STEPS",
            JobError::StepHasNoAction(_) => "STEP_HAS_NO_ACTION",
            JobError::NoAdmissionPending => "NO_ADMISSION_PENDING",
            JobError::NoControlPending => "NO_CONTROL_PENDING",
            JobError::NoDispatchInFlight => "NO_DISPATCH_IN_FLIGHT",
            JobError::WrongRequest { .. } => "WRONG_REQUEST",
            JobError::WrongControlAction { .. } => "WRONG_CONTROL_ACTION",
            JobError::SnapshotExpired => "SNAPSHOT_EXPIRED",
            JobError::CurrentFactsUnavailable => "CURRENT_FACTS_UNAVAILABLE",
            JobError::ContinuityBroken(_) => "CONTINUITY_BROKEN",
            JobError::AdmissionNotReady => "ADMISSION_NOT_READY",
            JobError::ObservedModeMismatch { .. } => "OBSERVED_MODE_MISMATCH",
            JobError::TagNotADigest => "NOT_A_DIGEST",
            JobError::ControlEvidenceMismatch => "CONTROL_EVIDENCE_MISMATCH",
            JobError::ReceiptCarriesForbiddenFacts(_) => "RECEIPT_CARRIES_FORBIDDEN_FACTS",
            JobError::UnknownControlAction(_) => "UNKNOWN_CONTROL_ACTION",
            JobError::CancelWouldHideAnUnresolvedEffect => "CANCEL_NOT_SAFE",
            JobError::IllegalTransition { .. } => "ILLEGAL_TRANSITION",
            JobError::Verification(_) => "PERMIT_REJECTED",
            JobError::Journal(_) => "JOURNAL",
            JobError::UnusableIdentifier(_) => "UNUSABLE_IDENTIFIER",
            JobError::Core(_) => "CORE",
            JobError::RegistryIo(_) => "REGISTRY_IO",
        }
    }
}

impl fmt::Display for JobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JobError::UnknownJob => f.write_str("no such job"),
            JobError::PlanHasNoSteps => f.write_str("the plan declares no steps"),
            JobError::StepHasNoAction(step) => {
                write!(f, "step {step} has no private action in the stored plan")
            }
            JobError::NoAdmissionPending => f.write_str("this job is not waiting for a permit"),
            JobError::NoControlPending => {
                f.write_str("this job is not waiting for a control receipt")
            }
            JobError::NoDispatchInFlight => {
                f.write_str("this job has no dispatch in flight to report on")
            }
            JobError::WrongRequest { expected, found } => write!(
                f,
                "this job is waiting for {expected}, and the submission answers {found}"
            ),
            JobError::WrongControlAction { expected, found } => write!(
                f,
                "the daemon asked for {} and the receipt reports {}",
                expected.as_str(),
                found.as_str()
            ),
            JobError::SnapshotExpired => f.write_str(
                "the admission snapshot expired before the permit arrived; a fresh snapshot has \
                 been published and the permit must be re-issued against it \
                 (architecture.md 8.3)",
            ),
            JobError::CurrentFactsUnavailable => {
                f.write_str("the current device facts could not be read from the admission session")
            }
            JobError::ContinuityBroken(reason) => {
                write!(f, "device continuity broke before dispatch: {reason}")
            }
            JobError::AdmissionNotReady => {
                f.write_str("the job is not waiting for a new admission snapshot")
            }
            JobError::ObservedModeMismatch { expected, found } => write!(
                f,
                "the live device mode is {found}; the next step requires {expected}"
            ),
            JobError::TagNotADigest => f.write_str("a digest field is not 32 bytes"),
            JobError::ControlEvidenceMismatch => f.write_str(
                "the receipt's evidence digest is not the canonical digest of its own facts",
            ),
            JobError::ReceiptCarriesForbiddenFacts(keys) => write!(
                f,
                "the control receipt carries {}, which the typed control port must never pass to \
                 ArkForge (architecture.md 9.2)",
                keys.join(", ")
            ),
            JobError::UnknownControlAction(action) => {
                write!(
                    f,
                    "the plan names control action {action:?}, which is not one of the four"
                )
            }
            JobError::CancelWouldHideAnUnresolvedEffect => f.write_str(
                "this job has a durable intent whose outcome is not settled; it cannot return \
                 cancelledSafe (architecture.md 13.4)",
            ),
            JobError::IllegalTransition { from, to } => write!(
                f,
                "illegal job transition {} -> {}",
                from.as_str(),
                to.as_str()
            ),
            JobError::Verification(error) => write!(f, "{error}"),
            JobError::Journal(error) => write!(f, "{error}"),
            JobError::UnusableIdentifier(value) => {
                write!(f, "{value:?} is not usable as a journal identifier")
            }
            JobError::Core(message) => f.write_str(message),
            JobError::RegistryIo(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for JobError {}

impl From<DurableJournalError> for JobError {
    fn from(error: DurableJournalError) -> Self {
        JobError::Journal(error)
    }
}

/// Reads the pairing secret the authority handed the daemon at startup.
///
/// architecture.md 15.2: held in memory only, never written to disk in the
/// clear. The authority writes it to the daemon's stdin and closes it, so it
/// never appears in an argv or an environment either — both of which other
/// processes on this host can sometimes read, and neither of which the daemon
/// can erase after reading.
pub fn read_pairing_secret_from_stdin(epoch: u64) -> Result<ControllerPairingSecret, String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::io::stdin()
        .read_to_end(&mut bytes)
        .map_err(|error| format!("reading the pairing secret: {error}"))?;
    while matches!(bytes.last(), Some(b'\n') | Some(b'\r')) {
        bytes.pop();
    }
    if bytes.len() < 32 {
        return Err(format!(
            "the pairing secret is {} bytes; at least 32 are required for an HMAC key that is not \
             guessable",
            bytes.len()
        ));
    }
    Ok(ControllerPairingSecret::new(PairingEpoch(epoch), bytes))
}

/// The path a job's journal lives at, given the registry root.
pub fn journal_path(root: &Path, job_id: &str) -> PathBuf {
    root.join(format!("{job_id}.journal"))
}
