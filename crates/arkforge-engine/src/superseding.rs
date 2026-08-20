//! Possible effects, read-only reconcile, and superseding-recovery eligibility.
//!
//! architecture.md 14. Three things live here, and all three are read-only by
//! construction — none of them can request a permit, and none of them can
//! reach a device:
//!
//! - [`possible_effects`] turns "an action whose outcome is unknown" into a
//!   conservative union of what it might have done (14.3);
//! - [`reconcile`] turns read-only observations into a verdict, including the
//!   verdict "still unknown" (14.2);
//! - [`assess_superseding_recovery`] answers whether a distinct recovery plan
//!   could be offered at all (14.5).
//!
//! # The rule under all three
//!
//! Never replay (14.1). Nothing here returns "try that again": the strongest
//! thing it can produce is an assessment that a *different* plan, with a new
//! plan id and `SupersedingRecovery` purpose, would be admissible — and even
//! that is only an assessment. Admission stays the authority's (14.6).

use crate::journal::Journal;
use crate::recovery::{PermitLedger, fact};
use arkforge_authority_api::{EffectSetCompleteness, PossibleEffectSet};
use arkforge_core::effect::{DataImpact, DataImpactState, EffectSet, PersistentEffect};
use arkforge_core::ids::{ActionId, OpaqueId};
use arkforge_core::profile::RecoveryDeclaration;
use arkforge_core::projection::StoredProviderPlan;
use arkforge_core::step::{SemanticTarget, WorkflowEffect};
use core::fmt;

/// The conservative union of what every unresolved action might have done.
///
/// "Conservative" has a precise meaning here (architecture.md 14.3): an
/// optional or conditional effect is **in** the union unless durable evidence
/// says it did not happen. The journal is the only durable evidence this
/// function reads, and the only thing it accepts as such is a receipt.
///
/// The set is `Unbounded` — which makes recovery ineligible outright — when a
/// permit is unresolved and this cannot say what it might have touched. That
/// happens when the journal names a step the plan does not contain, which is
/// the honest reading of "a plan and a journal that disagree".
pub fn possible_effects(
    journal: &Journal,
    plan: &StoredProviderPlan,
    declared_data_impact: &DataImpact,
) -> PossibleEffectSet {
    let ledger = PermitLedger::from_journal(journal);
    let unresolved = ledger.unresolved();

    let mut persistent: Vec<PersistentEffect> = Vec::new();
    let mut source_action_ids: Vec<ActionId> = Vec::new();
    let mut completeness = EffectSetCompleteness::Bounded;

    for permit_id in unresolved {
        let Some(step_id) = step_for_permit(journal, permit_id) else {
            // A permit with no step recorded: the intent never named what it
            // was for. Nothing can bound that.
            completeness = EffectSetCompleteness::Unbounded;
            continue;
        };

        let actions: Vec<_> = plan
            .actions
            .iter()
            .filter(|action| action.step_id.as_str() == step_id)
            .collect();
        if actions.is_empty() {
            completeness = EffectSetCompleteness::Unbounded;
            continue;
        }

        for action in actions {
            source_action_ids.push(action.action_id.clone());
            match effect_of(action) {
                EffectBound::None => {}
                EffectBound::Persistent(effect) => {
                    if !persistent.contains(&effect) {
                        persistent.push(effect);
                    }
                }
                EffectBound::Unbounded => completeness = EffectSetCompleteness::Unbounded,
            }
        }
    }

    // Data impact comes from the Profile's own declaration, passed in rather
    // than derived here: a Profile that says a full restore overwrites userdata
    // is the authority on that, and deriving it from whichever partitions
    // happened to be unresolved would under-report whenever the userdata step
    // was the one that completed.
    let data_impact = if persistent.is_empty() && completeness == EffectSetCompleteness::Bounded {
        DataImpact {
            userdata: DataImpactState::Preserved,
            calibration: DataImpactState::Preserved,
            non_volatile_config: DataImpactState::Preserved,
            secure_storage: DataImpactState::Preserved,
        }
    } else {
        *declared_data_impact
    };

    PossibleEffectSet {
        effects: EffectSet {
            persistent,
            transient: Vec::new(),
            data_impact,
        },
        completeness,
        source_action_ids,
    }
}

enum EffectBound {
    /// Read-only: contributes nothing to the union.
    None,
    Persistent(PersistentEffect),
    /// The action declares an effect this cannot name.
    Unbounded,
}

fn effect_of(action: &arkforge_core::projection::PrivateActionRecord) -> EffectBound {
    match action.effect_class {
        WorkflowEffect::ReadOnly => EffectBound::None,
        // A mode change or reboot leaves no persistent effect. It is still
        // recorded as a source action, so an operator can see which unresolved
        // actions were considered.
        WorkflowEffect::Transient | WorkflowEffect::Mutating => EffectBound::None,
        WorkflowEffect::Destructive => match (&action.declared_target, action.declared_range) {
            (Some(SemanticTarget::Partition(partition)), Some(range)) => {
                match action.content_digest {
                    Some(content) => EffectBound::Persistent(PersistentEffect::WritePartition {
                        partition: partition.clone(),
                        range,
                        content,
                    }),
                    None => EffectBound::Persistent(PersistentEffect::ErasePartition {
                        partition: partition.clone(),
                        range,
                    }),
                }
            }
            (Some(SemanticTarget::RawRegion(region)), Some(range)) => {
                match action.content_digest {
                    Some(content) => EffectBound::Persistent(PersistentEffect::WriteRawRegion {
                        region: region.clone(),
                        range,
                        content,
                    }),
                    // A raw region written with unknown content cannot be
                    // bounded: nothing says what is there now.
                    None => EffectBound::Unbounded,
                }
            }
            // Destructive with no named target or no range. This is what the
            // projection validator exists to prevent, so reaching it means the
            // plan is not one this engine can reason about.
            _ => EffectBound::Unbounded,
        },
    }
}

fn step_for_permit<'a>(journal: &'a Journal, permit_id: &str) -> Option<&'a str> {
    journal.records().iter().find_map(|record| {
        let matches_permit = record
            .facts
            .iter()
            .any(|(key, value)| key.as_str() == fact::PERMIT_ID && value == permit_id);
        if !matches_permit {
            return None;
        }
        record
            .facts
            .iter()
            .find(|(key, _)| key.as_str() == fact::STEP_ID)
            .map(|(_, value)| value.as_str())
    })
}

/// What a read-only reconcile could establish.
///
/// architecture.md 14.2. Four verdicts, and "still unknown" is one of them —
/// a reconcile that could only say "succeeded" or "failed" would have to
/// invent one of them when the evidence runs out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileVerdict {
    /// Observations prove the step's declared effect is present.
    Succeeded,
    /// Observations prove it is not, and nothing partial is present.
    ConfirmedNotExecuted,
    /// Some declared effects are present and some are not.
    ConfirmedPartial { present: usize, absent: usize },
    /// The observations do not settle it. Not a failure to reconcile — the
    /// correct answer when the read face cannot see what would decide it.
    StillUnknown { reason: String },
}

/// One read-only observation about one declared effect.
///
/// `Indeterminate` is the case that matters: on this hardware the read face
/// has a window, and a range outside it reads as filler regardless of content
/// (AD-006). An observation from out there decides nothing, and saying so is
/// the whole point of the variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectObservation {
    Present,
    Absent,
    Indeterminate,
}

/// Classifies read-only observations into a verdict.
///
/// Takes observations rather than making them: this function has no transport,
/// which is what makes "reconcile never sends a write" a property of the type
/// rather than a promise in a comment.
pub fn reconcile(observations: &[(PersistentEffect, EffectObservation)]) -> ReconcileVerdict {
    if observations.is_empty() {
        return ReconcileVerdict::StillUnknown {
            reason: "no declared effect was observed".into(),
        };
    }

    let indeterminate = observations
        .iter()
        .filter(|(_, observation)| *observation == EffectObservation::Indeterminate)
        .count();
    if indeterminate > 0 {
        return ReconcileVerdict::StillUnknown {
            reason: format!(
                "{indeterminate} of {} declared effects could not be observed; the read face does \
                 not reach them (architecture.md 16.4)",
                observations.len()
            ),
        };
    }

    let present = observations
        .iter()
        .filter(|(_, observation)| *observation == EffectObservation::Present)
        .count();
    let absent = observations.len() - present;

    match (present, absent) {
        (_, 0) => ReconcileVerdict::Succeeded,
        (0, _) => ReconcileVerdict::ConfirmedNotExecuted,
        _ => ReconcileVerdict::ConfirmedPartial { present, absent },
    }
}

/// Whether a distinct superseding recovery plan could be offered.
///
/// architecture.md 14.5. `Eligible` here does **not** mean a recovery will
/// happen: it means ArkForge could materialize a distinct plan, and admission
/// is still the authority's (14.6). The original job and its outcomeUnknown
/// stay exactly as they were either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupersedingRecoveryAssessment {
    Eligible {
        /// Effects the recovery would have to cover.
        covers: Vec<PersistentEffect>,
    },
    Ineligible(RecoveryBlocker),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryBlocker {
    /// The possible effects could not be bounded. 14.3 makes this fatal to
    /// eligibility by name.
    EffectsUnbounded,
    /// The Profile publishes no complete-overwrite coverage.
    NoPublishedCoverage { unsupported_states: Vec<String> },
    /// A possible effect lies outside what the published recipe covers. This is
    /// ineligible rather than best-effort: a recovery that covers most of the
    /// uncertainty leaves the rest uncertain, which is the state it was meant
    /// to end.
    EffectOutsideCoverage { effects: Vec<String> },
    /// Nothing is unresolved. Not a blocker in the pathological sense — there
    /// is simply nothing to supersede.
    NothingToRecover,
}

pub fn assess_superseding_recovery(
    possible: &PossibleEffectSet,
    recovery: &RecoveryDeclaration,
) -> SupersedingRecoveryAssessment {
    if possible.completeness == EffectSetCompleteness::Unbounded {
        return SupersedingRecoveryAssessment::Ineligible(RecoveryBlocker::EffectsUnbounded);
    }
    if possible.effects.persistent.is_empty() {
        return SupersedingRecoveryAssessment::Ineligible(RecoveryBlocker::NothingToRecover);
    }
    if !recovery.supports_complete_overwrite {
        return SupersedingRecoveryAssessment::Ineligible(RecoveryBlocker::NoPublishedCoverage {
            unsupported_states: recovery.unsupported_states.clone(),
        });
    }

    let uncovered: Vec<String> = possible
        .effects
        .persistent
        .iter()
        .filter(|effect| !covered(effect, recovery))
        .map(describe_effect)
        .collect();
    if !uncovered.is_empty() {
        return SupersedingRecoveryAssessment::Ineligible(RecoveryBlocker::EffectOutsideCoverage {
            effects: uncovered,
        });
    }

    SupersedingRecoveryAssessment::Eligible {
        covers: possible.effects.persistent.clone(),
    }
}

fn covered(effect: &PersistentEffect, recovery: &RecoveryDeclaration) -> bool {
    let Some(partition) = effect.partition() else {
        // Only partition effects can be matched against the published list.
        // Anything else is outside coverage until the declaration names it.
        return false;
    };
    recovery
        .covered_effects
        .iter()
        .any(|covered| covered.as_str() == partition.as_str())
}

fn describe_effect(effect: &PersistentEffect) -> String {
    match effect.partition() {
        Some(partition) => partition.to_string(),
        None => "<non-partition effect>".to_string(),
    }
}

impl fmt::Display for RecoveryBlocker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RecoveryBlocker::EffectsUnbounded => f.write_str(
                "the possible effects of the unresolved actions cannot be bounded, so no plan can \
                 be shown to cover them (architecture.md 14.3)",
            ),
            RecoveryBlocker::NoPublishedCoverage { unsupported_states } => write!(
                f,
                "this profile publishes no complete-overwrite recovery coverage: {}",
                unsupported_states.join("; ")
            ),
            RecoveryBlocker::EffectOutsideCoverage { effects } => write!(
                f,
                "these possible effects lie outside the published recovery coverage: {}",
                effects.join(", ")
            ),
            RecoveryBlocker::NothingToRecover => {
                f.write_str("no action is unresolved; there is nothing to supersede")
            }
        }
    }
}

/// A journal fact key for the assessment, so a caller can record why.
pub fn blocker_id(blocker: &RecoveryBlocker) -> OpaqueId {
    let text = match blocker {
        RecoveryBlocker::EffectsUnbounded => "REC-B01",
        RecoveryBlocker::NoPublishedCoverage { .. } => "REC-B02",
        RecoveryBlocker::EffectOutsideCoverage { .. } => "REC-B03",
        RecoveryBlocker::NothingToRecover => "REC-B04",
    };
    OpaqueId::new(text).expect("literal identifier")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::JournalRecordKind;
    use arkforge_core::digest::{CborValue, sha256};
    use arkforge_core::effect::ByteRange;
    use arkforge_core::ids::{PartitionId, StepId};
    use arkforge_core::projection::{PrivateActionRecord, PrivateActionRole};

    /// A full restore's declared impact, in the neutral vocabulary. Which
    /// device declares this is a Profile's business; this crate never learns
    /// the name of one (architecture.md 4.3).
    fn overwriting_restore() -> DataImpact {
        DataImpact {
            userdata: DataImpactState::Overwritten,
            calibration: DataImpactState::Preserved,
            non_volatile_config: DataImpactState::Preserved,
            secure_storage: DataImpactState::Preserved,
        }
    }

    /// The shape a profile with no published recovery coverage has.
    fn no_published_coverage() -> RecoveryDeclaration {
        RecoveryDeclaration::unsupported()
    }

    fn id(value: &str) -> OpaqueId {
        OpaqueId::new(value).unwrap()
    }

    fn write_action(step: &str, partition: &str, start: u64) -> PrivateActionRecord {
        PrivateActionRecord {
            action_id: ActionId::new(format!("ACT-{partition}")).unwrap(),
            step_id: StepId::new(step).unwrap(),
            role: PrivateActionRole::PrimaryEffect,
            effect_class: WorkflowEffect::Destructive,
            declared_target: Some(SemanticTarget::Partition(
                PartitionId::new(partition).unwrap(),
            )),
            declared_range: Some(ByteRange::new(start, 4096).unwrap()),
            content_digest: Some(sha256(partition.as_bytes())),
            body: CborValue::map(vec![]),
        }
    }

    fn plan(actions: Vec<PrivateActionRecord>) -> StoredProviderPlan {
        StoredProviderPlan { actions }
    }

    /// A journal that got as far as consuming a permit for `step`, with no
    /// receipt — the shape a process death mid-write leaves behind.
    fn journal_mid_write(step: &str) -> Journal {
        let mut journal = Journal::new();
        journal
            .append(JournalRecordKind::JobCreated, 1_000, 1, id("JOB-1"), vec![])
            .unwrap();
        for kind in [
            JournalRecordKind::StepPermitAccepted,
            JournalRecordKind::StepIntentRecorded,
            JournalRecordKind::PermitConsuming,
        ] {
            journal
                .append(
                    kind,
                    2_000,
                    1,
                    id(step),
                    vec![
                        (id(fact::PERMIT_ID), "PERMIT-1".into()),
                        (id(fact::JOB_ID), "JOB-1".into()),
                        (id(fact::STEP_ID), step.to_string()),
                    ],
                )
                .unwrap();
        }
        journal
    }

    #[test]
    fn an_unresolved_write_contributes_its_declared_effect_to_the_union() {
        let journal = journal_mid_write("STEP-SYSTEM");
        let plan = plan(vec![write_action("STEP-SYSTEM", "system", 125_829_120)]);
        let possible = possible_effects(&journal, &plan, &overwriting_restore());

        assert_eq!(possible.completeness, EffectSetCompleteness::Bounded);
        assert_eq!(possible.effects.persistent.len(), 1);
        assert_eq!(
            possible.effects.persistent[0]
                .partition()
                .map(|p| p.to_string()),
            Some("system".to_string())
        );
        assert_eq!(possible.source_action_ids.len(), 1);
    }

    /// A journal that names a step the plan does not contain cannot be bounded.
    /// Guessing "probably nothing" there is how a recovery ends up not covering
    /// the thing that actually happened.
    #[test]
    fn a_step_the_plan_does_not_contain_makes_the_union_unbounded() {
        let journal = journal_mid_write("STEP-SOMETHING-ELSE");
        let plan = plan(vec![write_action("STEP-SYSTEM", "system", 125_829_120)]);
        let possible = possible_effects(&journal, &plan, &overwriting_restore());
        assert_eq!(possible.completeness, EffectSetCompleteness::Unbounded);
        assert!(!possible.permits_recovery_assessment());
    }

    #[test]
    fn a_settled_journal_has_nothing_possible_and_preserves_data() {
        let mut journal = journal_mid_write("STEP-SYSTEM");
        journal
            .append(
                JournalRecordKind::PermitConsumed,
                3_000,
                1,
                id("STEP-SYSTEM"),
                vec![
                    (id(fact::PERMIT_ID), "PERMIT-1".into()),
                    (id(fact::RECEIPT_DIGEST), "abc".into()),
                ],
            )
            .unwrap();
        let plan = plan(vec![write_action("STEP-SYSTEM", "system", 125_829_120)]);
        let possible = possible_effects(&journal, &plan, &overwriting_restore());

        assert!(possible.effects.persistent.is_empty());
        assert_eq!(possible.completeness, EffectSetCompleteness::Bounded);
        assert_eq!(
            possible.effects.data_impact.userdata,
            DataImpactState::Preserved,
            "nothing unresolved means nothing was possibly touched"
        );
    }

    /// The read face has a window. An observation from outside it settles
    /// nothing, and the verdict has to say so rather than round to a failure.
    #[test]
    fn an_indeterminate_observation_keeps_the_outcome_unknown() {
        let effect = PersistentEffect::WritePartition {
            partition: PartitionId::new("system").unwrap(),
            range: ByteRange::new(125_829_120, 4096).unwrap(),
            content: sha256(b"system"),
        };
        let verdict = reconcile(&[(effect, EffectObservation::Indeterminate)]);
        assert!(matches!(verdict, ReconcileVerdict::StillUnknown { .. }));
    }

    #[test]
    fn reconcile_distinguishes_all_four_verdicts() {
        let effect = |name: &str| PersistentEffect::WritePartition {
            partition: PartitionId::new(name).unwrap(),
            range: ByteRange::new(4096, 4096).unwrap(),
            content: sha256(name.as_bytes()),
        };
        assert_eq!(
            reconcile(&[(effect("uboot"), EffectObservation::Present)]),
            ReconcileVerdict::Succeeded
        );
        assert_eq!(
            reconcile(&[(effect("uboot"), EffectObservation::Absent)]),
            ReconcileVerdict::ConfirmedNotExecuted
        );
        assert_eq!(
            reconcile(&[
                (effect("uboot"), EffectObservation::Present),
                (effect("system"), EffectObservation::Absent),
            ]),
            ReconcileVerdict::ConfirmedPartial {
                present: 1,
                absent: 1
            }
        );
        assert!(matches!(
            reconcile(&[]),
            ReconcileVerdict::StillUnknown { .. }
        ));
    }

    /// A profile that publishes no recovery coverage is ineligible, and stays
    /// ineligible until a declaration is published and reviewed — it does not
    /// become eligible because a caller wanted it to. Every profile shipped
    /// today is in this state.
    #[test]
    fn a_profile_with_no_published_coverage_is_ineligible() {
        let journal = journal_mid_write("STEP-SYSTEM");
        let plan = plan(vec![write_action("STEP-SYSTEM", "system", 125_829_120)]);
        let possible = possible_effects(&journal, &plan, &overwriting_restore());

        match assess_superseding_recovery(&possible, &no_published_coverage()) {
            SupersedingRecoveryAssessment::Ineligible(RecoveryBlocker::NoPublishedCoverage {
                unsupported_states,
            }) => assert!(!unsupported_states.is_empty()),
            other => panic!("expected ineligible, got {other:?}"),
        }
    }

    #[test]
    fn unbounded_effects_block_eligibility_before_coverage_is_even_consulted() {
        let possible = PossibleEffectSet {
            effects: EffectSet::read_only(),
            completeness: EffectSetCompleteness::Unbounded,
            source_action_ids: Vec::new(),
        };
        // A declaration that covers everything must not rescue an unbounded set.
        let generous = RecoveryDeclaration {
            supports_complete_overwrite: true,
            version: arkforge_core::identity::Version::new(1, 0, 0),
            covered_effects: vec![id("system"), id("userdata")],
            unsupported_states: Vec::new(),
        };
        assert_eq!(
            assess_superseding_recovery(&possible, &generous),
            SupersedingRecoveryAssessment::Ineligible(RecoveryBlocker::EffectsUnbounded)
        );
    }

    #[test]
    fn an_effect_outside_published_coverage_is_ineligible_not_best_effort() {
        let journal = journal_mid_write("STEP-SYSTEM");
        let plan = plan(vec![write_action("STEP-SYSTEM", "system", 125_829_120)]);
        let possible = possible_effects(&journal, &plan, &overwriting_restore());
        let partial = RecoveryDeclaration {
            supports_complete_overwrite: true,
            version: arkforge_core::identity::Version::new(1, 0, 0),
            // Covers userdata but not system.
            covered_effects: vec![id("userdata")],
            unsupported_states: Vec::new(),
        };
        assert_eq!(
            assess_superseding_recovery(&possible, &partial),
            SupersedingRecoveryAssessment::Ineligible(RecoveryBlocker::EffectOutsideCoverage {
                effects: vec!["system".into()]
            })
        );
    }

    #[test]
    fn every_blocker_has_a_distinct_identifier() {
        let blockers = [
            RecoveryBlocker::EffectsUnbounded,
            RecoveryBlocker::NoPublishedCoverage {
                unsupported_states: Vec::new(),
            },
            RecoveryBlocker::EffectOutsideCoverage {
                effects: Vec::new(),
            },
            RecoveryBlocker::NothingToRecover,
        ];
        let ids: std::collections::BTreeSet<String> = blockers
            .iter()
            .map(|blocker| blocker_id(blocker).to_string())
            .collect();
        assert_eq!(ids.len(), blockers.len());
        for blocker in &blockers {
            assert!(!blocker.to_string().is_empty());
        }
    }
}
