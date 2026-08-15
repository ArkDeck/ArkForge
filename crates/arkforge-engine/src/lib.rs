//! # arkforge-engine
//!
//! Plan storage, the job state machine, and the execution gate.
//!
//! - [`journal`] / [`durable`] — the hash-chained record and its file, with
//!   the fsync policy fixed per record kind (architecture.md 13.2);
//! - [`recovery`] — the crash-disposition table of 13.3, derived from the
//!   journal rather than left to a caller's memory;
//! - [`step`] — permit consumption, ordered by types rather than by discipline;
//! - [`superseding`] — possible effects, read-only reconcile, and whether a
//!   distinct recovery plan could be offered (14.2–14.5).
//!
//! What is still missing is an authority. [`ExecutionGate`] says so, and has no
//! variant meaning "allowed": turning execution on is a pairing, not a setting.

#![forbid(unsafe_code)]

pub mod durable;
pub mod journal;
pub mod recovery;
pub mod step;
pub mod superseding;

use arkforge_core::plan::{FlashPlanEnvelope, PlanError};
use arkforge_core::projection::StoredProviderPlan;
use arkforge_core::{PlanId, Sha256Digest};
use core::fmt;
use std::collections::BTreeMap;

/// The states a job moves through (architecture.md 13.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum JobState {
    Planned,
    AwaitingStart,
    Preflight,
    AwaitingPermit,
    StepIntentDurable,
    Dispatching,
    ReceiptDurable,
    Checkpointed,
    RebindWait,
    ReadOnlyDispatch,
    Postflight,
    Succeeded,
    ConfirmedFailed,
    CancelledSafe,
    OutcomeUnknown,
    Reconciling,
    /// ArkForge can offer a distinct recovery plan. This is *not* a success
    /// state for the original job, and the original outcome stays unknown
    /// (architecture.md 13.1).
    RecoveryAssessable,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Planned => "planned",
            JobState::AwaitingStart => "awaitingStart",
            JobState::Preflight => "preflight",
            JobState::AwaitingPermit => "awaitingPermit",
            JobState::StepIntentDurable => "stepIntentDurable",
            JobState::Dispatching => "dispatching",
            JobState::ReceiptDurable => "receiptDurable",
            JobState::Checkpointed => "checkpointed",
            JobState::RebindWait => "rebindWait",
            JobState::ReadOnlyDispatch => "readOnlyDispatch",
            JobState::Postflight => "postflight",
            JobState::Succeeded => "succeeded",
            JobState::ConfirmedFailed => "confirmedFailed",
            JobState::CancelledSafe => "cancelledSafe",
            JobState::OutcomeUnknown => "outcomeUnknown",
            JobState::Reconciling => "reconciling",
            JobState::RecoveryAssessable => "recoveryAssessable",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            JobState::Succeeded
                | JobState::ConfirmedFailed
                | JobState::CancelledSafe
                | JobState::RecoveryAssessable
        )
    }

    /// Whether an external effect may still be dispatched from this state.
    pub fn permits_external_dispatch(self) -> bool {
        matches!(self, JobState::StepIntentDurable)
    }

    /// The transitions architecture.md 13.1 draws.
    pub fn may_transition_to(self, next: JobState) -> bool {
        use JobState::*;
        match self {
            Planned => next == AwaitingStart,
            AwaitingStart => next == Preflight,
            Preflight => matches!(next, AwaitingPermit | ReadOnlyDispatch),
            ReadOnlyDispatch => matches!(next, Preflight | Postflight | CancelledSafe),
            AwaitingPermit => matches!(next, StepIntentDurable | CancelledSafe),
            StepIntentDurable => matches!(next, Dispatching | OutcomeUnknown),
            Dispatching => matches!(next, ReceiptDurable | OutcomeUnknown),
            ReceiptDurable => matches!(next, Checkpointed | OutcomeUnknown),
            Checkpointed => matches!(next, RebindWait | Preflight | Postflight),
            RebindWait => matches!(next, Preflight | OutcomeUnknown),
            Postflight => matches!(next, Succeeded | ConfirmedFailed),
            OutcomeUnknown => matches!(next, Reconciling | RecoveryAssessable),
            Reconciling => matches!(next, Succeeded | ConfirmedFailed | OutcomeUnknown),
            Succeeded | ConfirmedFailed | CancelledSafe | RecoveryAssessable => false,
        }
    }
}

/// A stored plan and the private plan it projects onto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPlan {
    pub envelope: FlashPlanEnvelope,
    pub private_plan: StoredProviderPlan,
}

/// Holds materialized plans for the lifetime of the daemon.
///
/// Durable plan storage across restarts is AF-V2, together with the journal
/// that makes a stored plan meaningful after a crash. What matters already is
/// that a plan handed back out is verified against its own digest first:
/// architecture.md 6.3 forbids trusting a plan whose store may have been
/// corrupted.
#[derive(Debug, Default)]
pub struct PlanStore {
    plans: BTreeMap<String, StoredPlan>,
}

impl PlanStore {
    pub fn new() -> Self {
        PlanStore {
            plans: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, plan: StoredPlan) -> Result<(), EngineError> {
        plan.envelope
            .verify_self_digest()
            .map_err(EngineError::Plan)?;
        self.plans
            .insert(plan.envelope.plan_id.as_str().to_string(), plan);
        Ok(())
    }

    /// Fetches a plan, checking its digest and the caller's expectation.
    pub fn get(
        &self,
        plan_id: &PlanId,
        expected_digest: Sha256Digest,
    ) -> Result<&StoredPlan, EngineError> {
        let stored = self
            .plans
            .get(plan_id.as_str())
            .ok_or_else(|| EngineError::UnknownPlan(plan_id.to_string()))?;

        let recomputed = stored
            .envelope
            .recompute_digest()
            .map_err(EngineError::Plan)?;
        if recomputed != stored.envelope.plan_digest {
            return Err(EngineError::StoreCorruption {
                plan_id: plan_id.to_string(),
                stored: stored.envelope.plan_digest,
                recomputed,
            });
        }
        if stored.envelope.plan_digest != expected_digest {
            return Err(EngineError::PlanDigestMismatch {
                plan_id: plan_id.to_string(),
                expected: expected_digest,
                stored: stored.envelope.plan_digest,
            });
        }
        Ok(stored)
    }

    pub fn len(&self) -> usize {
        self.plans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.plans.is_empty()
    }
}

/// Why execution is refused.
///
/// There is no `Allowed` variant, and the reason is stated as what is still
/// missing rather than as a stage name. A gate whose text outlives the thing it
/// describes is worse than no gate: it tells an operator a false story about
/// why the daemon will not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionGate {
    /// The durable journal, the crash-disposition table and single-use permit
    /// consumption are here ([`durable`], [`recovery`], [`step`]). No authority
    /// is paired with this daemon, so no permit can be verified against a real
    /// pairing secret, and no session exists to receive the receipts.
    NoPairedAuthority,
}

impl ExecutionGate {
    pub const CURRENT: ExecutionGate = ExecutionGate::NoPairedAuthority;

    /// The gate that applies, or `None` when nothing blocks execution.
    ///
    /// Pairing is the whole condition, and deliberately so: an authority that
    /// handed this daemon a secret is an authority that can sign permits and
    /// receive receipts, and there is nothing else to switch on. A build flag
    /// or a config key here would be a way to turn execution on without one.
    pub fn evaluate(authority_paired: bool) -> Option<ExecutionGate> {
        if authority_paired {
            None
        } else {
            Some(ExecutionGate::NoPairedAuthority)
        }
    }

    pub fn reason(self) -> &'static str {
        match self {
            ExecutionGate::NoPairedAuthority => {
                "startExecution is unavailable: no authority is paired with this daemon, so a \
                 StepPermit cannot be verified against a pairing secret and no session can \
                 receive its receipts (architecture.md 8.6, 21.2 Stage A)"
            }
        }
    }
}

/// The engine.
#[derive(Debug, Default)]
pub struct Engine {
    plans: PlanStore,
}

impl Engine {
    pub fn new() -> Self {
        Engine {
            plans: PlanStore::new(),
        }
    }

    pub fn plans(&self) -> &PlanStore {
        &self.plans
    }

    pub fn plans_mut(&mut self) -> &mut PlanStore {
        &mut self.plans
    }

    /// Resolves the plan a job would run, or says why it may not.
    ///
    /// The plan is resolved and verified before the gate is consulted *here*,
    /// so a paired daemon reports a corrupt or unknown plan as exactly that.
    /// Callers are expected to test the gate first — it is a standing fact
    /// about the daemon, while the plan is a fact about one request, and an
    /// unpaired daemon answering "unknown plan" would send an operator to fix
    /// a plan that could not have run either way.
    pub fn start_execution(
        &mut self,
        plan_id: &PlanId,
        plan_digest: Sha256Digest,
        authority_paired: bool,
    ) -> Result<&StoredPlan, EngineError> {
        self.plans.get(plan_id, plan_digest)?;
        if let Some(gate) = ExecutionGate::evaluate(authority_paired) {
            return Err(EngineError::ExecutionDisabled(gate));
        }
        self.plans.get(plan_id, plan_digest)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    UnknownPlan(String),
    PlanDigestMismatch {
        plan_id: String,
        expected: Sha256Digest,
        stored: Sha256Digest,
    },
    StoreCorruption {
        plan_id: String,
        stored: Sha256Digest,
        recomputed: Sha256Digest,
    },
    IllegalTransition {
        from: JobState,
        to: JobState,
    },
    ExecutionDisabled(ExecutionGate),
    Plan(PlanError),
    Journal(journal::JournalError),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::UnknownPlan(plan) => write!(f, "no stored plan {plan}"),
            EngineError::PlanDigestMismatch {
                plan_id,
                expected,
                stored,
            } => write!(
                f,
                "plan {plan_id}: caller expects digest {expected}, store holds {stored}"
            ),
            EngineError::StoreCorruption {
                plan_id,
                stored,
                recomputed,
            } => write!(
                f,
                "plan {plan_id} is corrupt: stored digest {stored}, contents hash to {recomputed}"
            ),
            EngineError::IllegalTransition { from, to } => write!(
                f,
                "illegal job transition {} -> {}",
                from.as_str(),
                to.as_str()
            ),
            EngineError::ExecutionDisabled(gate) => f.write_str(gate.reason()),
            EngineError::Plan(error) => write!(f, "{error}"),
            EngineError::Journal(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for EngineError {}

#[cfg(test)]
mod tests {
    use super::*;
    use arkforge_core::digest::sha256;

    #[test]
    fn no_state_but_step_intent_durable_permits_an_external_dispatch() {
        let all = [
            JobState::Planned,
            JobState::AwaitingStart,
            JobState::Preflight,
            JobState::AwaitingPermit,
            JobState::StepIntentDurable,
            JobState::Dispatching,
            JobState::ReceiptDurable,
            JobState::Checkpointed,
            JobState::RebindWait,
            JobState::ReadOnlyDispatch,
            JobState::Postflight,
            JobState::Succeeded,
            JobState::ConfirmedFailed,
            JobState::CancelledSafe,
            JobState::OutcomeUnknown,
            JobState::Reconciling,
            JobState::RecoveryAssessable,
        ];
        for state in all {
            assert_eq!(
                state.permits_external_dispatch(),
                state == JobState::StepIntentDurable,
                "{state:?}"
            );
        }
    }

    #[test]
    fn an_unknown_outcome_never_returns_to_a_dispatching_state() {
        // architecture.md 14.1: never replay. The state machine has no edge
        // from OutcomeUnknown back to anything that can dispatch.
        for target in [
            JobState::Preflight,
            JobState::AwaitingPermit,
            JobState::StepIntentDurable,
            JobState::Dispatching,
        ] {
            assert!(
                !JobState::OutcomeUnknown.may_transition_to(target),
                "OutcomeUnknown must not reach {target:?}"
            );
        }
        assert!(JobState::OutcomeUnknown.may_transition_to(JobState::Reconciling));
        assert!(JobState::OutcomeUnknown.may_transition_to(JobState::RecoveryAssessable));
    }

    #[test]
    fn reconcile_may_conclude_but_may_not_dispatch() {
        assert!(JobState::Reconciling.may_transition_to(JobState::Succeeded));
        assert!(JobState::Reconciling.may_transition_to(JobState::ConfirmedFailed));
        assert!(JobState::Reconciling.may_transition_to(JobState::OutcomeUnknown));
        assert!(!JobState::Reconciling.may_transition_to(JobState::Dispatching));
    }

    #[test]
    fn recovery_assessable_is_terminal_and_is_not_success() {
        assert!(JobState::RecoveryAssessable.is_terminal());
        assert_ne!(JobState::RecoveryAssessable, JobState::Succeeded);
        for target in [JobState::Succeeded, JobState::Preflight] {
            assert!(!JobState::RecoveryAssessable.may_transition_to(target));
        }
    }

    #[test]
    fn cancellation_before_a_permit_is_safe_but_not_after_dispatch() {
        assert!(JobState::AwaitingPermit.may_transition_to(JobState::CancelledSafe));
        // Once an intent is durable, the only honest answers are a receipt or
        // an unknown outcome (architecture.md 13.4).
        assert!(!JobState::StepIntentDurable.may_transition_to(JobState::CancelledSafe));
        assert!(!JobState::Dispatching.may_transition_to(JobState::CancelledSafe));
    }

    #[test]
    fn an_unknown_plan_is_reported_before_the_gate_is_consulted() {
        let mut engine = Engine::new();
        for paired in [false, true] {
            let error = engine
                .start_execution(&PlanId::new("PLAN-1").unwrap(), sha256(b"plan"), paired)
                .unwrap_err();
            // The gate never masks a real caller error: an operator told
            // "execution is unavailable" would go and fix the wrong thing.
            assert!(matches!(error, EngineError::UnknownPlan(_)), "paired={paired}");
        }
    }

    #[test]
    fn pairing_is_the_whole_condition_the_gate_switches_on() {
        assert_eq!(
            ExecutionGate::evaluate(false),
            Some(ExecutionGate::NoPairedAuthority)
        );
        assert_eq!(ExecutionGate::evaluate(true), None);
        assert!(ExecutionGate::CURRENT.reason().contains("unavailable"));
    }
}
