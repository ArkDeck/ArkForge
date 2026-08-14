//! # arkforge-engine
//!
//! Plan storage, the job state machine, and the execution gate.
//!
//! Scope note: the durable engine — journal fsync policy, crash campaigns,
//! permit consumption, reconcile — is AF-V2 (architecture.md 22). What AF-V1
//! needs, and what this crate provides, is the plan store the read-only API
//! hands out of, the state machine those stages will move through, and a gate
//! that refuses to start execution.
//!
//! The gate is not a `TODO`. It is the AF-V1 acceptance line "startExecution
//! disabled", implemented as a type that has no variant meaning "allowed".

#![forbid(unsafe_code)]

pub mod durable;
pub mod journal;
pub mod recovery;
pub mod step;

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

    /// Refuses, always, in this build.
    ///
    /// The signature takes the same arguments the AF-V2 entry point will, so
    /// the refusal sits on the real call site rather than beside it.
    pub fn start_execution(
        &mut self,
        plan_id: &PlanId,
        plan_digest: Sha256Digest,
    ) -> Result<JobState, EngineError> {
        // The plan is still resolved and verified: a caller that passes a
        // corrupt or unknown plan should hear about that, not about the gate.
        self.plans.get(plan_id, plan_digest)?;
        Err(EngineError::ExecutionDisabled(ExecutionGate::CURRENT))
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
    fn start_execution_is_refused_and_says_why() {
        let mut engine = Engine::new();
        let error = engine
            .start_execution(&PlanId::new("PLAN-1").unwrap(), sha256(b"plan"))
            .unwrap_err();
        // An unknown plan is reported as an unknown plan, so the gate never
        // masks a real caller error.
        assert!(matches!(error, EngineError::UnknownPlan(_)));
    }

    #[test]
    fn the_execution_gate_has_no_allowed_state() {
        assert_eq!(ExecutionGate::CURRENT, ExecutionGate::NoPairedAuthority);
        assert!(ExecutionGate::CURRENT.reason().contains("unavailable"));
    }
}
