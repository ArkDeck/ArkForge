//! Public step vocabulary.
//!
//! architecture.md 5.4. `FlashStepKind` belongs to Core and stays device- and
//! authority-neutral; compatibility with ArkDeck's WorkflowStep registry is the
//! adapter's published mapping table, never a Core dependency.

use crate::digest::{CanonicalCbor, CborError, CborValue, Domain, Sha256Digest, digest_canonical};
use crate::effect::{BootMetadataField, DeviceMode};
use crate::ids::{PartitionId, RegionId, StepId};
use core::fmt;

/// The closed set of public step kinds.
///
/// A provider that needs a step this list cannot express does not get a new
/// free-text kind — the vocabulary is extended here, under review, so ArkDeck
/// can keep mapping every kind to a published registry entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FlashStepKind {
    /// Bring the device into a required semantic mode.
    EnsureMode,
    /// Read identity and capability evidence.
    ProbeDevice,
    /// Compare the device's own layout against the plan's assumptions.
    ValidateLayout,
    /// Load a loader/FDL/boot agent into volatile memory.
    LoadEphemeralAgent,
    /// Erase a semantic target.
    EraseTarget,
    /// Write content to a semantic target.
    WriteTarget,
    /// Verify a previously written target.
    VerifyTarget,
    /// Wait for the device to re-enumerate into an expected identity.
    AwaitRebind,
    /// Reboot into a target mode.
    Reboot,
    /// Re-adopt the device after reboot and confirm build/model facts.
    PostflightProbe,
}

impl FlashStepKind {
    pub const ALL: [FlashStepKind; 10] = [
        FlashStepKind::EnsureMode,
        FlashStepKind::ProbeDevice,
        FlashStepKind::ValidateLayout,
        FlashStepKind::LoadEphemeralAgent,
        FlashStepKind::EraseTarget,
        FlashStepKind::WriteTarget,
        FlashStepKind::VerifyTarget,
        FlashStepKind::AwaitRebind,
        FlashStepKind::Reboot,
        FlashStepKind::PostflightProbe,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            FlashStepKind::EnsureMode => "ensureMode",
            FlashStepKind::ProbeDevice => "probeDevice",
            FlashStepKind::ValidateLayout => "validateLayout",
            FlashStepKind::LoadEphemeralAgent => "loadEphemeralAgent",
            FlashStepKind::EraseTarget => "eraseTarget",
            FlashStepKind::WriteTarget => "writeTarget",
            FlashStepKind::VerifyTarget => "verifyTarget",
            FlashStepKind::AwaitRebind => "awaitRebind",
            FlashStepKind::Reboot => "reboot",
            FlashStepKind::PostflightProbe => "postflightProbe",
        }
    }

    /// Unknown kinds fail closed rather than degrading to a default
    /// (architecture.md 15.2).
    pub fn parse(value: &str) -> Option<Self> {
        FlashStepKind::ALL
            .into_iter()
            .find(|kind| kind.as_str() == value)
    }

    /// The lowest effect class this kind can legitimately declare. A step that
    /// claims *less* than this is rejected; claiming more is allowed, because
    /// over-declaring an effect only tightens admission.
    pub fn minimum_effect(self) -> WorkflowEffect {
        match self {
            FlashStepKind::ProbeDevice
            | FlashStepKind::ValidateLayout
            | FlashStepKind::VerifyTarget
            | FlashStepKind::AwaitRebind
            | FlashStepKind::PostflightProbe => WorkflowEffect::ReadOnly,
            FlashStepKind::EnsureMode
            | FlashStepKind::LoadEphemeralAgent
            | FlashStepKind::Reboot => WorkflowEffect::Transient,
            FlashStepKind::EraseTarget | FlashStepKind::WriteTarget => WorkflowEffect::Destructive,
        }
    }
}

impl fmt::Display for FlashStepKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl CanonicalCbor for FlashStepKind {
    fn to_cbor(&self) -> CborValue {
        CborValue::text(self.as_str())
    }
}

/// How much a step is allowed to affect the device.
///
/// `Ord` is severity order, so "not below the registry minimum" is a `>=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WorkflowEffect {
    ReadOnly,
    Transient,
    Mutating,
    Destructive,
}

impl WorkflowEffect {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkflowEffect::ReadOnly => "readOnly",
            WorkflowEffect::Transient => "transient",
            WorkflowEffect::Mutating => "mutating",
            WorkflowEffect::Destructive => "destructive",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "readOnly" => Some(WorkflowEffect::ReadOnly),
            "transient" => Some(WorkflowEffect::Transient),
            "mutating" => Some(WorkflowEffect::Mutating),
            "destructive" => Some(WorkflowEffect::Destructive),
            _ => None,
        }
    }

    /// A step at or above `Mutating` needs an exact StepPermit before dispatch
    /// (architecture.md 25.9).
    pub fn requires_permit(self) -> bool {
        self >= WorkflowEffect::Mutating
    }
}

impl CanonicalCbor for WorkflowEffect {
    fn to_cbor(&self) -> CborValue {
        CborValue::text(self.as_str())
    }
}

/// How a step behaves when cancellation arrives.
///
/// `Ord` is cancellation *strength*, so "not weaker than the registry" is a
/// `>=`: `NonInterruptible` is the weakest guarantee to the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CancellationPolicy {
    /// Cannot be interrupted once dispatched; cancellation queues to the next
    /// safe boundary (architecture.md 13.4).
    NonInterruptible,
    /// Cancellation takes effect at a declared boundary inside the step.
    CancellableAtBoundary,
    /// Cancellation takes effect immediately with no external effect.
    CancellableImmediately,
}

impl CancellationPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            CancellationPolicy::NonInterruptible => "nonInterruptible",
            CancellationPolicy::CancellableAtBoundary => "cancellableAtBoundary",
            CancellationPolicy::CancellableImmediately => "cancellableImmediately",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "nonInterruptible" => Some(CancellationPolicy::NonInterruptible),
            "cancellableAtBoundary" => Some(CancellationPolicy::CancellableAtBoundary),
            "cancellableImmediately" => Some(CancellationPolicy::CancellableImmediately),
            _ => None,
        }
    }
}

impl CanonicalCbor for CancellationPolicy {
    fn to_cbor(&self) -> CborValue {
        CborValue::text(self.as_str())
    }
}

/// How tightly a step is bound to the authority's target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BindingRequirement {
    /// The exact bound target must be observed.
    ExactBoundTarget,
    /// The exact bound target must be observed *and* its lineage across the
    /// mode transition proven.
    ExactBoundTargetWithModeLineage,
}

impl BindingRequirement {
    pub fn as_str(self) -> &'static str {
        match self {
            BindingRequirement::ExactBoundTarget => "exactBoundTarget",
            BindingRequirement::ExactBoundTargetWithModeLineage => {
                "exactBoundTargetWithModeLineage"
            }
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "exactBoundTarget" => Some(BindingRequirement::ExactBoundTarget),
            "exactBoundTargetWithModeLineage" => {
                Some(BindingRequirement::ExactBoundTargetWithModeLineage)
            }
            _ => None,
        }
    }
}

impl CanonicalCbor for BindingRequirement {
    fn to_cbor(&self) -> CborValue {
        CborValue::text(self.as_str())
    }
}

/// What a step acts on, in semantic terms. Never an address.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SemanticTarget {
    Partition(PartitionId),
    RawRegion(RegionId),
    BootMetadata(BootMetadataField),
    /// The device as a whole (mode transitions, reboot, probe).
    Device,
}

impl CanonicalCbor for SemanticTarget {
    fn to_cbor(&self) -> CborValue {
        match self {
            SemanticTarget::Partition(id) => CborValue::map(vec![("partition", id.to_cbor())]),
            SemanticTarget::RawRegion(id) => CborValue::map(vec![("rawRegion", id.to_cbor())]),
            SemanticTarget::BootMetadata(field) => {
                CborValue::map(vec![("bootMetadata", CborValue::text(field.as_str()))])
            }
            SemanticTarget::Device => CborValue::map(vec![("device", CborValue::Null)]),
        }
    }
}

/// One ordered, externally visible step of a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicFlashStep {
    pub step_id: StepId,
    pub kind: FlashStepKind,
    pub effect: WorkflowEffect,
    pub cancellation: CancellationPolicy,
    pub binding: BindingRequirement,
    pub semantic_target: Option<SemanticTarget>,
    pub content_digest: Option<Sha256Digest>,
    pub expected_mode_before: Option<DeviceMode>,
    pub expected_mode_after: Option<DeviceMode>,
    /// Binds this public step to the exact private action that implements it.
    pub private_action_digest: Sha256Digest,
}

impl PublicFlashStep {
    pub fn digest(&self) -> Result<Sha256Digest, CborError> {
        digest_canonical(Domain::PublicStep, self)
    }

    /// A step may not claim a lower effect class than its kind implies.
    pub fn validate_self_consistent(&self) -> Result<(), StepError> {
        let minimum = self.kind.minimum_effect();
        if self.effect < minimum {
            return Err(StepError::EffectBelowKindMinimum {
                step: self.step_id.to_string(),
                kind: self.kind,
                declared: self.effect,
                minimum,
            });
        }
        if matches!(self.kind, FlashStepKind::WriteTarget) && self.content_digest.is_none() {
            return Err(StepError::WriteWithoutContentDigest(
                self.step_id.to_string(),
            ));
        }
        if matches!(
            self.kind,
            FlashStepKind::WriteTarget | FlashStepKind::EraseTarget | FlashStepKind::VerifyTarget
        ) && !matches!(
            self.semantic_target,
            Some(SemanticTarget::Partition(_)) | Some(SemanticTarget::RawRegion(_))
        ) {
            return Err(StepError::TargetedStepWithoutTarget {
                step: self.step_id.to_string(),
                kind: self.kind,
            });
        }
        Ok(())
    }
}

impl CanonicalCbor for PublicFlashStep {
    fn to_cbor(&self) -> CborValue {
        let mut entries = vec![
            ("stepId", self.step_id.to_cbor()),
            ("kind", self.kind.to_cbor()),
            ("effect", self.effect.to_cbor()),
            ("cancellation", self.cancellation.to_cbor()),
            ("binding", self.binding.to_cbor()),
            ("privateActionDigest", self.private_action_digest.to_cbor()),
        ];
        entries.push((
            "semanticTarget",
            match &self.semantic_target {
                Some(target) => target.to_cbor(),
                None => CborValue::Null,
            },
        ));
        entries.push((
            "contentDigest",
            match &self.content_digest {
                Some(digest) => digest.to_cbor(),
                None => CborValue::Null,
            },
        ));
        entries.push((
            "expectedModeBefore",
            match &self.expected_mode_before {
                Some(mode) => mode.to_cbor(),
                None => CborValue::Null,
            },
        ));
        entries.push((
            "expectedModeAfter",
            match &self.expected_mode_after {
                Some(mode) => mode.to_cbor(),
                None => CborValue::Null,
            },
        ));
        CborValue::map(entries)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepError {
    EffectBelowKindMinimum {
        step: String,
        kind: FlashStepKind,
        declared: WorkflowEffect,
        minimum: WorkflowEffect,
    },
    WriteWithoutContentDigest(String),
    TargetedStepWithoutTarget {
        step: String,
        kind: FlashStepKind,
    },
}

impl fmt::Display for StepError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StepError::EffectBelowKindMinimum {
                step,
                kind,
                declared,
                minimum,
            } => write!(
                f,
                "step {step} of kind {kind} declares effect {} below the kind minimum {}",
                declared.as_str(),
                minimum.as_str()
            ),
            StepError::WriteWithoutContentDigest(step) => {
                write!(f, "write step {step} has no content digest")
            }
            StepError::TargetedStepWithoutTarget { step, kind } => {
                write!(
                    f,
                    "step {step} of kind {kind} needs a partition or region target"
                )
            }
        }
    }
}

impl std::error::Error for StepError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::sha256;

    fn step(kind: FlashStepKind, effect: WorkflowEffect) -> PublicFlashStep {
        PublicFlashStep {
            step_id: StepId::new("STEP-1").unwrap(),
            kind,
            effect,
            cancellation: CancellationPolicy::CancellableImmediately,
            binding: BindingRequirement::ExactBoundTarget,
            semantic_target: Some(SemanticTarget::Partition(
                PartitionId::new("system").unwrap(),
            )),
            content_digest: Some(sha256(b"image")),
            expected_mode_before: None,
            expected_mode_after: None,
            private_action_digest: sha256(b"action"),
        }
    }

    #[test]
    fn write_step_cannot_declare_itself_read_only() {
        let step = step(FlashStepKind::WriteTarget, WorkflowEffect::ReadOnly);
        assert!(matches!(
            step.validate_self_consistent(),
            Err(StepError::EffectBelowKindMinimum { .. })
        ));
    }

    #[test]
    fn over_declaring_effect_is_allowed() {
        let step = step(FlashStepKind::ProbeDevice, WorkflowEffect::Destructive);
        assert!(step.validate_self_consistent().is_ok());
    }

    #[test]
    fn write_step_requires_content_digest_and_target() {
        let mut step = step(FlashStepKind::WriteTarget, WorkflowEffect::Destructive);
        step.content_digest = None;
        assert!(matches!(
            step.validate_self_consistent(),
            Err(StepError::WriteWithoutContentDigest(_))
        ));

        let mut step = step.clone();
        step.content_digest = Some(sha256(b"image"));
        step.semantic_target = Some(SemanticTarget::Device);
        assert!(matches!(
            step.validate_self_consistent(),
            Err(StepError::TargetedStepWithoutTarget { .. })
        ));
    }

    #[test]
    fn unknown_step_kind_fails_closed() {
        assert_eq!(
            FlashStepKind::parse("writeTarget"),
            Some(FlashStepKind::WriteTarget)
        );
        assert_eq!(FlashStepKind::parse("flashEverything"), None);
        assert_eq!(FlashStepKind::parse("WriteTarget"), None);
    }

    #[test]
    fn effect_severity_orders_permit_requirement() {
        assert!(!WorkflowEffect::ReadOnly.requires_permit());
        assert!(!WorkflowEffect::Transient.requires_permit());
        assert!(WorkflowEffect::Mutating.requires_permit());
        assert!(WorkflowEffect::Destructive.requires_permit());
    }

    #[test]
    fn every_kind_round_trips_through_its_wire_name() {
        for kind in FlashStepKind::ALL {
            assert_eq!(FlashStepKind::parse(kind.as_str()), Some(kind));
        }
    }
}
