//! # arkforge-arkdeck-adapter
//!
//! The published `FlashStepKind ↔ WorkflowStep kind` mapping table.
//!
//! architecture.md 5.4 puts this here rather than in Core on purpose: Core's
//! step vocabulary is authority-neutral, and compatibility with ArkDeck's
//! registry is a versioned, reviewed adapter artifact. A second authority would
//! bring its own table without touching Core.
//!
//! [`control`] carries the second published table: which ArkDeck provider
//! actions each semantic [`ManagedDeviceControlAction`] binds to, and which of
//! ArkDeck's Rockchip actions move to ArkForge. Still outstanding for AF-V2:
//! the ExecutionAuthority implementation and the IPC client, both of which are
//! written against these two tables.
//!
//! [`ManagedDeviceControlAction`]: arkforge_authority_api::ManagedDeviceControlAction
//!
//! Values are read from the ArkDeck audit baseline
//! (`Packages/ArkDeckKit/Sources/ArkDeckCore/WorkflowStep.swift`,
//! `WorkflowStepMetadata`). They are pinned data, not policy this crate relaxes.

#![forbid(unsafe_code)]

pub mod control;

use arkforge_core::step::{
    BindingRequirement, CancellationPolicy, FlashStepKind, PublicFlashStep, WorkflowEffect,
};
use core::fmt;

/// The adapter's own version. It changes when this table changes, because the
/// table is what ArkDeck admission depends on.
pub const MAPPING_TABLE_VERSION: &str = "arkforge.arkdeck-step-map/v1";

/// The ArkDeck audit baseline this table was read from.
pub const ARKDECK_BASELINE: &str = "2849c5c188717ac351f9228a9cd60c054035fbcf";

/// How an ArkForge step kind reaches ArkDeck's registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mapping {
    /// A published WorkflowStep kind, with the registry floors it carries.
    Published(RegistryEntry),
    /// ArkDeck's registry has no counterpart. A plan containing this kind
    /// cannot be admitted until a registry entry is published and reviewed —
    /// which is the fail-closed answer, not a blocker to work around.
    Unmapped { reason: &'static str },
}

/// A published ArkDeck WorkflowStep kind and its floors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryEntry {
    /// The ArkDeck `WorkflowStepKind` raw value.
    pub workflow_step_kind: &'static str,
    /// ArkDeck's `minimumEffect`, expressed in ArkForge's ladder.
    ///
    /// The two ladders are not identical: ArkDeck has no separate "transient"
    /// band, so its `deviceMutation` floor lands on `Mutating` here. A step
    /// that declares only `Transient` therefore under-declares against the
    /// registry and is refused — which is why the Rockchip provider declares
    /// its mode changes as `Mutating`.
    pub minimum_effect: WorkflowEffect,
    /// ArkDeck's `minimumCancellation`, in ArkForge's strength ladder. A step
    /// must be at least this cancellable.
    pub minimum_cancellation: CancellationPolicy,
    /// ArkDeck's `minimumBindingRequirement`.
    pub minimum_binding: BindingRequirement,
}

/// The table. Every [`FlashStepKind`] appears exactly once.
pub fn mapping(kind: FlashStepKind) -> Mapping {
    match kind {
        FlashStepKind::EnsureMode => Mapping::Published(RegistryEntry {
            workflow_step_kind: "enterUpdater",
            minimum_effect: WorkflowEffect::Mutating,
            minimum_cancellation: CancellationPolicy::CancellableAtBoundary,
            minimum_binding: BindingRequirement::ExactBoundTarget,
        }),
        FlashStepKind::ProbeDevice => Mapping::Published(RegistryEntry {
            workflow_step_kind: "probeDevice",
            minimum_effect: WorkflowEffect::ReadOnly,
            minimum_cancellation: CancellationPolicy::CancellableImmediately,
            minimum_binding: BindingRequirement::ExactBoundTarget,
        }),
        FlashStepKind::ValidateLayout => Mapping::Published(RegistryEntry {
            workflow_step_kind: "verifyRemoteState",
            minimum_effect: WorkflowEffect::ReadOnly,
            minimum_cancellation: CancellationPolicy::CancellableImmediately,
            minimum_binding: BindingRequirement::ExactBoundTarget,
        }),
        FlashStepKind::EraseTarget => Mapping::Published(RegistryEntry {
            workflow_step_kind: "erasePartition",
            minimum_effect: WorkflowEffect::Destructive,
            minimum_cancellation: CancellationPolicy::NonInterruptible,
            minimum_binding: BindingRequirement::ExactBoundTarget,
        }),
        FlashStepKind::WriteTarget => Mapping::Published(RegistryEntry {
            workflow_step_kind: "flashPartition",
            minimum_effect: WorkflowEffect::Destructive,
            minimum_cancellation: CancellationPolicy::NonInterruptible,
            minimum_binding: BindingRequirement::ExactBoundTarget,
        }),
        FlashStepKind::VerifyTarget => Mapping::Published(RegistryEntry {
            workflow_step_kind: "verifyRemoteState",
            minimum_effect: WorkflowEffect::ReadOnly,
            minimum_cancellation: CancellationPolicy::CancellableImmediately,
            minimum_binding: BindingRequirement::ExactBoundTarget,
        }),
        FlashStepKind::AwaitRebind => Mapping::Published(RegistryEntry {
            workflow_step_kind: "waitForReconnect",
            minimum_effect: WorkflowEffect::ReadOnly,
            minimum_cancellation: CancellationPolicy::CancellableImmediately,
            minimum_binding: BindingRequirement::ExactBoundTarget,
        }),
        FlashStepKind::Reboot => Mapping::Published(RegistryEntry {
            workflow_step_kind: "rebootDevice",
            minimum_effect: WorkflowEffect::Mutating,
            minimum_cancellation: CancellationPolicy::CancellableAtBoundary,
            minimum_binding: BindingRequirement::ExactBoundTarget,
        }),
        FlashStepKind::PostflightProbe => Mapping::Published(RegistryEntry {
            workflow_step_kind: "verifyRemoteState",
            minimum_effect: WorkflowEffect::ReadOnly,
            minimum_cancellation: CancellationPolicy::CancellableImmediately,
            minimum_binding: BindingRequirement::ExactBoundTarget,
        }),
        FlashStepKind::LoadEphemeralAgent => Mapping::Unmapped {
            reason: "ArkDeck's registry has no WorkflowStep kind for loading a boot agent into \
                     volatile device memory. The DAYU200 vertical does not need one — the loader \
                     enters through enterUpdater — and a Unisoc FDL stage would need a reviewed \
                     registry entry before any plan containing it could be admitted",
        },
    }
}

/// Why a step cannot be admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionRefusal {
    UnmappedKind {
        step: String,
        kind: FlashStepKind,
        reason: &'static str,
    },
    EffectBelowRegistryMinimum {
        step: String,
        workflow_step_kind: &'static str,
        declared: WorkflowEffect,
        minimum: WorkflowEffect,
    },
    CancellationWeakerThanRegistry {
        step: String,
        workflow_step_kind: &'static str,
        declared: CancellationPolicy,
        minimum: CancellationPolicy,
    },
    BindingWeakerThanRegistry {
        step: String,
        workflow_step_kind: &'static str,
        declared: BindingRequirement,
        minimum: BindingRequirement,
    },
}

impl fmt::Display for AdmissionRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AdmissionRefusal::UnmappedKind { step, kind, reason } => write!(
                f,
                "step {step} of kind {kind} has no published ArkDeck registry entry: {reason}"
            ),
            AdmissionRefusal::EffectBelowRegistryMinimum {
                step,
                workflow_step_kind,
                declared,
                minimum,
            } => write!(
                f,
                "step {step} declares effect {} but {workflow_step_kind} requires at least {}",
                declared.as_str(),
                minimum.as_str()
            ),
            AdmissionRefusal::CancellationWeakerThanRegistry {
                step,
                workflow_step_kind,
                declared,
                minimum,
            } => write!(
                f,
                "step {step} declares cancellation {} but {workflow_step_kind} requires at least {}",
                declared.as_str(),
                minimum.as_str()
            ),
            AdmissionRefusal::BindingWeakerThanRegistry {
                step,
                workflow_step_kind,
                declared,
                minimum,
            } => write!(
                f,
                "step {step} declares binding {} but {workflow_step_kind} requires at least {}",
                declared.as_str(),
                minimum.as_str()
            ),
        }
    }
}

impl std::error::Error for AdmissionRefusal {}

/// Checks one public step against the registry entry its kind maps to.
///
/// This is the ArkForge-side pre-check. ArkDeck Runtime still performs its own
/// admission — this crate cannot admit anything (architecture.md 3.1). What it
/// prevents is materializing a plan that the authority is certain to refuse.
pub fn check_step(step: &PublicFlashStep) -> Result<&'static str, AdmissionRefusal> {
    let entry = match mapping(step.kind) {
        Mapping::Published(entry) => entry,
        Mapping::Unmapped { reason } => {
            return Err(AdmissionRefusal::UnmappedKind {
                step: step.step_id.to_string(),
                kind: step.kind,
                reason,
            })
        }
    };

    if step.effect < entry.minimum_effect {
        return Err(AdmissionRefusal::EffectBelowRegistryMinimum {
            step: step.step_id.to_string(),
            workflow_step_kind: entry.workflow_step_kind,
            declared: step.effect,
            minimum: entry.minimum_effect,
        });
    }
    if step.cancellation < entry.minimum_cancellation {
        return Err(AdmissionRefusal::CancellationWeakerThanRegistry {
            step: step.step_id.to_string(),
            workflow_step_kind: entry.workflow_step_kind,
            declared: step.cancellation,
            minimum: entry.minimum_cancellation,
        });
    }
    if step.binding < entry.minimum_binding {
        return Err(AdmissionRefusal::BindingWeakerThanRegistry {
            step: step.step_id.to_string(),
            workflow_step_kind: entry.workflow_step_kind,
            declared: step.binding,
            minimum: entry.minimum_binding,
        });
    }
    Ok(entry.workflow_step_kind)
}

/// Checks every step of a plan.
pub fn check_plan(steps: &[PublicFlashStep]) -> Result<Vec<&'static str>, AdmissionRefusal> {
    steps.iter().map(check_step).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use arkforge_core::digest::sha256;
    use arkforge_core::ids::{PartitionId, StepId};
    use arkforge_core::step::SemanticTarget;

    fn step(kind: FlashStepKind, effect: WorkflowEffect, cancellation: CancellationPolicy) -> PublicFlashStep {
        PublicFlashStep {
            step_id: StepId::new("STEP-001").unwrap(),
            kind,
            effect,
            cancellation,
            binding: BindingRequirement::ExactBoundTargetWithModeLineage,
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
    fn every_step_kind_has_exactly_one_table_entry() {
        // The table is total: a new kind added to Core without a decision here
        // fails to compile the match, which is the point of a closed enum.
        for kind in FlashStepKind::ALL {
            let _ = mapping(kind);
        }
    }

    #[test]
    fn a_write_maps_to_flash_partition() {
        let step = step(
            FlashStepKind::WriteTarget,
            WorkflowEffect::Destructive,
            CancellationPolicy::NonInterruptible,
        );
        assert_eq!(check_step(&step).unwrap(), "flashPartition");
    }

    #[test]
    fn a_write_that_under_declares_its_effect_is_refused() {
        let step = step(
            FlashStepKind::WriteTarget,
            WorkflowEffect::Mutating,
            CancellationPolicy::NonInterruptible,
        );
        assert!(matches!(
            check_step(&step),
            Err(AdmissionRefusal::EffectBelowRegistryMinimum { .. })
        ));
    }

    #[test]
    fn a_read_step_that_is_not_immediately_cancellable_is_refused() {
        let step = step(
            FlashStepKind::ProbeDevice,
            WorkflowEffect::ReadOnly,
            CancellationPolicy::NonInterruptible,
        );
        assert!(matches!(
            check_step(&step),
            Err(AdmissionRefusal::CancellationWeakerThanRegistry { .. })
        ));
    }

    #[test]
    fn a_mode_change_that_declares_only_transient_is_refused() {
        // ArkDeck's ladder has no transient band: `enterUpdater` floors at
        // deviceMutation. This is the case the Rockchip provider's comment
        // points at.
        let step = step(
            FlashStepKind::EnsureMode,
            WorkflowEffect::Transient,
            CancellationPolicy::CancellableAtBoundary,
        );
        assert!(matches!(
            check_step(&step),
            Err(AdmissionRefusal::EffectBelowRegistryMinimum { .. })
        ));

        let admissible = step2(
            FlashStepKind::EnsureMode,
            WorkflowEffect::Mutating,
            CancellationPolicy::CancellableAtBoundary,
        );
        assert_eq!(check_step(&admissible).unwrap(), "enterUpdater");
    }

    fn step2(
        kind: FlashStepKind,
        effect: WorkflowEffect,
        cancellation: CancellationPolicy,
    ) -> PublicFlashStep {
        let mut step = step(kind, effect, cancellation);
        step.semantic_target = Some(SemanticTarget::Device);
        step.content_digest = None;
        step
    }

    #[test]
    fn an_unmapped_kind_is_refused_with_its_reason() {
        let step = step2(
            FlashStepKind::LoadEphemeralAgent,
            WorkflowEffect::Destructive,
            CancellationPolicy::NonInterruptible,
        );
        match check_step(&step) {
            Err(AdmissionRefusal::UnmappedKind { reason, .. }) => {
                assert!(reason.contains("registry entry"), "{reason}");
            }
            other => panic!("expected an unmapped refusal, got {other:?}"),
        }
    }

    #[test]
    fn over_declaring_is_allowed_because_it_only_tightens_admission() {
        let step = step(
            FlashStepKind::VerifyTarget,
            WorkflowEffect::Destructive,
            CancellationPolicy::CancellableImmediately,
        );
        assert_eq!(check_step(&step).unwrap(), "verifyRemoteState");
    }
}
