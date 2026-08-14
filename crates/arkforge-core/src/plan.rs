//! Plan materialization: an executable envelope, or an assessment that says
//! exactly what is missing.
//!
//! architecture.md 5.2 / 5.3. The separation is the whole point: incomplete
//! evidence must produce something displayable and exportable that *cannot* be
//! handed to `startExecution`, rather than an executable plan with a warning.

use crate::authority::AuthorityBindingRef;
use crate::digest::{digest_in_domain, CanonicalCbor, CborError, CborValue, Domain, Sha256Digest};
use crate::effect::EffectSet;
use crate::identity::{
    ArtifactIdentity, DeviceProfileIdentity, MaturityState, NegotiatedCapabilities,
    ProviderIdentity, ToolchainIdentity,
};
use crate::ids::{EvidenceId, OpaqueId, PlanId};
use crate::projection::{ActionDigestBinding, ProjectionDigests};
use crate::step::PublicFlashStep;
use core::fmt;

/// The plan schema version. A change here invalidates stored plans rather than
/// letting a new daemon reinterpret an old private plan (architecture.md 6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlanSchemaVersion(pub u32);

impl PlanSchemaVersion {
    pub const CURRENT: PlanSchemaVersion = PlanSchemaVersion(1);
}

impl CanonicalCbor for PlanSchemaVersion {
    fn to_cbor(&self) -> CborValue {
        CborValue::Unsigned(self.0 as u64)
    }
}

/// Why a plan exists. A superseding recovery plan is a distinct plan with a
/// distinct purpose, never a retry of the original (architecture.md 14.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExecutionPurpose {
    PrimaryFlash,
    SupersedingRecovery,
}

impl ExecutionPurpose {
    pub fn as_str(self) -> &'static str {
        match self {
            ExecutionPurpose::PrimaryFlash => "primaryFlash",
            ExecutionPurpose::SupersedingRecovery => "supersedingRecovery",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "primaryFlash" => Some(ExecutionPurpose::PrimaryFlash),
            "supersedingRecovery" => Some(ExecutionPurpose::SupersedingRecovery),
            _ => None,
        }
    }
}

impl CanonicalCbor for ExecutionPurpose {
    fn to_cbor(&self) -> CborValue {
        CborValue::text(self.as_str())
    }
}

/// What postflight must confirm after the last step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostflightPolicy {
    pub require_exact_target_lineage: bool,
    pub require_mode: Option<crate::effect::DeviceMode>,
    /// Facts read back from the booted device and compared against the artifact
    /// manifest, e.g. product model and full build name.
    pub required_runtime_facts: Vec<(OpaqueId, String)>,
}

impl PostflightPolicy {
    pub fn none() -> Self {
        PostflightPolicy {
            require_exact_target_lineage: false,
            require_mode: None,
            required_runtime_facts: Vec::new(),
        }
    }
}

impl CanonicalCbor for PostflightPolicy {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            (
                "requireExactTargetLineage",
                CborValue::Bool(self.require_exact_target_lineage),
            ),
            (
                "requireMode",
                match &self.require_mode {
                    Some(mode) => mode.to_cbor(),
                    None => CborValue::Null,
                },
            ),
            (
                "requiredRuntimeFacts",
                CborValue::Map(
                    self.required_runtime_facts
                        .iter()
                        .map(|(key, value)| (key.to_cbor(), CborValue::text(value.clone())))
                        .collect(),
                ),
            ),
        ])
    }
}

/// Reference to a published recovery coverage declaration (architecture.md 14.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryContractRef {
    pub id: OpaqueId,
    pub version: crate::identity::Version,
    pub digest: Sha256Digest,
}

impl CanonicalCbor for RecoveryContractRef {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("id", self.id.to_cbor()),
            ("version", self.version.to_cbor()),
            ("digest", self.digest.to_cbor()),
        ])
    }
}

/// An immutable, executable plan.
///
/// Construct through [`FlashPlanEnvelope::seal`] so `plan_digest` can never
/// disagree with the contents it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlashPlanEnvelope {
    pub schema_version: PlanSchemaVersion,
    pub plan_id: PlanId,
    pub plan_digest: Sha256Digest,
    pub execution_purpose: ExecutionPurpose,

    pub authority_binding: AuthorityBindingRef,
    pub provider: ProviderIdentity,
    pub profile: DeviceProfileIdentity,
    pub artifact: ArtifactIdentity,
    pub toolchain: ToolchainIdentity,

    pub negotiated_capabilities: NegotiatedCapabilities,
    pub public_steps: Vec<PublicFlashStep>,
    pub effect_set: EffectSet,

    pub provider_execution_plan_digest: Sha256Digest,
    pub public_projection_digest: Sha256Digest,
    pub per_action_digests: Vec<ActionDigestBinding>,

    pub recovery_contract: Option<RecoveryContractRef>,
    pub postflight: PostflightPolicy,
    pub created_at_epoch_ms: u64,
    pub expires_at_epoch_ms: u64,
}

/// Everything a plan digest covers except the digest itself (architecture.md 15.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanSealInput {
    pub schema_version: PlanSchemaVersion,
    pub plan_id: PlanId,
    pub execution_purpose: ExecutionPurpose,
    pub authority_binding: AuthorityBindingRef,
    pub provider: ProviderIdentity,
    pub profile: DeviceProfileIdentity,
    pub artifact: ArtifactIdentity,
    pub toolchain: ToolchainIdentity,
    pub negotiated_capabilities: NegotiatedCapabilities,
    pub public_steps: Vec<PublicFlashStep>,
    pub effect_set: EffectSet,
    pub projection: ProjectionDigests,
    pub recovery_contract: Option<RecoveryContractRef>,
    pub postflight: PostflightPolicy,
    pub created_at_epoch_ms: u64,
    pub expires_at_epoch_ms: u64,
}

impl FlashPlanEnvelope {
    /// Seals a validated plan, computing `plan_digest` over its contents.
    ///
    /// The projection digests must come from
    /// [`crate::projection::validate_projection`]: sealing does not re-derive
    /// them, it binds them.
    pub fn seal(input: PlanSealInput) -> Result<Self, PlanError> {
        if input.expires_at_epoch_ms <= input.created_at_epoch_ms {
            return Err(PlanError::ExpiryNotAfterCreation {
                created: input.created_at_epoch_ms,
                expires: input.expires_at_epoch_ms,
            });
        }
        input
            .effect_set
            .validate_executable()
            .map_err(PlanError::Effect)?;

        let body = plan_digest_body(&input);
        let bytes = body.to_canonical_bytes().map_err(PlanError::Cbor)?;
        let plan_digest = digest_in_domain(Domain::Plan, &bytes);

        Ok(FlashPlanEnvelope {
            schema_version: input.schema_version,
            plan_id: input.plan_id,
            plan_digest,
            execution_purpose: input.execution_purpose,
            authority_binding: input.authority_binding,
            provider: input.provider,
            profile: input.profile,
            artifact: input.artifact,
            toolchain: input.toolchain,
            negotiated_capabilities: input.negotiated_capabilities,
            public_steps: input.public_steps,
            effect_set: input.effect_set,
            provider_execution_plan_digest: input.projection.provider_execution_plan_digest,
            public_projection_digest: input.projection.public_projection_digest,
            per_action_digests: input.projection.per_action,
            recovery_contract: input.recovery_contract,
            postflight: input.postflight,
            created_at_epoch_ms: input.created_at_epoch_ms,
            expires_at_epoch_ms: input.expires_at_epoch_ms,
        })
    }

    /// Recomputes the plan digest from the envelope's own contents.
    ///
    /// Used on load: a store that returns a plan whose digest no longer matches
    /// its bytes is corruption, and corruption fails closed rather than
    /// executing (architecture.md 6.3).
    pub fn recompute_digest(&self) -> Result<Sha256Digest, PlanError> {
        let input = PlanSealInput {
            schema_version: self.schema_version,
            plan_id: self.plan_id.clone(),
            execution_purpose: self.execution_purpose,
            authority_binding: self.authority_binding.clone(),
            provider: self.provider.clone(),
            profile: self.profile.clone(),
            artifact: self.artifact.clone(),
            toolchain: self.toolchain.clone(),
            negotiated_capabilities: self.negotiated_capabilities.clone(),
            public_steps: self.public_steps.clone(),
            effect_set: self.effect_set.clone(),
            projection: ProjectionDigests {
                per_action: self.per_action_digests.clone(),
                provider_execution_plan_digest: self.provider_execution_plan_digest,
                public_projection_digest: self.public_projection_digest,
            },
            recovery_contract: self.recovery_contract.clone(),
            postflight: self.postflight.clone(),
            created_at_epoch_ms: self.created_at_epoch_ms,
            expires_at_epoch_ms: self.expires_at_epoch_ms,
        };
        let bytes = plan_digest_body(&input)
            .to_canonical_bytes()
            .map_err(PlanError::Cbor)?;
        Ok(digest_in_domain(Domain::Plan, &bytes))
    }

    pub fn verify_self_digest(&self) -> Result<(), PlanError> {
        let recomputed = self.recompute_digest()?;
        if recomputed != self.plan_digest {
            return Err(PlanError::DigestMismatch {
                stored: self.plan_digest,
                recomputed,
            });
        }
        Ok(())
    }

    pub fn is_expired_at(&self, now_epoch_ms: u64) -> bool {
        now_epoch_ms >= self.expires_at_epoch_ms
    }
}

fn plan_digest_body(input: &PlanSealInput) -> CborValue {
    CborValue::map(vec![
        ("schemaVersion", input.schema_version.to_cbor()),
        ("planId", input.plan_id.to_cbor()),
        ("executionPurpose", input.execution_purpose.to_cbor()),
        ("authorityBinding", input.authority_binding.to_cbor()),
        ("provider", input.provider.to_cbor()),
        ("profile", input.profile.to_cbor()),
        ("artifact", input.artifact.to_cbor()),
        ("toolchain", input.toolchain.to_cbor()),
        (
            "negotiatedCapabilities",
            input.negotiated_capabilities.to_cbor(),
        ),
        (
            "publicSteps",
            CborValue::array(input.public_steps.iter().map(|s| s.to_cbor()).collect()),
        ),
        ("effectSet", input.effect_set.to_cbor()),
        (
            "providerExecutionPlanDigest",
            input.projection.provider_execution_plan_digest.to_cbor(),
        ),
        (
            "publicProjectionDigest",
            input.projection.public_projection_digest.to_cbor(),
        ),
        (
            "recoveryContract",
            match &input.recovery_contract {
                Some(contract) => contract.to_cbor(),
                None => CborValue::Null,
            },
        ),
        ("postflight", input.postflight.to_cbor()),
        (
            "createdAtEpochMs",
            CborValue::Unsigned(input.created_at_epoch_ms),
        ),
        (
            "expiresAtEpochMs",
            CborValue::Unsigned(input.expires_at_epoch_ms),
        ),
    ])
}

/// Why execution is not available for an assessed combination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionAvailability {
    Available,
    Unavailable { reason: String },
}

impl ExecutionAvailability {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExecutionAvailability::Available => "available",
            ExecutionAvailability::Unavailable { .. } => "unavailable",
        }
    }
}

impl CanonicalCbor for ExecutionAvailability {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("availability", CborValue::text(self.as_str())),
            (
                "reason",
                match self {
                    ExecutionAvailability::Available => CborValue::Null,
                    ExecutionAvailability::Unavailable { reason } => CborValue::text(reason.clone()),
                },
            ),
        ])
    }
}

/// A fact that is missing or contradictory, named precisely enough to be
/// closed by evidence rather than by argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionUnknown {
    pub id: OpaqueId,
    pub summary: String,
}

impl CanonicalCbor for ExecutionUnknown {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("id", self.id.to_cbor()),
            ("summary", CborValue::text(self.summary.clone())),
        ])
    }
}

/// What would have to be obtained to close an unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceRequirement {
    pub id: EvidenceId,
    pub closes: Vec<OpaqueId>,
    pub description: String,
    /// Evidence grade required (architecture.md 2.3): `A`..`D`.
    pub minimum_grade: char,
}

impl CanonicalCbor for EvidenceRequirement {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("id", self.id.to_cbor()),
            (
                "closes",
                CborValue::array(self.closes.iter().map(|id| id.to_cbor()).collect()),
            ),
            ("description", CborValue::text(self.description.clone())),
            (
                "minimumGrade",
                CborValue::text(self.minimum_grade.to_string()),
            ),
        ])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCandidate {
    pub provider: ProviderIdentity,
    pub maturity: MaturityState,
    pub rationale: String,
}

impl CanonicalCbor for ProviderCandidate {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("provider", self.provider.to_cbor()),
            ("maturity", self.maturity.to_cbor()),
            ("rationale", CborValue::text(self.rationale.clone())),
        ])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileCandidate {
    pub profile: DeviceProfileIdentity,
    pub maturity: MaturityState,
    pub rationale: String,
}

impl CanonicalCbor for ProfileCandidate {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("profile", self.profile.to_cbor()),
            ("maturity", self.maturity.to_cbor()),
            ("rationale", CborValue::text(self.rationale.clone())),
        ])
    }
}

/// A non-executable assessment.
///
/// It has no plan id and no plan digest — not as a convention, but because the
/// type has no field to put one in. `startExecution` takes a `PlanId`, so an
/// assessment cannot reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanAssessment {
    pub provider_candidates: Vec<ProviderCandidate>,
    pub profile_candidates: Vec<ProfileCandidate>,
    pub known_effects: EffectSet,
    pub unknowns: Vec<ExecutionUnknown>,
    pub evidence_requirements: Vec<EvidenceRequirement>,
    pub availability: ExecutionAvailability,
}

impl CanonicalCbor for PlanAssessment {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            (
                "providerCandidates",
                CborValue::array(self.provider_candidates.iter().map(|c| c.to_cbor()).collect()),
            ),
            (
                "profileCandidates",
                CborValue::array(self.profile_candidates.iter().map(|c| c.to_cbor()).collect()),
            ),
            ("knownEffects", self.known_effects.to_cbor()),
            (
                "unknowns",
                CborValue::array(self.unknowns.iter().map(|u| u.to_cbor()).collect()),
            ),
            (
                "evidenceRequirements",
                CborValue::array(
                    self.evidence_requirements
                        .iter()
                        .map(|r| r.to_cbor())
                        .collect(),
                ),
            ),
            ("availability", self.availability.to_cbor()),
        ])
    }
}

/// The result of materialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanMaterialization {
    Executable(Box<FlashPlanEnvelope>),
    Assessment(Box<PlanAssessment>),
}

impl PlanMaterialization {
    pub fn executable(&self) -> Option<&FlashPlanEnvelope> {
        match self {
            PlanMaterialization::Executable(plan) => Some(plan),
            PlanMaterialization::Assessment(_) => None,
        }
    }

    pub fn assessment(&self) -> Option<&PlanAssessment> {
        match self {
            PlanMaterialization::Assessment(assessment) => Some(assessment),
            PlanMaterialization::Executable(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    ExpiryNotAfterCreation {
        created: u64,
        expires: u64,
    },
    DigestMismatch {
        stored: Sha256Digest,
        recomputed: Sha256Digest,
    },
    Effect(crate::effect::EffectError),
    Cbor(CborError),
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanError::ExpiryNotAfterCreation { created, expires } => write!(
                f,
                "plan expiry {expires} must be after creation {created}"
            ),
            PlanError::DigestMismatch { stored, recomputed } => write!(
                f,
                "stored plan digest {stored} does not match recomputed {recomputed}"
            ),
            PlanError::Effect(error) => write!(f, "{error}"),
            PlanError::Cbor(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for PlanError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::AuthorityNamespace;
    use crate::digest::sha256;
    use crate::effect::{ByteRange, DataImpact, DataImpactState, PersistentEffect};
    use crate::identity::{ArtifactFormat, ToolchainKind, Version};
    use crate::ids::{ActionId, PartitionId, StepId};
    use crate::projection::{
        validate_projection, PrivateActionRecord, PrivateActionRole, StoredProviderPlan,
    };
    use crate::step::{
        BindingRequirement, CancellationPolicy, FlashStepKind, SemanticTarget, WorkflowEffect,
    };

    fn seal_input() -> PlanSealInput {
        let content = sha256(b"system image");
        let range = ByteRange::new(0, 4096).unwrap();
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
            body: CborValue::map(vec![("tool", CborValue::text("wlx"))]),
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
        let effect_set = EffectSet {
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
        let projection = validate_projection(
            std::slice::from_ref(&step),
            &StoredProviderPlan {
                actions: vec![action],
            },
            &effect_set,
        )
        .unwrap();

        PlanSealInput {
            schema_version: PlanSchemaVersion::CURRENT,
            plan_id: PlanId::new("PLAN-001").unwrap(),
            execution_purpose: ExecutionPurpose::PrimaryFlash,
            authority_binding: AuthorityBindingRef {
                authority_namespace: AuthorityNamespace::new("test-authority").unwrap(),
                binding_id: OpaqueId::new("TGT-958780b2ffb7").unwrap(),
                binding_revision: 2,
                stable_identity_digest: sha256(b"device"),
            },
            provider: ProviderIdentity {
                id: OpaqueId::new("arkforge.example").unwrap(),
                version: Version::new(1, 0, 0),
                implementation_digest: sha256(b"impl"),
            },
            profile: DeviceProfileIdentity {
                id: OpaqueId::new("org.example.testboard").unwrap(),
                version: Version::new(1, 0, 0),
                digest: sha256(b"profile"),
            },
            artifact: ArtifactIdentity {
                artifact_id: OpaqueId::new("ART-1").unwrap(),
                format: ArtifactFormat {
                    id: OpaqueId::new("example-images-targz").unwrap(),
                    version: Version::new(1, 0, 0),
                },
                content_digest: sha256(b"archive"),
                size_bytes: 730_769_584,
                manifest_digest: sha256(b"manifest"),
            },
            toolchain: ToolchainIdentity {
                id: OpaqueId::new("example-tool-fixed").unwrap(),
                kind: ToolchainKind::FixedTool,
                version: Version::new(1, 32, 0),
                backend_digest: sha256(b"tool"),
            },
            negotiated_capabilities: NegotiatedCapabilities::empty(),
            public_steps: vec![step],
            effect_set,
            projection,
            recovery_contract: None,
            postflight: PostflightPolicy::none(),
            created_at_epoch_ms: 1_000,
            expires_at_epoch_ms: 2_000,
        }
    }

    #[test]
    fn sealing_twice_produces_the_same_digest() {
        let first = FlashPlanEnvelope::seal(seal_input()).unwrap();
        let second = FlashPlanEnvelope::seal(seal_input()).unwrap();
        assert_eq!(first.plan_digest, second.plan_digest);
        first.verify_self_digest().unwrap();
    }

    #[test]
    fn mutating_a_sealed_plan_breaks_its_digest() {
        let mut plan = FlashPlanEnvelope::seal(seal_input()).unwrap();
        plan.public_steps[0].effect = WorkflowEffect::ReadOnly;
        assert!(matches!(
            plan.verify_self_digest(),
            Err(PlanError::DigestMismatch { .. })
        ));
    }

    #[test]
    fn a_changed_toolchain_changes_the_plan_digest() {
        let baseline = FlashPlanEnvelope::seal(seal_input()).unwrap();
        let mut input = seal_input();
        input.toolchain.backend_digest = sha256(b"a different tool build");
        let rebuilt = FlashPlanEnvelope::seal(input).unwrap();
        assert_ne!(baseline.plan_digest, rebuilt.plan_digest);
    }

    #[test]
    fn a_plan_that_never_expires_is_rejected() {
        let mut input = seal_input();
        input.expires_at_epoch_ms = input.created_at_epoch_ms;
        assert!(matches!(
            FlashPlanEnvelope::seal(input),
            Err(PlanError::ExpiryNotAfterCreation { .. })
        ));
    }

    #[test]
    fn unknown_data_impact_cannot_be_sealed() {
        let mut input = seal_input();
        input.effect_set.data_impact.userdata = DataImpactState::Unknown;
        assert!(matches!(
            FlashPlanEnvelope::seal(input),
            Err(PlanError::Effect(_))
        ));
    }

    #[test]
    fn an_assessment_carries_no_plan_id() {
        let assessment = PlanAssessment {
            provider_candidates: vec![],
            profile_candidates: vec![],
            known_effects: EffectSet::read_only(),
            unknowns: vec![ExecutionUnknown {
                id: OpaqueId::new("UNI-U01").unwrap(),
                summary: "UIS7885 PAC/FDL wire protocol".into(),
            }],
            evidence_requirements: vec![],
            availability: ExecutionAvailability::Unavailable {
                reason: "evidence gate 17.5 not passed".into(),
            },
        };
        let materialization = PlanMaterialization::Assessment(Box::new(assessment));
        assert!(materialization.executable().is_none());
        assert!(materialization.assessment().is_some());
    }
}
