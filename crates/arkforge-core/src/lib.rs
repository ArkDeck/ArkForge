//! # arkforge-core
//!
//! The device- and authority-neutral heart of ArkForge.
//!
//! What lives here is everything both sides of the authority boundary must
//! agree on: identifiers, digests, the public step vocabulary, the effect
//! model, plan sealing, and the public/private projection invariant.
//!
//! What may never live here (architecture.md 4.3):
//!
//! - ArkDeck types, or any authority's business semantics;
//! - device names — dayu200, dayu600;
//! - vendor names — Rockchip, Unisoc;
//! - firmware formats — PAC, FDL, RockUSB;
//! - external flashing CLI implementations;
//! - UI or platform frameworks.
//!
//! The rule is enforced by a dependency-and-symbol guard test rather than by a
//! substring scan (architecture.md 4.3); see `tests/architecture_guard.rs` in
//! the workspace root crate `arkforge-arkdeck-adapter`'s sibling test suite.

#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

pub mod authority;
pub mod digest;
pub mod effect;
pub mod identity;
pub mod ids;
pub mod outcome;
pub mod plan;
pub mod profile;
pub mod projection;
pub mod step;
pub mod verification;
pub mod yaml;

pub use authority::{
    AuthorityBindingRef, AuthorityNamespace, AuthoritySupportBinding, AuthoritySupportState,
};
pub use digest::{
    CanonicalCbor, CborError, CborValue, Domain, Sha256, Sha256Digest, digest_canonical,
    digest_in_domain, digest_ordered,
};
pub use effect::{
    AgentStage, BootMetadataField, ByteRange, DataImpact, DataImpactState, DeviceMode, EffectError,
    EffectSet, MemoryRegion, PersistentEffect, TransientEffect, TypedValue,
};
pub use identity::{
    ArtifactFormat, ArtifactIdentity, DeviceProfileIdentity, HostPlatform, MaturityKey,
    MaturityState, NegotiatedCapabilities, ProviderIdentity, ToolchainIdentity, ToolchainKind,
    Version,
};
pub use ids::{
    ActionId, ArtifactId, AttemptId, ControllerSessionId, EvidenceId, IdError, JobId,
    ObservationId, OpaqueId, PartitionId, PermitId, PlanId, RegionId, RequestId, StepId,
};
pub use plan::{
    EvidenceRequirement, ExecutionAvailability, ExecutionPurpose, ExecutionUnknown,
    FlashPlanEnvelope, PlanAssessment, PlanError, PlanMaterialization, PlanSchemaVersion,
    PlanSealInput, PostflightPolicy, ProfileCandidate, ProviderCandidate, RecoveryContractRef,
};
pub use profile::{
    AllowedTarget, DeviceProfile, HardwareRevisionPolicy, IdentityFieldPolicy, ModeDeclaration,
    ModeTransition, ProfileError, ProfileExecutionBlocker, ProviderCombination, ReadDomainPolicy,
    RebindTolerance, RecoveryDeclaration, SocIdentity, StorageDeclaration, WriteDomainDeclaration,
    load as load_profile,
};
pub use projection::{
    ActionDigestBinding, PrivateActionRecord, PrivateActionRole, ProjectionDigests,
    ProjectionError, StoredProviderPlan, validate_projection,
};
pub use step::{
    BindingRequirement, CancellationPolicy, FlashStepKind, PublicFlashStep, SemanticTarget,
    StepError, WorkflowEffect,
};
pub use verification::{
    FailureClassification, MeasuredReadDomain, ReadDomainDeclaration, ReadbackObservation,
    TargetVerificationDeclaration, TypedSkipReason, VerificationError, VerificationFallback,
    VerificationOutcome, VerificationStrength, classify_verification,
};

/// The wire/schema version of this Core.
pub const CORE_SCHEMA_VERSION: u32 = 1;
