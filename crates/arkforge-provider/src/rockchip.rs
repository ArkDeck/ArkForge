//! Rockchip provider — DAYU200 read-only probe and plan materialization.
//!
//! architecture.md 16. This module is where `wlx`, `rl` and sector offsets are
//! allowed to exist. They appear only inside private action bodies, which never
//! cross the Agent/App API; what crosses is a public step naming a partition
//! and a digest that binds it to exactly one private action (architecture.md 6).
//!
//! AF-V1 implements probe, validate and materialize. Execution is AF-V2 and the
//! SPI's default refusal stands.

use crate::{
    FlashIntent, FlashProvider, MaterializeRequest, MaturityRegistry, ProbeContext,
    ProviderDescriptor, ProviderError, ProviderProbe, ValidationReport,
};
use arkforge_artifact::manifest::ArtifactManifest;
use arkforge_core::digest::{digest_in_domain, sha256, CanonicalCbor, CborValue, Domain};
use arkforge_core::effect::{
    ByteRange, DeviceMode, EffectSet, PersistentEffect, TransientEffect,
};
use arkforge_core::identity::{
    ArtifactFormat, ArtifactIdentity, MaturityKey, MaturityState, ProviderIdentity, Version,
};
use arkforge_core::ids::{ActionId, OpaqueId, StepId};
use arkforge_core::plan::{
    EvidenceRequirement, ExecutionAvailability, ExecutionPurpose, ExecutionUnknown,
    FlashPlanEnvelope, PlanAssessment, PlanMaterialization, PlanSchemaVersion, PlanSealInput,
    PostflightPolicy, ProfileCandidate, ProviderCandidate,
};
use arkforge_core::profile::{AllowedTarget, DeviceProfile};
use arkforge_core::projection::{
    validate_projection, PrivateActionRecord, PrivateActionRole, StoredProviderPlan,
};
use arkforge_core::step::{
    BindingRequirement, CancellationPolicy, FlashStepKind, PublicFlashStep, SemanticTarget,
    WorkflowEffect,
};
use arkforge_core::{EvidenceId, NegotiatedCapabilities, Sha256Digest};
use arkforge_transport::TransportError;

pub const PROVIDER_ID: &str = "arkforge.rockchip";
pub const BACKEND_FIXED_TOOL: &str = "rkdeveloptool-fixed";
pub const BACKEND_REPLAY: &str = "transcript-replay";

/// The mode the device must be in to accept writes.
const LOADER_MODE: &str = "rockusb-loader";
const NORMAL_MODE: &str = "hdc-normal";

/// The result of materialization together with the private plan it produced.
///
/// The private plan never leaves the daemon. It is returned here so the engine
/// can store it beside the envelope; nothing in the IPC surface serializes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedPlan {
    pub materialization: PlanMaterialization,
    pub private_plan: Option<StoredProviderPlan>,
}

/// The DAYU200 Rockchip provider.
#[derive(Debug, Clone)]
pub struct RockchipProvider {
    identity: ProviderIdentity,
}

impl Default for RockchipProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl RockchipProvider {
    pub fn new() -> Self {
        RockchipProvider {
            identity: ProviderIdentity {
                id: OpaqueId::new(PROVIDER_ID).expect("literal identifier"),
                version: Version::new(1, 0, 0),
                // A build-identifying digest. It changes when the lowering
                // changes, which is what makes a rebuilt provider a different
                // provider for maturity and plan binding (architecture.md 16.1).
                implementation_digest: sha256(
                    concat!(
                        "arkforge.rockchip/lowering/v1;",
                        "steps=ensure-mode,probe,validate-layout,write*,verify*,reboot,postflight;",
                        "actions=enter-loader,probe-loader,validate-partition-table,",
                        "write-partition(wlx),characterize-read-domain,readback-partition(rl),",
                        "reset-device,verify-hdc-postflight"
                    )
                    .as_bytes(),
                ),
            },
        }
    }

    pub fn identity(&self) -> &ProviderIdentity {
        &self.identity
    }

    fn artifact_format(&self) -> ArtifactFormat {
        ArtifactFormat {
            id: OpaqueId::new(arkforge_artifact::dayu200::FORMAT_ID).expect("literal identifier"),
            version: arkforge_artifact::dayu200::FORMAT_VERSION,
        }
    }

    /// Builds the plan and decides whether it may be executable.
    ///
    /// The two decisions are separate on purpose: the plan is always built in
    /// full, so an assessment can show exactly what *would* happen, and the
    /// gate then decides whether it may be handed to an authority.
    pub fn materialize_with_private_plan(
        &self,
        request: &MaterializeRequest<'_>,
        maturity: &MaturityRegistry,
    ) -> Result<MaterializedPlan, ProviderError> {
        let profile = request.profile;
        let artifact = request.artifact;

        let profile_identity = profile
            .identity()
            .map_err(|error| ProviderError::Core(error.to_string()))?;
        let maturity_key = MaturityKey {
            provider: self.identity.clone(),
            profile: profile_identity.clone(),
            artifact_format: self.artifact_format(),
            toolchain: request.toolchain.clone(),
            host_platform: request.host_platform.clone(),
            driver_facts_digest: request.driver_facts_digest,
            evidence_set_digest: request.evidence_set_digest,
        };
        let maturity_state = maturity.lookup(&maturity_key);

        let validation = self.validate(artifact, profile, request.probe)?;
        let mut blockers: Vec<ExecutionUnknown> = Vec::new();
        for violation in &validation.violations {
            blockers.push(ExecutionUnknown {
                id: violation.id.clone(),
                summary: violation.detail.clone(),
            });
        }
        blockers.extend(residual_artifact_unknowns(artifact, profile));
        if !maturity_state.permits_executable_plan() {
            blockers.push(ExecutionUnknown {
                id: OpaqueId::new("RK-M02").expect("literal identifier"),
                summary: format!(
                    "provider/profile/artifact/toolchain/platform combination is {}{}",
                    maturity_state.as_str(),
                    maturity_state
                        .blocker()
                        .map(|blocker| format!(": {blocker}"))
                        .unwrap_or_default()
                ),
            });
        }

        // Build the whole plan regardless, so an assessment is as informative
        // as an executable plan minus its authority.
        let built = self.build_steps(request)?;

        if !blockers.is_empty() {
            let assessment = PlanAssessment {
                provider_candidates: vec![ProviderCandidate {
                    provider: self.identity.clone(),
                    maturity: maturity_state.clone(),
                    rationale: "Rockchip RockUSB provider for the DAYU200 vertical".into(),
                }],
                profile_candidates: vec![ProfileCandidate {
                    profile: profile_identity,
                    maturity: maturity_state.clone(),
                    rationale: format!(
                        "profile declares {} writable targets",
                        profile.allowed_targets.len()
                    ),
                }],
                known_effects: built.effect_set,
                evidence_requirements: evidence_requirements(&blockers),
                unknowns: blockers,
                availability: ExecutionAvailability::Unavailable {
                    reason: format!(
                        "materialization is complete but execution is gated; maturity is {}",
                        maturity_state.as_str()
                    ),
                },
            };
            return Ok(MaterializedPlan {
                materialization: PlanMaterialization::Assessment(Box::new(assessment)),
                // The private plan is still returned: an assessment must be
                // able to show its own projection under audit.
                private_plan: Some(built.private_plan),
            });
        }

        let projection = validate_projection(&built.public_steps, &built.private_plan, &built.effect_set)
            .map_err(|error| ProviderError::Core(error.to_string()))?;

        let artifact_identity = ArtifactIdentity {
            artifact_id: request.artifact_id.clone(),
            format: self.artifact_format(),
            content_digest: artifact.content_digest,
            size_bytes: artifact.size_bytes,
            manifest_digest: artifact
                .digest()
                .map_err(|error| ProviderError::Core(error.to_string()))?,
        };

        let envelope = FlashPlanEnvelope::seal(PlanSealInput {
            schema_version: PlanSchemaVersion::CURRENT,
            plan_id: request.plan_id.clone(),
            execution_purpose: ExecutionPurpose::PrimaryFlash,
            authority_binding: request.authority_binding.clone(),
            provider: self.identity.clone(),
            profile: profile_identity,
            artifact: artifact_identity,
            toolchain: request.toolchain.clone(),
            negotiated_capabilities: NegotiatedCapabilities::empty(),
            public_steps: built.public_steps,
            effect_set: built.effect_set,
            projection,
            recovery_contract: None,
            postflight: built.postflight,
            created_at_epoch_ms: request.created_at_epoch_ms,
            expires_at_epoch_ms: request.created_at_epoch_ms + request.plan_lifetime_ms,
        })
        .map_err(|error| ProviderError::Core(error.to_string()))?;

        Ok(MaterializedPlan {
            materialization: PlanMaterialization::Executable(Box::new(envelope)),
            private_plan: Some(built.private_plan),
        })
    }

    fn build_steps(&self, request: &MaterializeRequest<'_>) -> Result<BuiltPlan, ProviderError> {
        let profile = request.profile;
        let artifact = request.artifact;
        if request.intent != FlashIntent::FullRestore {
            return Err(ProviderError::Unsupported(format!(
                "intent {} is not implemented",
                request.intent.as_str()
            )));
        }

        let normal = mode(NORMAL_MODE)?;
        let loader = mode(LOADER_MODE)?;

        let mut public_steps: Vec<PublicFlashStep> = Vec::new();
        let mut actions: Vec<PrivateActionRecord> = Vec::new();
        let mut persistent: Vec<PersistentEffect> = Vec::new();
        let transient = vec![
            TransientEffect::EnterMode {
                from: normal.clone(),
                to: loader.clone(),
            },
            TransientEffect::Reboot {
                target_mode: normal.clone(),
            },
        ];

        let mut sequence = 0u32;

        // 1. Enter the loader through the authority's managed control port.
        {
            let (step_id, action_id) = next_ids(&mut sequence, "ACT");
            let action = private_action(
                action_id,
                step_id.clone(),
                PrivateActionRole::PrimaryEffect,
                WorkflowEffect::Transient,
                None,
                None,
                None,
                CborValue::map(vec![
                    ("action", CborValue::text("enter-loader")),
                    ("via", CborValue::text("managed-device-control-port")),
                    ("controlAction", CborValue::text("enter-updater")),
                ]),
            );
            public_steps.push(PublicFlashStep {
                step_id,
                kind: FlashStepKind::EnsureMode,
                // The kind's own floor is Transient — a mode change does not
                // survive power loss. It is declared Mutating because the
                // authority's registry floor for `enterUpdater` is a device
                // mutation, and a step that under-declares against the registry
                // is not admissible (architecture.md 5.4). Over-declaring only
                // tightens admission.
                effect: WorkflowEffect::Mutating,
                cancellation: CancellationPolicy::CancellableAtBoundary,
                binding: BindingRequirement::ExactBoundTargetWithModeLineage,
                semantic_target: Some(SemanticTarget::Device),
                content_digest: None,
                expected_mode_before: Some(normal.clone()),
                expected_mode_after: Some(loader.clone()),
                private_action_digest: digest_of(&action)?,
            });
            actions.push(action);
        }

        // 2. Probe the loader for exact identity.
        {
            let (step_id, action_id) = next_ids(&mut sequence, "ACT");
            let action = private_action(
                action_id,
                step_id.clone(),
                PrivateActionRole::PrimaryEffect,
                WorkflowEffect::ReadOnly,
                None,
                None,
                None,
                CborValue::map(vec![
                    ("action", CborValue::text("probe-loader")),
                    ("tool", CborValue::text("rkdeveloptool")),
                    ("command", CborValue::text("ld")),
                ]),
            );
            public_steps.push(PublicFlashStep {
                step_id,
                kind: FlashStepKind::ProbeDevice,
                effect: WorkflowEffect::ReadOnly,
                cancellation: CancellationPolicy::CancellableImmediately,
                binding: BindingRequirement::ExactBoundTargetWithModeLineage,
                semantic_target: Some(SemanticTarget::Device),
                content_digest: None,
                expected_mode_before: Some(loader.clone()),
                expected_mode_after: Some(loader.clone()),
                private_action_digest: digest_of(&action)?,
            });
            actions.push(action);
        }

        // 3. Validate that the device's own table is the one the plan assumes.
        {
            let (step_id, action_id) = next_ids(&mut sequence, "ACT");
            let layout_digest = layout_digest(profile);
            let action = private_action(
                action_id,
                step_id.clone(),
                PrivateActionRole::PrimaryEffect,
                WorkflowEffect::ReadOnly,
                None,
                None,
                None,
                CborValue::map(vec![
                    ("action", CborValue::text("validate-partition-table")),
                    ("tool", CborValue::text("rkdeveloptool")),
                    ("command", CborValue::text("ppt")),
                    ("expectedLayoutDigest", CborValue::Bytes(layout_digest.as_bytes().to_vec())),
                ]),
            );
            public_steps.push(PublicFlashStep {
                step_id,
                kind: FlashStepKind::ValidateLayout,
                effect: WorkflowEffect::ReadOnly,
                cancellation: CancellationPolicy::CancellableImmediately,
                binding: BindingRequirement::ExactBoundTarget,
                semantic_target: Some(SemanticTarget::Device),
                content_digest: None,
                expected_mode_before: Some(loader.clone()),
                expected_mode_after: Some(loader.clone()),
                private_action_digest: digest_of(&action)?,
            });
            actions.push(action);
        }

        // 4. The writes, in the profile's declared order.
        let mut ordered: Vec<&AllowedTarget> = profile.allowed_targets.iter().collect();
        ordered.sort_by_key(|target| target.write_order);
        for target in &ordered {
            let member_name = target.source_member.as_deref().ok_or_else(|| {
                ProviderError::FactsInsufficient(format!(
                    "profile target {} declares no source member",
                    target.partition
                ))
            })?;
            let member = artifact.member(member_name).ok_or_else(|| {
                ProviderError::FactsInsufficient(format!(
                    "artifact has no member {member_name} for target {}",
                    target.partition
                ))
            })?;
            let block_size = block_size(profile)?;
            let start = target
                .offset_sectors
                .checked_mul(block_size)
                .ok_or_else(|| {
                    ProviderError::FactsInsufficient(format!(
                        "target {} offset overflows a byte address",
                        target.partition
                    ))
                })?;
            let range = ByteRange::new(start, member.size_bytes)
                .map_err(|error| ProviderError::FactsInsufficient(error.to_string()))?;

            let (step_id, action_id) = next_ids(&mut sequence, "ACT");
            let action = private_action(
                action_id,
                step_id.clone(),
                PrivateActionRole::PrimaryEffect,
                WorkflowEffect::Destructive,
                Some(SemanticTarget::Partition(target.partition.clone())),
                Some(range),
                Some(member.sha256),
                CborValue::map(vec![
                    ("action", CborValue::text("write-partition")),
                    ("tool", CborValue::text("rkdeveloptool")),
                    // `wlx` addresses by partition name, resolved from the
                    // device's own table — which the preceding step has just
                    // proved equal to this plan's. There is deliberately no
                    // sector-addressed fallback: the executor's closed command
                    // surface cannot spell one, so naming one here would be a
                    // capability the plan claims and the executor does not have.
                    // `beginSector` is carried as a refusal check, not as an
                    // address (crate::rockchip_execute).
                    ("command", CborValue::text("wlx")),
                    ("partition", CborValue::text(target.partition.as_str())),
                    ("beginSector", CborValue::Unsigned(target.offset_sectors)),
                    ("member", CborValue::text(member_name)),
                ]),
            );
            public_steps.push(PublicFlashStep {
                step_id,
                kind: FlashStepKind::WriteTarget,
                effect: WorkflowEffect::Destructive,
                // A partition write is not interruptible mid-flight; cancel
                // queues to the next safe boundary (architecture.md 13.4).
                cancellation: CancellationPolicy::NonInterruptible,
                binding: BindingRequirement::ExactBoundTargetWithModeLineage,
                semantic_target: Some(SemanticTarget::Partition(target.partition.clone())),
                content_digest: Some(member.sha256),
                expected_mode_before: Some(loader.clone()),
                expected_mode_after: Some(loader.clone()),
                private_action_digest: digest_of(&action)?,
            });
            actions.push(action);
            persistent.push(PersistentEffect::WritePartition {
                partition: target.partition.clone(),
                range,
                content: member.sha256,
            });
        }

        // 5. Verification, read-domain aware: characterize first, then read
        //    back only what the read face can reach (architecture.md 16.2/16.4).
        for target in &ordered {
            let member_name = target.source_member.as_deref().expect("checked above");
            let member = artifact.member(member_name).expect("checked above");
            let block_size = block_size(profile)?;
            let range = ByteRange::new(
                target.offset_sectors * block_size,
                member.size_bytes,
            )
            .map_err(|error| ProviderError::FactsInsufficient(error.to_string()))?;

            let (step_id, action_id) = next_ids(&mut sequence, "ACT");
            let readback = private_action(
                action_id,
                step_id.clone(),
                PrivateActionRole::PrimaryEffect,
                WorkflowEffect::ReadOnly,
                Some(SemanticTarget::Partition(target.partition.clone())),
                Some(range),
                Some(member.sha256),
                CborValue::map(vec![
                    ("action", CborValue::text("readback-partition")),
                    ("tool", CborValue::text("rkdeveloptool")),
                    ("command", CborValue::text("rl")),
                    ("partition", CborValue::text(target.partition.as_str())),
                    ("beginSector", CborValue::Unsigned(target.offset_sectors)),
                    (
                        "maxStrengthWhenReadable",
                        CborValue::text(target.verification.max_strength_when_readable.as_str()),
                    ),
                    (
                        "erasedMediumFiller",
                        match profile.read_domain.erased_medium_filler {
                            Some(byte) => CborValue::Unsigned(byte as u64),
                            None => CborValue::Null,
                        },
                    ),
                ]),
            );
            let characterize = private_action(
                ActionId::new(format!("SUB-{:03}", sequence)).expect("generated identifier"),
                step_id.clone(),
                PrivateActionRole::ReadOnlyTransportSubAction,
                WorkflowEffect::ReadOnly,
                None,
                None,
                None,
                CborValue::map(vec![
                    ("action", CborValue::text("characterize-read-domain")),
                    ("tool", CborValue::text("rkdeveloptool")),
                    ("command", CborValue::text("rl")),
                    ("probe", CborValue::text("primary-and-backup-gpt")),
                    (
                        "policy",
                        CborValue::text(profile.read_domain.read.as_str()),
                    ),
                ]),
            );
            public_steps.push(PublicFlashStep {
                step_id,
                kind: FlashStepKind::VerifyTarget,
                effect: WorkflowEffect::ReadOnly,
                cancellation: CancellationPolicy::CancellableImmediately,
                binding: BindingRequirement::ExactBoundTargetWithModeLineage,
                semantic_target: Some(SemanticTarget::Partition(target.partition.clone())),
                content_digest: Some(member.sha256),
                expected_mode_before: Some(loader.clone()),
                expected_mode_after: Some(loader.clone()),
                private_action_digest: digest_of(&readback)?,
            });
            // Order matters and is not cosmetic: architecture.md 16.2 pairs
            // VerifyTarget as "CharacterizeReadDomain + ReadbackPartition", and
            // a readback that ran first would have to classify uniform filler
            // with no measurement of whether the read face reaches that far —
            // which is precisely the mistake AD-006 records.
            actions.push(characterize);
            actions.push(readback);
        }

        // 6. Reboot back to normal.
        {
            let (step_id, action_id) = next_ids(&mut sequence, "ACT");
            let action = private_action(
                action_id,
                step_id.clone(),
                PrivateActionRole::PrimaryEffect,
                WorkflowEffect::Transient,
                None,
                None,
                None,
                CborValue::map(vec![
                    ("action", CborValue::text("reset-device")),
                    ("tool", CborValue::text("rkdeveloptool")),
                    ("command", CborValue::text("rd")),
                ]),
            );
            public_steps.push(PublicFlashStep {
                step_id,
                kind: FlashStepKind::Reboot,
                // As with EnsureMode: the registry floor for `rebootDevice` is
                // a device mutation cancellable at a safe boundary.
                effect: WorkflowEffect::Mutating,
                cancellation: CancellationPolicy::CancellableAtBoundary,
                binding: BindingRequirement::ExactBoundTargetWithModeLineage,
                semantic_target: Some(SemanticTarget::Device),
                content_digest: None,
                expected_mode_before: Some(loader.clone()),
                expected_mode_after: Some(normal.clone()),
                private_action_digest: digest_of(&action)?,
            });
            actions.push(action);
        }

        // 7. Postflight: re-adopt the exact target and confirm the build.
        //    For every target past the read window this is the verification
        //    that actually carries the write (AD-006).
        let build_facts: Vec<(OpaqueId, String)> = artifact
            .build_facts
            .iter()
            .filter(|(key, _)| {
                matches!(
                    key.as_str(),
                    "const.ohos.fullname" | "const.product.model"
                )
            })
            .cloned()
            .collect();
        {
            let (step_id, action_id) = next_ids(&mut sequence, "ACT");
            let action = private_action(
                action_id,
                step_id.clone(),
                PrivateActionRole::PrimaryEffect,
                WorkflowEffect::ReadOnly,
                None,
                None,
                None,
                CborValue::map(vec![
                    ("action", CborValue::text("verify-hdc-postflight")),
                    ("via", CborValue::text("managed-device-control-port")),
                    ("controlAction", CborValue::text("read-build-facts")),
                    (
                        "expect",
                        CborValue::Map(
                            build_facts
                                .iter()
                                .map(|(key, value)| {
                                    (key.to_cbor(), CborValue::text(value.clone()))
                                })
                                .collect(),
                        ),
                    ),
                ]),
            );
            public_steps.push(PublicFlashStep {
                step_id,
                kind: FlashStepKind::PostflightProbe,
                effect: WorkflowEffect::ReadOnly,
                cancellation: CancellationPolicy::CancellableImmediately,
                binding: BindingRequirement::ExactBoundTargetWithModeLineage,
                semantic_target: Some(SemanticTarget::Device),
                content_digest: None,
                expected_mode_before: Some(normal.clone()),
                expected_mode_after: Some(normal.clone()),
                private_action_digest: digest_of(&action)?,
            });
            actions.push(action);
        }

        Ok(BuiltPlan {
            public_steps,
            private_plan: StoredProviderPlan { actions },
            effect_set: EffectSet {
                persistent,
                transient,
                data_impact: profile.data_impact,
            },
            postflight: PostflightPolicy {
                require_exact_target_lineage: true,
                require_mode: Some(normal),
                required_runtime_facts: build_facts,
            },
        })
    }
}

#[derive(Debug)]
struct BuiltPlan {
    public_steps: Vec<PublicFlashStep>,
    private_plan: StoredProviderPlan,
    effect_set: EffectSet,
    postflight: PostflightPolicy,
}

/// Allocates the paired step and action identifiers for one plan position.
fn next_ids(sequence: &mut u32, prefix: &str) -> (StepId, ActionId) {
    *sequence += 1;
    (
        StepId::new(format!("STEP-{sequence:03}")).expect("generated identifier"),
        ActionId::new(format!("{prefix}-{sequence:03}")).expect("generated identifier"),
    )
}

#[allow(clippy::too_many_arguments)]
fn private_action(
    action_id: ActionId,
    step_id: StepId,
    role: PrivateActionRole,
    effect_class: WorkflowEffect,
    declared_target: Option<SemanticTarget>,
    declared_range: Option<ByteRange>,
    content_digest: Option<Sha256Digest>,
    body: CborValue,
) -> PrivateActionRecord {
    PrivateActionRecord {
        action_id,
        step_id,
        role,
        effect_class,
        declared_target,
        declared_range,
        content_digest,
        body,
    }
}

fn digest_of(action: &PrivateActionRecord) -> Result<Sha256Digest, ProviderError> {
    action
        .digest()
        .map_err(|error| ProviderError::Core(error.to_string()))
}

/// A plan needs an exact byte address, so an unmeasured block size is a hard
/// stop here rather than a default of 512.
fn block_size(profile: &DeviceProfile) -> Result<u64, ProviderError> {
    profile
        .storage
        .logical_block_size
        .map(u64::from)
        .ok_or_else(|| {
            ProviderError::FactsInsufficient(format!(
                "profile {} does not declare a logical block size, so a sector offset cannot \
                 become a byte address",
                profile.id
            ))
        })
}

fn mode(name: &str) -> Result<DeviceMode, ProviderError> {
    DeviceMode::new(name).map_err(|error| ProviderError::Core(error.to_string()))
}

/// A digest over the layout the plan assumes, so the ValidateLayout step has
/// something exact to compare the device's own table against.
fn layout_digest(profile: &DeviceProfile) -> Sha256Digest {
    let mut ordered: Vec<&AllowedTarget> = profile.allowed_targets.iter().collect();
    ordered.sort_by_key(|target| target.write_order);
    let value = CborValue::array(
        ordered
            .iter()
            .map(|target| {
                CborValue::map(vec![
                    ("partition", CborValue::text(target.partition.as_str())),
                    (
                        "offsetSectors",
                        CborValue::Unsigned(target.offset_sectors),
                    ),
                ])
            })
            .collect(),
    );
    let bytes = value
        .to_canonical_bytes()
        .expect("layout values are canonical");
    digest_in_domain(Domain::DeviceProfile, &bytes)
}

/// The parser's unknowns, minus the ones this Profile discharges.
///
/// The parser will not guess what an unclassifiable member is; the Profile
/// says. Anything the Profile does not account for stays an unknown, so an
/// archive that grew a member nobody reviewed still blocks execution
/// (architecture.md 5.5, AF-V1 "unknown member fail closed").
///
/// Parser confidence is not a separate gate: it is derived from exactly these
/// unknowns, so checking it again would double-count a discharged one.
fn residual_artifact_unknowns(
    artifact: &ArtifactManifest,
    profile: &DeviceProfile,
) -> Vec<ExecutionUnknown> {
    let unaccounted: Vec<&String> = artifact
        .unclassified_members
        .iter()
        .filter(|member| !profile.known_metadata_members.contains(member))
        .collect();

    artifact
        .execution_relevant_unknowns
        .iter()
        .filter_map(|unknown| {
            if unknown.id.as_str() == "RK-A02" {
                if unaccounted.is_empty() {
                    return None;
                }
                return Some(ExecutionUnknown {
                    id: unknown.id.clone(),
                    summary: format!(
                        "archive members the profile does not account for: {}",
                        unaccounted
                            .iter()
                            .map(|member| member.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                });
            }
            Some(unknown.clone())
        })
        .collect()
}

fn evidence_requirements(blockers: &[ExecutionUnknown]) -> Vec<EvidenceRequirement> {
    blockers
        .iter()
        .map(|blocker| EvidenceRequirement {
            id: EvidenceId::new(format!("EVR-{}", blocker.id)).unwrap_or_else(|_| {
                EvidenceId::new("EVR-UNNAMED").expect("literal identifier")
            }),
            closes: vec![blocker.id.clone()],
            description: blocker.summary.clone(),
            minimum_grade: 'C',
        })
        .collect()
}

impl FlashProvider for RockchipProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            identity: self.identity.clone(),
            artifact_formats: vec![self.artifact_format()],
            backends: vec![
                OpaqueId::new(BACKEND_FIXED_TOOL).expect("literal identifier"),
                OpaqueId::new(BACKEND_REPLAY).expect("literal identifier"),
            ],
        }
    }

    fn probe(&self, ctx: &ProbeContext<'_>) -> Result<ProviderProbe, ProviderError> {
        // Read-only: open the exact device the caller observed, re-read its
        // identity on the same handle, and report. No command that could change
        // the device exists on this path.
        let mut session = ctx
            .transport
            .open_exact(ctx.observation)
            .map_err(|error: TransportError| ProviderError::Unsupported(error.to_string()))?;
        let observation = session
            .reread_identity()
            .map_err(|error| ProviderError::Unsupported(error.to_string()))?;

        if ctx.profile.mode(&observation.mode).is_none() {
            return Err(ProviderError::Unsupported(format!(
                "profile {} declares no mode matching {}",
                ctx.profile.id, observation.mode
            )));
        }

        let mut protocol_facts: Vec<(OpaqueId, String)> = observation
            .protocol_identity
            .iter()
            .map(|fact| (fact.key.clone(), fact.value.clone()))
            .collect();
        protocol_facts.push((
            OpaqueId::new("transport").expect("literal identifier"),
            ctx.transport.transport_id().to_string(),
        ));
        protocol_facts.sort();

        let facts_digest = observation
            .facts_digest()
            .map_err(|error| ProviderError::Core(error.to_string()))?;
        let profile_candidate = ctx
            .profile
            .identity()
            .map(Some)
            .map_err(|error| ProviderError::Core(error.to_string()))?;

        Ok(ProviderProbe {
            observation,
            protocol_facts,
            profile_candidate,
            facts_digest,
        })
    }

    fn validate(
        &self,
        artifact: &ArtifactManifest,
        profile: &DeviceProfile,
        probe: &ProviderProbe,
    ) -> Result<ValidationReport, ProviderError> {
        let mut report = ValidationReport::default();

        let format_id = self.artifact_format().id;
        if artifact.format.id != format_id {
            report.violation(
                "RK-V01",
                format!(
                    "artifact format {} is not {}",
                    artifact.format.id, format_id
                ),
            );
        }
        if !profile.artifact_formats.contains(&artifact.format.id) {
            report.violation(
                "RK-V02",
                format!(
                    "profile {} does not accept artifact format {}",
                    profile.id, artifact.format.id
                ),
            );
        }

        // Three-way agreement: profile allowlist, artifact partition table and
        // artifact members (architecture.md 16.3).
        let Some(table) = artifact.partition_table.as_ref() else {
            report.violation(
                "RK-V03",
                "artifact declares no partition table to agree with",
            );
            return Ok(report);
        };

        for target in &profile.allowed_targets {
            let Some(entry) = table.entry(target.partition.as_str()) else {
                report.violation(
                    "RK-V04",
                    format!(
                        "profile permits {} but the artifact's table has no such partition",
                        target.partition
                    ),
                );
                continue;
            };
            if entry.offset_sectors != target.offset_sectors {
                report.violation(
                    "RK-V05",
                    format!(
                        "{}: profile offset {} does not match the artifact table's {}",
                        target.partition, target.offset_sectors, entry.offset_sectors
                    ),
                );
            }
            let Some(member_name) = target.source_member.as_deref() else {
                report.violation(
                    "RK-V06",
                    format!("profile target {} declares no source member", target.partition),
                );
                continue;
            };
            let Some(member) = artifact.member(member_name) else {
                report.violation(
                    "RK-V07",
                    format!(
                        "profile target {} needs member {member_name}, which the artifact lacks",
                        target.partition
                    ),
                );
                continue;
            };
            if let Some(extent_sectors) = entry.size_sectors {
                let extent_bytes = extent_sectors * table.logical_block_size as u64;
                if member.size_bytes > extent_bytes {
                    report.violation(
                        "RK-V08",
                        format!(
                            "{}: member {member_name} is {} bytes but the partition holds {}",
                            target.partition, member.size_bytes, extent_bytes
                        ),
                    );
                }
            }
        }

        // A protected partition must not be reachable through any allowed
        // target, and must exist as a fact rather than a hope.
        for protected in &profile.protected_targets {
            if profile.allowed_target(protected).is_some() {
                report.violation(
                    "RK-V09",
                    format!("{protected} is both protected and allowed"),
                );
            }
        }

        // The device must be somewhere this provider can act from.
        let observed = &probe.observation.mode;
        if profile.mode(observed).is_none() {
            report.violation(
                "RK-V10",
                format!("device reports mode {observed}, which the profile does not declare"),
            );
        }

        Ok(report)
    }

    fn materialize(
        &self,
        request: &MaterializeRequest<'_>,
        maturity: &MaturityRegistry,
    ) -> Result<PlanMaterialization, ProviderError> {
        Ok(self
            .materialize_with_private_plan(request, maturity)?
            .materialization)
    }
}

/// Publishes the AF-V1 maturity states for the DAYU200 combinations.
///
/// Both are non-executable, for different reasons, and both say so in their own
/// words rather than through a missing entry:
///
/// - the fixed-tool combination is `HardwareGated`: the lowering exists, the
///   real-hardware campaign that AF-V2 requires has not run against ArkForge;
/// - the replay combination is `PlanOnly`: a transcript is not a device, and
///   no amount of replay makes it one.
pub fn publish_af_v1_maturity(
    registry: &mut MaturityRegistry,
    provider: &RockchipProvider,
    profile: &DeviceProfile,
    toolchain: &arkforge_core::identity::ToolchainIdentity,
    host_platform: &arkforge_core::identity::HostPlatform,
    driver_facts_digest: Sha256Digest,
    evidence_set_digest: Sha256Digest,
) -> Result<(), ProviderError> {
    let profile_identity = profile
        .identity()
        .map_err(|error| ProviderError::Core(error.to_string()))?;
    let key = MaturityKey {
        provider: provider.identity().clone(),
        profile: profile_identity,
        artifact_format: provider.artifact_format(),
        toolchain: toolchain.clone(),
        host_platform: host_platform.clone(),
        driver_facts_digest,
        evidence_set_digest,
    };
    let state = match toolchain.kind {
        arkforge_core::identity::ToolchainKind::Replay => MaturityState::PlanOnly {
            blocker: "transcript replay is not a device; no plan built on it may execute".into(),
        },
        _ => MaturityState::HardwareGated {
            blocker: "AF-V2 requires a real DAYU200 full-flash pass through ArkForge before this \
                      combination can be ProductionVerified (architecture.md 22 AF-V2)"
                .into(),
        },
    };
    registry.publish(&key, state);
    Ok(())
}
