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
//! [`ExecutionReadiness`] says what this daemon can do, as standing facts — an
//! authority paired and an execution dispatcher bound. Neither can be turned on by a
//! request, and [`ExecutionBlocker`] has no variant meaning "allowed".

#![forbid(unsafe_code)]

pub mod durable;
pub mod journal;
pub mod recovery;
pub mod step;
pub mod superseding;

use arkforge_core::ids::OpaqueId;
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

/// The implementation this daemon has bound for dispatch.
///
/// Identity, not a path: what matters downstream is whether its backend digest
/// is the one a plan's maturity was published against (architecture.md 12.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundToolchain {
    pub id: OpaqueId,
    pub backend_digest: Sha256Digest,
}

/// What this daemon can do, as standing facts rather than a stage name.
///
/// Both fields are established once at startup and neither can be turned on by
/// a request. That is deliberate: execution becoming available is a pairing and
/// a tool binding, not a setting a caller could flip.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecutionReadiness {
    /// An authority handed this daemon a pairing secret, so a permit can be
    /// verified and receipts have somewhere to go.
    pub authority_paired: bool,
    /// An execution dispatcher is bound, so a step this daemon must dispatch
    /// can actually run. Without it a job would walk to its first dispatch and
    /// stop, which is worse than refusing at the start: the permit would
    /// already be spent.
    pub dispatcher: Option<BoundToolchain>,
}

/// Why execution is refused.
///
/// There is no `Allowed` variant and no `reason()` that outlives its subject:
/// each blocker names the thing that is missing, so an operator reading one is
/// not sent to look for a stage that already shipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionBlocker {
    NoPairedAuthority,
    NoDispatcher,
    /// The plan was materialized against one tool and this daemon has another
    /// bound. The toolchain digest is part of the maturity combination
    /// (architecture.md 12.3), so running it here would run a combination
    /// nobody published.
    ToolchainDigestMismatch {
        plan_expects: Sha256Digest,
        daemon_bound: Sha256Digest,
    },
}

impl ExecutionBlocker {
    /// A stable code, so a caller can branch without parsing prose.
    pub fn code(&self) -> &'static str {
        match self {
            ExecutionBlocker::NoPairedAuthority => "NO_PAIRED_AUTHORITY",
            ExecutionBlocker::NoDispatcher => "NO_DISPATCHER",
            ExecutionBlocker::ToolchainDigestMismatch { .. } => "TOOLCHAIN_DIGEST_MISMATCH",
        }
    }
}

impl fmt::Display for ExecutionBlocker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecutionBlocker::NoPairedAuthority => f.write_str(
                "no authority is paired with this daemon, so a StepPermit cannot be verified \
                 against a pairing secret and no session can receive its receipts \
                 (architecture.md 8.6)",
            ),
            ExecutionBlocker::NoDispatcher => f.write_str(
                "no execution dispatcher is bound, so a step this daemon must dispatch could not run. \
                 Refused here rather than at the step: by then the permit would be spent and \
                 the job would have to be reconciled instead of simply not started",
            ),
            ExecutionBlocker::ToolchainDigestMismatch {
                plan_expects,
                daemon_bound,
            } => write!(
                f,
                "the plan was materialized for toolchain {plan_expects} and this daemon has \
                 {daemon_bound} bound; the toolchain digest is part of the maturity combination, \
                 so this pairing was never published (architecture.md 12.3)"
            ),
        }
    }
}

impl ExecutionReadiness {
    /// Blockers that apply to every plan.
    pub fn standing_blockers(&self) -> Vec<ExecutionBlocker> {
        let mut blockers = Vec::new();
        if !self.authority_paired {
            blockers.push(ExecutionBlocker::NoPairedAuthority);
        }
        if self.dispatcher.is_none() {
            blockers.push(ExecutionBlocker::NoDispatcher);
        }
        blockers
    }

    /// Whether this daemon could execute *some* plan.
    ///
    /// Not whether it could execute *a given* plan — that needs the plan, and
    /// [`Self::blockers_for`] is the call that takes one. Reporting "ready"
    /// from here and refusing later is the point: a client learns the standing
    /// facts at handshake and the per-plan ones when it names a plan.
    pub fn is_ready(&self) -> bool {
        self.standing_blockers().is_empty()
    }

    /// Blockers for one plan, standing ones included.
    pub fn blockers_for(&self, plan: &FlashPlanEnvelope) -> Vec<ExecutionBlocker> {
        let mut blockers = self.standing_blockers();
        if let Some(bound) = &self.dispatcher
            && bound.backend_digest != plan.toolchain.backend_digest
        {
            blockers.push(ExecutionBlocker::ToolchainDigestMismatch {
                plan_expects: plan.toolchain.backend_digest,
                daemon_bound: bound.backend_digest,
            });
        }
        blockers
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
    /// The plan is resolved first so a paired, tool-bound daemon reports a
    /// corrupt or unknown plan as exactly that. Callers test the standing
    /// blockers before this — they are facts about the daemon, while the plan
    /// is a fact about one request, and a daemon with no authority answering
    /// "unknown plan" would send an operator to fix a plan that could not have
    /// run either way.
    ///
    /// The per-plan check happens here because it needs the plan: the
    /// toolchain the plan was materialized against must be the toolchain this
    /// daemon has bound.
    pub fn start_execution(
        &mut self,
        plan_id: &PlanId,
        plan_digest: Sha256Digest,
        readiness: &ExecutionReadiness,
    ) -> Result<&StoredPlan, EngineError> {
        let blockers = {
            let stored = self.plans.get(plan_id, plan_digest)?;
            readiness.blockers_for(&stored.envelope)
        };
        if !blockers.is_empty() {
            return Err(EngineError::ExecutionDisabled(blockers));
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
    ExecutionDisabled(Vec<ExecutionBlocker>),
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
            EngineError::ExecutionDisabled(blockers) => {
                let mut first = true;
                for blocker in blockers {
                    if !first {
                        f.write_str("; ")?;
                    }
                    write!(f, "{blocker}")?;
                    first = false;
                }
                Ok(())
            }
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

    fn bound(digest: Sha256Digest) -> ExecutionReadiness {
        ExecutionReadiness {
            authority_paired: true,
            dispatcher: Some(BoundToolchain {
                id: OpaqueId::new("example-tool-fixed").unwrap(),
                backend_digest: digest,
            }),
        }
    }

    #[test]
    fn an_unknown_plan_is_reported_before_the_readiness_is_consulted() {
        let mut engine = Engine::new();
        for readiness in [ExecutionReadiness::default(), bound(sha256(b"tool"))] {
            let error = engine
                .start_execution(&PlanId::new("PLAN-1").unwrap(), sha256(b"plan"), &readiness)
                .unwrap_err();
            // Readiness never masks a real caller error: an operator told
            // "execution is unavailable" would go and fix the wrong thing.
            assert!(
                matches!(error, EngineError::UnknownPlan(_)),
                "{readiness:?}"
            );
        }
    }

    /// A paired daemon with no tool is not ready. Reporting it as ready is how
    /// a job walks to its first dispatch, spends a permit, and stops.
    #[test]
    fn pairing_alone_is_not_readiness() {
        let paired_only = ExecutionReadiness {
            authority_paired: true,
            dispatcher: None,
        };
        assert!(!paired_only.is_ready());
        assert_eq!(
            paired_only.standing_blockers(),
            vec![ExecutionBlocker::NoDispatcher]
        );

        let tool_only = ExecutionReadiness {
            authority_paired: false,
            dispatcher: Some(BoundToolchain {
                id: OpaqueId::new("example-tool-fixed").unwrap(),
                backend_digest: sha256(b"tool"),
            }),
        };
        assert_eq!(
            tool_only.standing_blockers(),
            vec![ExecutionBlocker::NoPairedAuthority]
        );

        // Neither, and both are reported — an operator fixing one at a time
        // should not have to discover the second by trying again.
        let neither = ExecutionReadiness::default();
        assert_eq!(
            neither.standing_blockers(),
            vec![
                ExecutionBlocker::NoPairedAuthority,
                ExecutionBlocker::NoDispatcher
            ]
        );
        assert!(bound(sha256(b"tool")).is_ready());
    }

    #[test]
    fn every_blocker_has_a_distinct_code_and_says_what_is_missing() {
        let blockers = [
            ExecutionBlocker::NoPairedAuthority,
            ExecutionBlocker::NoDispatcher,
            ExecutionBlocker::ToolchainDigestMismatch {
                plan_expects: sha256(b"a"),
                daemon_bound: sha256(b"b"),
            },
        ];
        let codes: std::collections::BTreeSet<&str> =
            blockers.iter().map(|blocker| blocker.code()).collect();
        assert_eq!(codes.len(), blockers.len());
        for blocker in &blockers {
            assert!(!blocker.to_string().is_empty());
        }
    }
}
