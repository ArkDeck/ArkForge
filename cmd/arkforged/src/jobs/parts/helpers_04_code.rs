use super::*;

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
            JobError::NoReconcileInFlight => "NO_RECONCILE_IN_FLIGHT",
            JobError::ReconcileNotAvailable(_) => "RECONCILE_NOT_AVAILABLE",
            JobError::WrongRequest { .. } => "WRONG_REQUEST",
            JobError::WrongControlAction { .. } => "WRONG_CONTROL_ACTION",
            JobError::SnapshotExpired => "SNAPSHOT_EXPIRED",
            JobError::CurrentFactsUnavailable => "CURRENT_FACTS_UNAVAILABLE",
            JobError::ContinuityBroken(_) => "CONTINUITY_BROKEN",
            JobError::AdmissionNotReady => "ADMISSION_NOT_READY",
            JobError::ObservedModeMismatch { .. } => "OBSERVED_MODE_MISMATCH",
            JobError::TagNotADigest => "NOT_A_DIGEST",
            JobError::ControlEvidenceMismatch => "CONTROL_EVIDENCE_MISMATCH",
            JobError::ControlFactsIncomplete(_) => "CONTROL_FACTS_INCOMPLETE",
            JobError::InvalidReceiptFactKey(_) => "INVALID_RECEIPT_FACT_KEY",
            JobError::DuplicateReceiptFactKey(_) => "DUPLICATE_RECEIPT_FACT_KEY",
            JobError::ReceiptCarriesForbiddenFacts(_) => "RECEIPT_CARRIES_FORBIDDEN_FACTS",
            JobError::UnknownControlAction(_) => "UNKNOWN_CONTROL_ACTION",
            JobError::CancelWouldHideAnUnresolvedEffect => "CANCEL_NOT_SAFE",
            JobError::IllegalTransition { .. } => "ILLEGAL_TRANSITION",
            JobError::Verification(_) => "PERMIT_REJECTED",
            JobError::Step(_) => "PERMIT_REJECTED",
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
            JobError::NoReconcileInFlight => {
                f.write_str("this job has no read-only reconciliation in flight")
            }
            JobError::ReconcileNotAvailable(state) => {
                write!(f, "a job in {} cannot begin reconciliation", state.as_str())
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
            JobError::ControlFactsIncomplete(keys) => write!(
                f,
                "the accepted control receipt does not reproduce the requested fact(s): {}",
                keys.join(", ")
            ),
            JobError::InvalidReceiptFactKey(key) => {
                write!(f, "receipt fact key {key:?} is not an ArkForge OpaqueId")
            }
            JobError::DuplicateReceiptFactKey(key) => {
                write!(f, "receipt fact key {key:?} appears more than once")
            }
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
            JobError::Step(error) => write!(f, "{error}"),
            JobError::Journal(error) => write!(f, "{error}"),
            JobError::UnusableIdentifier(value) => {
                write!(f, "{value:?} is not usable as a journal identifier")
            }
            JobError::Core(message) => f.write_str(message),
            JobError::RegistryIo(message) => f.write_str(message),
        }
    }
}

impl JobError {
    pub(super) fn from_step(error: StepError) -> Self {
        match error {
            StepError::Verification(error) => JobError::Verification(error),
            StepError::Journal(error) => JobError::Journal(error),
            StepError::UnusableIdentifier(value) => JobError::UnusableIdentifier(value),
            other => JobError::Step(other),
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
/// clear. The authority writes exactly 32 bytes to the daemon's inherited
/// stdin and retains the write end as a liveness capability. The secret never
/// appears in argv or the environment; EOF after these bytes proves that the
/// authority process died and is handled by the daemon binary's monitor.
pub fn read_pairing_secret_from_stdin(epoch: u64) -> Result<ControllerPairingSecret, String> {
    use std::io::Read;
    let mut bytes = [0_u8; 32];
    std::io::stdin()
        .read_exact(&mut bytes)
        .map_err(|error| format!("reading the pairing secret: {error}"))?;
    Ok(ControllerPairingSecret::new(
        PairingEpoch(epoch),
        bytes.to_vec(),
    ))
}

/// The path a job's journal lives at, given the registry root.
pub fn journal_path(root: &Path, job_id: &str) -> PathBuf {
    root.join(format!("{job_id}.journal"))
}
