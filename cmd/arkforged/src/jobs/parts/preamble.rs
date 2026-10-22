use super::*;

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
    pub(super) fn authority_snapshot(&self) -> Result<AuthoritySnapshot, JobError> {
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
    pub(super) job_id: String,
    pub(super) controller_session_id: ControllerSessionId,
    pub(super) plan_id: String,
    pub(super) plan_digest: Sha256Digest,
    pub(super) state: JobState,
    /// Index into the plan's public steps.
    pub(super) step_index: usize,
    pub(super) total_steps: usize,
    pub(super) journal: DurableJournal,
    pub(super) events: Vec<JobEvent>,
    pub(super) pending: Option<Pending>,
    /// Work a dispatcher took and has not yet reported on. While this is set,
    /// whether the device changed is unknown.
    pub(super) in_flight: Option<InFlightDispatch>,
    /// Set once the job has stopped for a reason that is not a state.
    pub(super) stopped: Option<JobStop>,
    /// Durable operator intent to stop after the current non-interruptible
    /// action reaches a settled checkpoint.
    pub(super) cancellation_requested: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancellationDisposition {
    QueuedAtSafeBoundary,
    CancelledSafe,
    AlreadyTerminal,
    OutcomeUnknown,
}

impl CancellationDisposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QueuedAtSafeBoundary => "queued-at-safe-boundary",
            Self::CancelledSafe => "cancelled-safe",
            Self::AlreadyTerminal => "already-terminal",
            Self::OutcomeUnknown => "outcome-unknown",
        }
    }
}

/// What a job is waiting for.
#[derive(Debug)]
pub(super) enum Pending {
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
        transaction: DispatchInFlight,
    },
    /// This daemon's own dispatcher to run the step's private action.
    ///
    /// Held rather than run here: dispatch can take minutes, and this registry
    /// is reached under the service lock. The dispatcher takes the work,
    /// releases the lock, runs it, and comes back with a receipt.
    Dispatch {
        work: Box<PendingDispatch>,
        transaction: DispatchInFlight,
    },
}

/// One dispatch handed across the service-lock boundary. The work may be
/// cloned for the native runner; the linear transaction token may not.
#[derive(Debug)]
pub(super) struct InFlightDispatch {
    pub(super) work: PendingDispatch,
    pub(super) transaction: DispatchInFlight,
}

/// One private action waiting for this daemon's dispatcher.
#[derive(Debug, Clone)]
pub struct PendingDispatch {
    pub job_id: String,
    pub step_id: String,
    /// Every private action this step declares, in the order they must run:
    /// read-only sub-actions first, then the one primary effect
    /// (architecture.md 6.3). A step whose sub-action was skipped would have
    /// its primary run against a measurement nobody took — which is how a
    /// readback ends up classifying filler it has no way to interpret.
    pub actions: Vec<PrivateActionRecord>,
    /// The whole validated private plan. The dispatcher uses only its declared
    /// write inputs to bind a persistent staging-cache key; it never broadens
    /// the effects of the current step from this list.
    pub plan_actions: Vec<PrivateActionRecord>,
    /// The profile whose allowlist the write is checked against. Carried with
    /// the work so the dispatcher never has to reach back into the service for
    /// it — which is what keeps the lock uncontended while it runs.
    pub profile: DeviceProfile,
    /// The archive the images are staged from.
    pub artifact_digest: Sha256Digest,
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
    /// A read-only verification reached a conclusive negative verdict.
    ConfirmedFailure { reason: String },
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
    /// A read-only reconciliation completed without enough evidence to settle
    /// the original execution.
    ReconcileStillUnknown { reason: String },
}

impl fmt::Display for JobStop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JobStop::Completed => f.write_str("every step completed"),
            JobStop::ConfirmedFailure { reason } => f.write_str(reason),
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
            JobStop::ReconcileStillUnknown { reason } => f.write_str(reason),
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
        matches!(self.state, JobState::Preflight | JobState::RebindWait) && self.pending.is_none()
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
            Some(Pending::Dispatch { work, .. }) => work.step_id.clone(),
            None => self
                .in_flight
                .as_ref()
                .map(|in_flight| in_flight.work.step_id.clone())
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

    pub fn last_reconcile_assessment(&self) -> Option<(String, String)> {
        self.journal
            .journal()
            .records()
            .iter()
            .rev()
            .find(|record| record.kind == JournalRecordKind::RecoveryAssessmentPublished)
            .and_then(|record| {
                let verdict = record
                    .facts
                    .iter()
                    .find(|(key, _)| key.as_str() == "reconcileVerdict")?
                    .1
                    .clone();
                let reason = record
                    .facts
                    .iter()
                    .find(|(key, _)| key.as_str() == "reason")
                    .map(|(_, value)| value.clone())
                    .unwrap_or_default();
                Some((verdict, reason))
            })
    }

    pub(super) fn next_sequence(&self) -> u64 {
        // A live job publishes every event, so length and cursor happen to
        // agree. Recovery intentionally reconstructs only durable receipts and
        // classifications; those events retain their original (possibly
        // sparse) cursors. Count + 1 would therefore move backwards as soon as
        // a recovered outcomeUnknown job entered reconciliation.
        self.events
            .iter()
            .map(|event| event.sequence)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
    }

    pub(super) fn publish(
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

    pub(super) fn move_to(&mut self, next: JobState) -> Result<(), JobError> {
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
    pub(super) root: PathBuf,
    pub(super) jobs: BTreeMap<String, Job>,
    /// Monotonic, so two jobs started in the same millisecond differ.
    pub(super) counter: u64,
}
