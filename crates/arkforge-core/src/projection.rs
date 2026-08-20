//! Public/private plan projection — the invariant that keeps a private action
//! from doing something the public plan never declared.
//!
//! architecture.md 6. The private execution plan (addresses, tool actions,
//! packets, FDL parameters) never crosses the Agent/App API, but every one of
//! its actions is covered by a digest that does cross the authority boundary,
//! and every destructive action is projected onto exactly one public step whose
//! declared effect appears in the plan's EffectSet.

use crate::digest::{
    CanonicalCbor, CborError, CborValue, Domain, Sha256Digest, digest_canonical, digest_in_domain,
    digest_ordered,
};
use crate::effect::{ByteRange, EffectSet, PersistentEffect, TransientEffect};
use crate::ids::{ActionId, StepId};
use crate::step::{FlashStepKind, PublicFlashStep, SemanticTarget, WorkflowEffect};
use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

/// What a private action is to its public step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PrivateActionRole {
    /// Produces the public step's declared effect. Exactly one per step.
    PrimaryEffect,
    /// A read-only transport sub-action inside the public step
    /// (architecture.md 6.3). May not carry any persistent effect.
    ReadOnlyTransportSubAction,
}

impl PrivateActionRole {
    pub fn as_str(self) -> &'static str {
        match self {
            PrivateActionRole::PrimaryEffect => "primaryEffect",
            PrivateActionRole::ReadOnlyTransportSubAction => "readOnlyTransportSubAction",
        }
    }
}

impl CanonicalCbor for PrivateActionRole {
    fn to_cbor(&self) -> CborValue {
        CborValue::text(self.as_str())
    }
}

/// One private provider action.
///
/// `body` is provider-shaped and stays inside the daemon; only
/// `digest()` crosses to the authority. The declared target/range/content
/// fields are the part Core can check against the public step, which is what
/// makes "private address exceeds public semantic target" detectable here
/// instead of during review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateActionRecord {
    pub action_id: ActionId,
    pub step_id: StepId,
    pub role: PrivateActionRole,
    pub effect_class: WorkflowEffect,
    pub declared_target: Option<SemanticTarget>,
    pub declared_range: Option<ByteRange>,
    pub content_digest: Option<Sha256Digest>,
    pub body: CborValue,
}

impl PrivateActionRecord {
    /// `privateActionDigest[i]` per architecture.md 6.2.
    pub fn digest(&self) -> Result<Sha256Digest, CborError> {
        digest_canonical(Domain::PrivateAction, self)
    }
}

impl CanonicalCbor for PrivateActionRecord {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("actionId", self.action_id.to_cbor()),
            ("stepId", self.step_id.to_cbor()),
            ("role", self.role.to_cbor()),
            ("effectClass", self.effect_class.to_cbor()),
            (
                "declaredTarget",
                match &self.declared_target {
                    Some(target) => target.to_cbor(),
                    None => CborValue::Null,
                },
            ),
            (
                "declaredRange",
                match &self.declared_range {
                    Some(range) => range.to_cbor(),
                    None => CborValue::Null,
                },
            ),
            (
                "contentDigest",
                match &self.content_digest {
                    Some(digest) => digest.to_cbor(),
                    None => CborValue::Null,
                },
            ),
            ("body", self.body.clone()),
        ])
    }
}

/// Binds one public step to one private action digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionDigestBinding {
    pub step_id: StepId,
    pub action_id: ActionId,
    pub role: PrivateActionRole,
    pub private_action_digest: Sha256Digest,
}

impl CanonicalCbor for ActionDigestBinding {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("stepId", self.step_id.to_cbor()),
            ("actionId", self.action_id.to_cbor()),
            ("role", self.role.to_cbor()),
            ("privateActionDigest", self.private_action_digest.to_cbor()),
        ])
    }
}

/// The digests a validated projection produces (architecture.md 6.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionDigests {
    pub per_action: Vec<ActionDigestBinding>,
    pub provider_execution_plan_digest: Sha256Digest,
    pub public_projection_digest: Sha256Digest,
}

/// The stored private plan. Never serialized to the Agent/App API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredProviderPlan {
    pub actions: Vec<PrivateActionRecord>,
}

impl StoredProviderPlan {
    pub fn action(&self, action_id: &ActionId) -> Option<&PrivateActionRecord> {
        self.actions.iter().find(|a| &a.action_id == action_id)
    }
}

/// Validates the projection between a public step list, a private action list
/// and the declared EffectSet, and returns the resulting digests.
///
/// Every rejection here corresponds to a line in architecture.md 6.3 or 5.4.
pub fn validate_projection(
    public_steps: &[PublicFlashStep],
    private_plan: &StoredProviderPlan,
    effect_set: &EffectSet,
) -> Result<ProjectionDigests, ProjectionError> {
    if public_steps.is_empty() {
        return Err(ProjectionError::EmptyPlan);
    }

    let mut seen_steps: BTreeSet<&str> = BTreeSet::new();
    for step in public_steps {
        if !seen_steps.insert(step.step_id.as_str()) {
            return Err(ProjectionError::DuplicateStepId(step.step_id.to_string()));
        }
        step.validate_self_consistent()
            .map_err(ProjectionError::Step)?;
    }

    let mut seen_actions: BTreeSet<&str> = BTreeSet::new();
    for action in &private_plan.actions {
        if !seen_actions.insert(action.action_id.as_str()) {
            return Err(ProjectionError::DuplicateActionId(
                action.action_id.to_string(),
            ));
        }
        if !seen_steps.contains(action.step_id.as_str()) {
            return Err(ProjectionError::ActionWithoutPublicStep {
                action: action.action_id.to_string(),
                step: action.step_id.to_string(),
            });
        }
    }

    // One primary action per step; sub-actions must be read-only.
    let mut primary_by_step: BTreeMap<&str, &PrivateActionRecord> = BTreeMap::new();
    for action in &private_plan.actions {
        match action.role {
            PrivateActionRole::PrimaryEffect => {
                if primary_by_step
                    .insert(action.step_id.as_str(), action)
                    .is_some()
                {
                    return Err(ProjectionError::MultiplePrimaryActions(
                        action.step_id.to_string(),
                    ));
                }
            }
            PrivateActionRole::ReadOnlyTransportSubAction => {
                if action.effect_class != WorkflowEffect::ReadOnly {
                    return Err(ProjectionError::SubActionNotReadOnly {
                        action: action.action_id.to_string(),
                        effect: action.effect_class,
                    });
                }
            }
        }
    }

    let mut per_action = Vec::with_capacity(private_plan.actions.len());
    for step in public_steps {
        let primary = primary_by_step
            .get(step.step_id.as_str())
            .ok_or_else(|| ProjectionError::StepWithoutPrimaryAction(step.step_id.to_string()))?;

        let primary_digest = primary.digest().map_err(ProjectionError::Cbor)?;
        if primary_digest != step.private_action_digest {
            return Err(ProjectionError::StepActionDigestMismatch {
                step: step.step_id.to_string(),
                declared: step.private_action_digest,
                computed: primary_digest,
            });
        }

        // A private action may never be more powerful than the public step
        // that authorizes it.
        if primary.effect_class > step.effect {
            return Err(ProjectionError::PrivateEffectExceedsPublic {
                step: step.step_id.to_string(),
                private_effect: primary.effect_class,
                public_effect: step.effect,
            });
        }

        if let Some(private_target) = primary.declared_target.as_ref() {
            match step.semantic_target.as_ref() {
                Some(public_target) if public_target == private_target => {}
                _ => {
                    return Err(ProjectionError::PrivateTargetOutsidePublicTarget {
                        step: step.step_id.to_string(),
                        action: primary.action_id.to_string(),
                    });
                }
            }
        }

        if let (Some(public_content), Some(private_content)) =
            (step.content_digest, primary.content_digest)
            && public_content != private_content
        {
            return Err(ProjectionError::ContentDigestMismatch {
                step: step.step_id.to_string(),
                public: public_content,
                private: private_content,
            });
        }
        if step.content_digest.is_some() && primary.content_digest.is_none() {
            return Err(ProjectionError::PrivateActionMissingContentDigest {
                step: step.step_id.to_string(),
                action: primary.action_id.to_string(),
            });
        }

        per_action.push(ActionDigestBinding {
            step_id: step.step_id.clone(),
            action_id: primary.action_id.clone(),
            role: PrivateActionRole::PrimaryEffect,
            private_action_digest: primary_digest,
        });

        for action in private_plan
            .actions
            .iter()
            .filter(|a| a.step_id == step.step_id && a.role != PrivateActionRole::PrimaryEffect)
        {
            per_action.push(ActionDigestBinding {
                step_id: step.step_id.clone(),
                action_id: action.action_id.clone(),
                role: action.role,
                private_action_digest: action.digest().map_err(ProjectionError::Cbor)?,
            });
        }
    }

    validate_effect_coverage(public_steps, &primary_by_step, effect_set)?;

    let ordered_digests: Vec<Sha256Digest> = per_action
        .iter()
        .map(|binding| binding.private_action_digest)
        .collect();
    let provider_execution_plan_digest =
        digest_ordered(Domain::ProviderExecutionPlan, &ordered_digests);

    let mapping = CborValue::array(per_action.iter().map(|b| b.to_cbor()).collect());
    let mapping_bytes = mapping
        .to_canonical_bytes()
        .map_err(ProjectionError::Cbor)?;
    let public_projection_digest = digest_in_domain(Domain::PublicProjection, &mapping_bytes);

    Ok(ProjectionDigests {
        per_action,
        provider_execution_plan_digest,
        public_projection_digest,
    })
}

/// Every declared effect is claimed by exactly one step, and every effectful
/// step claims a declared effect (architecture.md 5.4, 6.3).
fn validate_effect_coverage(
    public_steps: &[PublicFlashStep],
    primary_by_step: &BTreeMap<&str, &PrivateActionRecord>,
    effect_set: &EffectSet,
) -> Result<(), ProjectionError> {
    let mut persistent_claimed = vec![false; effect_set.persistent.len()];
    let mut transient_claimed = vec![false; effect_set.transient.len()];

    for step in public_steps {
        match step.kind {
            FlashStepKind::WriteTarget | FlashStepKind::EraseTarget => {
                let target = step.semantic_target.as_ref().ok_or_else(|| {
                    ProjectionError::EffectfulStepWithoutTarget(step.step_id.to_string())
                })?;
                let primary = primary_by_step
                    .get(step.step_id.as_str())
                    .expect("primary presence checked by caller");
                let index = effect_set
                    .persistent
                    .iter()
                    .enumerate()
                    .position(|(index, effect)| {
                        !persistent_claimed[index]
                            && persistent_effect_matches(effect, step, target, primary)
                    })
                    .ok_or_else(|| ProjectionError::StepEffectNotDeclared {
                        step: step.step_id.to_string(),
                        kind: step.kind,
                    })?;
                persistent_claimed[index] = true;
            }
            FlashStepKind::EnsureMode
            | FlashStepKind::Reboot
            | FlashStepKind::LoadEphemeralAgent => {
                if let Some(index) =
                    effect_set
                        .transient
                        .iter()
                        .enumerate()
                        .position(|(index, effect)| {
                            !transient_claimed[index] && transient_effect_matches(effect, step)
                        })
                {
                    transient_claimed[index] = true;
                } else {
                    return Err(ProjectionError::StepEffectNotDeclared {
                        step: step.step_id.to_string(),
                        kind: step.kind,
                    });
                }
            }
            _ => {}
        }
    }

    if let Some(index) = persistent_claimed.iter().position(|claimed| !claimed) {
        return Err(ProjectionError::UnclaimedPersistentEffect(index));
    }
    if let Some(index) = transient_claimed.iter().position(|claimed| !claimed) {
        return Err(ProjectionError::UnclaimedTransientEffect(index));
    }
    Ok(())
}

fn persistent_effect_matches(
    effect: &PersistentEffect,
    step: &PublicFlashStep,
    target: &SemanticTarget,
    primary: &PrivateActionRecord,
) -> bool {
    let (effect_target_matches, effect_range) = match (effect, target) {
        (
            PersistentEffect::WritePartition {
                partition,
                range,
                content,
            },
            SemanticTarget::Partition(step_partition),
        ) => {
            if step.kind != FlashStepKind::WriteTarget {
                return false;
            }
            if step.content_digest != Some(*content) {
                return false;
            }
            (partition == step_partition, Some(range))
        }
        (
            PersistentEffect::ErasePartition { partition, range },
            SemanticTarget::Partition(step_partition),
        ) => {
            if step.kind != FlashStepKind::EraseTarget {
                return false;
            }
            (partition == step_partition, Some(range))
        }
        (
            PersistentEffect::WriteRawRegion {
                region,
                range,
                content,
            },
            SemanticTarget::RawRegion(step_region),
        ) => {
            if step.kind != FlashStepKind::WriteTarget {
                return false;
            }
            if step.content_digest != Some(*content) {
                return false;
            }
            (region == step_region, Some(range))
        }
        _ => (false, None),
    };
    if !effect_target_matches {
        return false;
    }
    // The private action's range may not exceed the declared public effect.
    match (primary.declared_range, effect_range) {
        (Some(private_range), Some(effect_range)) => effect_range.contains_range(&private_range),
        (None, _) => true,
        (Some(_), None) => false,
    }
}

fn transient_effect_matches(effect: &TransientEffect, step: &PublicFlashStep) -> bool {
    match (effect, step.kind) {
        (TransientEffect::EnterMode { from, to }, FlashStepKind::EnsureMode) => {
            step.expected_mode_before.as_ref() == Some(from)
                && step.expected_mode_after.as_ref() == Some(to)
        }
        (TransientEffect::Reboot { target_mode }, FlashStepKind::Reboot) => {
            step.expected_mode_after.as_ref() == Some(target_mode)
        }
        (TransientEffect::LoadEphemeralAgent { .. }, FlashStepKind::LoadEphemeralAgent) => true,
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionError {
    EmptyPlan,
    DuplicateStepId(String),
    DuplicateActionId(String),
    ActionWithoutPublicStep {
        action: String,
        step: String,
    },
    MultiplePrimaryActions(String),
    StepWithoutPrimaryAction(String),
    SubActionNotReadOnly {
        action: String,
        effect: WorkflowEffect,
    },
    StepActionDigestMismatch {
        step: String,
        declared: Sha256Digest,
        computed: Sha256Digest,
    },
    PrivateEffectExceedsPublic {
        step: String,
        private_effect: WorkflowEffect,
        public_effect: WorkflowEffect,
    },
    PrivateTargetOutsidePublicTarget {
        step: String,
        action: String,
    },
    ContentDigestMismatch {
        step: String,
        public: Sha256Digest,
        private: Sha256Digest,
    },
    PrivateActionMissingContentDigest {
        step: String,
        action: String,
    },
    EffectfulStepWithoutTarget(String),
    StepEffectNotDeclared {
        step: String,
        kind: FlashStepKind,
    },
    UnclaimedPersistentEffect(usize),
    UnclaimedTransientEffect(usize),
    Step(crate::step::StepError),
    Cbor(CborError),
}

impl fmt::Display for ProjectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProjectionError::EmptyPlan => f.write_str("a plan must contain at least one step"),
            ProjectionError::DuplicateStepId(step) => write!(f, "duplicate step id {step}"),
            ProjectionError::DuplicateActionId(action) => {
                write!(f, "duplicate private action id {action}")
            }
            ProjectionError::ActionWithoutPublicStep { action, step } => write!(
                f,
                "private action {action} projects onto unknown public step {step}"
            ),
            ProjectionError::MultiplePrimaryActions(step) => {
                write!(
                    f,
                    "public step {step} has more than one primary private action"
                )
            }
            ProjectionError::StepWithoutPrimaryAction(step) => {
                write!(f, "public step {step} has no primary private action")
            }
            ProjectionError::SubActionNotReadOnly { action, effect } => write!(
                f,
                "transport sub-action {action} declares effect {} but sub-actions must be read-only",
                effect.as_str()
            ),
            ProjectionError::StepActionDigestMismatch {
                step,
                declared,
                computed,
            } => write!(
                f,
                "step {step} declares private action digest {declared} but its action hashes to {computed}"
            ),
            ProjectionError::PrivateEffectExceedsPublic {
                step,
                private_effect,
                public_effect,
            } => write!(
                f,
                "step {step}: private action effect {} exceeds public effect {}",
                private_effect.as_str(),
                public_effect.as_str()
            ),
            ProjectionError::PrivateTargetOutsidePublicTarget { step, action } => write!(
                f,
                "private action {action} targets something the public step {step} does not declare"
            ),
            ProjectionError::ContentDigestMismatch {
                step,
                public,
                private,
            } => write!(
                f,
                "step {step}: public content {public} does not match private slice {private}"
            ),
            ProjectionError::PrivateActionMissingContentDigest { step, action } => write!(
                f,
                "step {step} declares content but private action {action} does not bind a slice"
            ),
            ProjectionError::EffectfulStepWithoutTarget(step) => {
                write!(f, "effectful step {step} declares no semantic target")
            }
            ProjectionError::StepEffectNotDeclared { step, kind } => write!(
                f,
                "step {step} of kind {kind} has no matching entry in the plan EffectSet"
            ),
            ProjectionError::UnclaimedPersistentEffect(index) => write!(
                f,
                "persistent effect #{index} is declared but no public step produces it"
            ),
            ProjectionError::UnclaimedTransientEffect(index) => write!(
                f,
                "transient effect #{index} is declared but no public step produces it"
            ),
            ProjectionError::Step(error) => write!(f, "{error}"),
            ProjectionError::Cbor(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for ProjectionError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::sha256;
    use crate::effect::{DataImpact, DataImpactState};
    use crate::ids::PartitionId;
    use crate::step::{BindingRequirement, CancellationPolicy};

    struct Fixture {
        steps: Vec<PublicFlashStep>,
        plan: StoredProviderPlan,
        effects: EffectSet,
    }

    /// One write step with one primary action, wired consistently.
    fn fixture() -> Fixture {
        let content = sha256(b"system image");
        let range = ByteRange::new(245_760 * 512, 2_147_483_648).unwrap();
        let action = PrivateActionRecord {
            action_id: ActionId::new("ACT-001").unwrap(),
            step_id: StepId::new("STEP-001").unwrap(),
            role: PrivateActionRole::PrimaryEffect,
            effect_class: WorkflowEffect::Destructive,
            declared_target: Some(SemanticTarget::Partition(
                PartitionId::new("system").unwrap(),
            )),
            declared_range: Some(range),
            content_digest: Some(content),
            body: CborValue::map(vec![("action", CborValue::text("write-partition"))]),
        };
        let step = PublicFlashStep {
            step_id: StepId::new("STEP-001").unwrap(),
            kind: FlashStepKind::WriteTarget,
            effect: WorkflowEffect::Destructive,
            cancellation: CancellationPolicy::NonInterruptible,
            binding: BindingRequirement::ExactBoundTargetWithModeLineage,
            semantic_target: Some(SemanticTarget::Partition(
                PartitionId::new("system").unwrap(),
            )),
            content_digest: Some(content),
            expected_mode_before: None,
            expected_mode_after: None,
            private_action_digest: action.digest().unwrap(),
        };
        let effects = EffectSet {
            persistent: vec![PersistentEffect::WritePartition {
                partition: PartitionId::new("system").unwrap(),
                range,
                content,
            }],
            transient: vec![],
            data_impact: DataImpact {
                userdata: DataImpactState::Overwritten,
                calibration: DataImpactState::Preserved,
                non_volatile_config: DataImpactState::Preserved,
                secure_storage: DataImpactState::Preserved,
            },
        };
        Fixture {
            steps: vec![step],
            plan: StoredProviderPlan {
                actions: vec![action],
            },
            effects,
        }
    }

    #[test]
    fn a_consistent_projection_validates_and_is_deterministic() {
        let f = fixture();
        let first = validate_projection(&f.steps, &f.plan, &f.effects).unwrap();
        let second = validate_projection(&f.steps, &f.plan, &f.effects).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.per_action.len(), 1);
    }

    #[test]
    fn a_tampered_private_body_breaks_the_step_binding() {
        let mut f = fixture();
        f.plan.actions[0].body = CborValue::map(vec![("tool", CborValue::text("wl"))]);
        assert!(matches!(
            validate_projection(&f.steps, &f.plan, &f.effects),
            Err(ProjectionError::StepActionDigestMismatch { .. })
        ));
    }

    #[test]
    fn a_private_action_may_not_target_a_partition_the_step_did_not_declare() {
        let mut f = fixture();
        f.plan.actions[0].declared_target = Some(SemanticTarget::Partition(
            PartitionId::new("userdata").unwrap(),
        ));
        f.steps[0].private_action_digest = f.plan.actions[0].digest().unwrap();
        assert!(matches!(
            validate_projection(&f.steps, &f.plan, &f.effects),
            Err(ProjectionError::PrivateTargetOutsidePublicTarget { .. })
        ));
    }

    #[test]
    fn a_private_range_may_not_exceed_the_declared_effect_range() {
        let mut f = fixture();
        let effect_range = match &f.effects.persistent[0] {
            PersistentEffect::WritePartition { range, .. } => *range,
            other => panic!("unexpected effect {other:?}"),
        };
        f.plan.actions[0].declared_range =
            Some(ByteRange::new(effect_range.start, effect_range.length + 512).unwrap());
        f.steps[0].private_action_digest = f.plan.actions[0].digest().unwrap();
        assert!(matches!(
            validate_projection(&f.steps, &f.plan, &f.effects),
            Err(ProjectionError::StepEffectNotDeclared { .. })
        ));
    }

    #[test]
    fn a_declared_effect_no_step_produces_is_rejected() {
        let mut f = fixture();
        f.effects.persistent.push(PersistentEffect::ErasePartition {
            partition: PartitionId::new("userdata").unwrap(),
            range: ByteRange::new(0, 512).unwrap(),
        });
        assert!(matches!(
            validate_projection(&f.steps, &f.plan, &f.effects),
            Err(ProjectionError::UnclaimedPersistentEffect(1))
        ));
    }

    #[test]
    fn a_destructive_step_with_no_declared_effect_is_rejected() {
        let mut f = fixture();
        f.effects.persistent.clear();
        assert!(matches!(
            validate_projection(&f.steps, &f.plan, &f.effects),
            Err(ProjectionError::StepEffectNotDeclared { .. })
        ));
    }

    #[test]
    fn a_sub_action_may_not_carry_an_effect() {
        let mut f = fixture();
        f.plan.actions.push(PrivateActionRecord {
            action_id: ActionId::new("ACT-002").unwrap(),
            step_id: StepId::new("STEP-001").unwrap(),
            role: PrivateActionRole::ReadOnlyTransportSubAction,
            effect_class: WorkflowEffect::Destructive,
            declared_target: None,
            declared_range: None,
            content_digest: None,
            body: CborValue::map(vec![(
                "action",
                CborValue::text("characterize-read-domain"),
            )]),
        });
        assert!(matches!(
            validate_projection(&f.steps, &f.plan, &f.effects),
            Err(ProjectionError::SubActionNotReadOnly { .. })
        ));
    }

    #[test]
    fn a_read_only_sub_action_joins_the_provider_plan_digest() {
        let mut f = fixture();
        let without_sub_action = validate_projection(&f.steps, &f.plan, &f.effects).unwrap();
        f.plan.actions.push(PrivateActionRecord {
            action_id: ActionId::new("ACT-002").unwrap(),
            step_id: StepId::new("STEP-001").unwrap(),
            role: PrivateActionRole::ReadOnlyTransportSubAction,
            effect_class: WorkflowEffect::ReadOnly,
            declared_target: None,
            declared_range: None,
            content_digest: None,
            body: CborValue::map(vec![(
                "action",
                CborValue::text("characterize-read-domain"),
            )]),
        });
        let with_sub_action = validate_projection(&f.steps, &f.plan, &f.effects).unwrap();
        assert_eq!(with_sub_action.per_action.len(), 2);
        assert_ne!(
            with_sub_action.provider_execution_plan_digest,
            without_sub_action.provider_execution_plan_digest
        );
    }

    #[test]
    fn a_step_with_no_private_action_is_rejected() {
        let mut f = fixture();
        f.plan.actions.clear();
        assert!(matches!(
            validate_projection(&f.steps, &f.plan, &f.effects),
            Err(ProjectionError::StepWithoutPrimaryAction(_))
        ));
    }

    #[test]
    fn an_orphan_private_action_is_rejected() {
        let mut f = fixture();
        f.plan.actions.push(PrivateActionRecord {
            action_id: ActionId::new("ACT-009").unwrap(),
            step_id: StepId::new("STEP-404").unwrap(),
            role: PrivateActionRole::PrimaryEffect,
            effect_class: WorkflowEffect::Destructive,
            declared_target: None,
            declared_range: None,
            content_digest: None,
            body: CborValue::Null,
        });
        assert!(matches!(
            validate_projection(&f.steps, &f.plan, &f.effects),
            Err(ProjectionError::ActionWithoutPublicStep { .. })
        ));
    }
}
