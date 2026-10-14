//! Private, versioned storage encoding. This is never an IPC representation.
//!
//! Decode the sealed bytes, rather than invoking a provider to lower them again.
//! A canonical round trip rejects unknown fields and incompatible enum shapes.

use crate::StoredPlan;
use arkforge_core::Sha256Digest;
use arkforge_core::authority::*;
use arkforge_core::digest::{CanonicalCbor, CborValue, decode_canonical};
use arkforge_core::effect::*;
use arkforge_core::identity::*;
use arkforge_core::ids::*;
use arkforge_core::plan::*;
use arkforge_core::projection::*;
use arkforge_core::step::*;

type Result<T> = std::result::Result<T, String>;

pub(crate) const MAX_BYTES: u64 = 8 * 1024 * 1024;

fn checked<T, E: std::fmt::Display>(value: std::result::Result<T, E>) -> Result<T> {
    value.map_err(|error| error.to_string())
}

pub(crate) fn validate(plan: &StoredPlan) -> Result<()> {
    if plan.envelope.schema_version != PlanSchemaVersion::CURRENT {
        return Err("unsupported sealed plan schema".into());
    }
    checked(plan.envelope.verify_self_digest())?;
    checked(plan.envelope.effect_set.validate_executable())?;
    if plan.envelope.expires_at_epoch_ms <= plan.envelope.created_at_epoch_ms {
        return Err("sealed plan expiry precedes creation".into());
    }
    let projection = checked(validate_projection(
        &plan.envelope.public_steps,
        &plan.private_plan,
        &plan.envelope.effect_set,
    ))?;
    if projection.per_action != plan.envelope.per_action_digests
        || projection.provider_execution_plan_digest != plan.envelope.provider_execution_plan_digest
        || projection.public_projection_digest != plan.envelope.public_projection_digest
    {
        return Err("stored private plan does not match the sealed projection".into());
    }
    Ok(())
}

pub(crate) fn encode(plan: &StoredPlan) -> Result<Vec<u8>> {
    validate(plan)?;
    let bytes = checked(
        CborValue::map(vec![
            ("storageVersion", CborValue::Unsigned(1)),
            ("planDigest", plan.envelope.plan_digest.to_cbor()),
            (
                "envelope",
                CborValue::bytes(checked(plan.envelope.digest_body_bytes())?),
            ),
            (
                "privateActions",
                CborValue::array(
                    plan.private_plan
                        .actions
                        .iter()
                        .map(CanonicalCbor::to_cbor)
                        .collect(),
                ),
            ),
        ])
        .to_canonical_bytes(),
    )?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("sealed plan exceeds the storage size bound".into());
    }
    Ok(bytes)
}

pub(crate) fn decode(bytes: &[u8]) -> Result<StoredPlan> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err("sealed plan exceeds the storage size bound".into());
    }
    let storage = checked(decode_canonical(bytes))?;
    if uint(field(&storage, "storageVersion")?)? != 1 {
        return Err("unsupported private plan storage version".into());
    }
    let body_bytes = byte_string(field(&storage, "envelope")?)?;
    let body = checked(decode_canonical(body_bytes))?;
    let private_plan = StoredProviderPlan {
        actions: list(field(&storage, "privateActions")?, action)?,
    };
    let public_steps = list(field(&body, "publicSteps")?, step)?;
    let effect_set = effects(field(&body, "effectSet")?)?;
    let projection = checked(validate_projection(
        &public_steps,
        &private_plan,
        &effect_set,
    ))?;
    if projection.provider_execution_plan_digest
        != digest(field(&body, "providerExecutionPlanDigest")?)?
        || projection.public_projection_digest != digest(field(&body, "publicProjectionDigest")?)?
    {
        return Err("private actions do not match sealed projection digests".into());
    }
    let schema_version = PlanSchemaVersion(u32_value(field(&body, "schemaVersion")?)?);
    if schema_version != PlanSchemaVersion::CURRENT {
        return Err("unsupported sealed plan schema".into());
    }
    let envelope = checked(FlashPlanEnvelope::seal(PlanSealInput {
        schema_version,
        plan_id: checked(PlanId::new(text(field(&body, "planId")?)?))?,
        execution_purpose: ExecutionPurpose::parse(text(field(&body, "executionPurpose")?)?)
            .ok_or("unknown execution purpose")?,
        authority_binding: binding(field(&body, "authorityBinding")?)?,
        provider: provider(field(&body, "provider")?)?,
        profile: profile(field(&body, "profile")?)?,
        artifact: artifact(field(&body, "artifact")?)?,
        toolchain: toolchain(field(&body, "toolchain")?)?,
        maturity: maturity(field(&body, "maturity")?)?,
        authority_support: authority_support(field(&body, "authoritySupport")?)?,
        negotiated_capabilities: NegotiatedCapabilities {
            entries: map(field(&body, "negotiatedCapabilities")?)?
                .iter()
                .map(|(key, value)| Ok((opaque(key)?, boolean(value)?)))
                .collect::<Result<_>>()?,
        },
        public_steps,
        effect_set,
        projection,
        recovery_contract: optional(field(&body, "recoveryContract")?, recovery_contract)?,
        postflight: postflight(field(&body, "postflight")?)?,
        created_at_epoch_ms: uint(field(&body, "createdAtEpochMs")?)?,
        expires_at_epoch_ms: uint(field(&body, "expiresAtEpochMs")?)?,
    }))?;
    if envelope.plan_digest != digest(field(&storage, "planDigest")?)?
        || checked(envelope.digest_body_bytes())? != body_bytes
    {
        return Err("sealed envelope bytes or digest do not match".into());
    }
    let stored = StoredPlan {
        envelope,
        private_plan,
    };
    if encode(&stored)? != bytes {
        return Err("unsupported or inconsistent private plan storage fields".into());
    }
    Ok(stored)
}

fn field<'a>(value: &'a CborValue, key: &str) -> Result<&'a CborValue> {
    map(value)?
        .iter()
        .find_map(|(name, value)| (name == &CborValue::text(key)).then_some(value))
        .ok_or_else(|| format!("missing sealed field {key}"))
}
fn map(value: &CborValue) -> Result<&[(CborValue, CborValue)]> {
    match value {
        CborValue::Map(items) => Ok(items),
        _ => Err("expected map".into()),
    }
}
fn array(value: &CborValue) -> Result<&[CborValue]> {
    match value {
        CborValue::Array(items) => Ok(items),
        _ => Err("expected array".into()),
    }
}
fn list<T>(value: &CborValue, decode: impl Fn(&CborValue) -> Result<T>) -> Result<Vec<T>> {
    array(value)?.iter().map(decode).collect()
}
fn text(value: &CborValue) -> Result<&str> {
    match value {
        CborValue::Text(text) => Ok(text),
        _ => Err("expected text".into()),
    }
}
fn byte_string(value: &CborValue) -> Result<&[u8]> {
    match value {
        CborValue::Bytes(bytes) => Ok(bytes),
        _ => Err("expected bytes".into()),
    }
}
fn uint(value: &CborValue) -> Result<u64> {
    match value {
        CborValue::Unsigned(number) => Ok(*number),
        _ => Err("expected unsigned integer".into()),
    }
}
fn u32_value(value: &CborValue) -> Result<u32> {
    checked(u32::try_from(uint(value)?))
}
fn boolean(value: &CborValue) -> Result<bool> {
    match value {
        CborValue::Bool(value) => Ok(*value),
        _ => Err("expected boolean".into()),
    }
}
fn optional<T>(
    value: &CborValue,
    decode: impl FnOnce(&CborValue) -> Result<T>,
) -> Result<Option<T>> {
    if value == &CborValue::Null {
        Ok(None)
    } else {
        decode(value).map(Some)
    }
}
fn digest(value: &CborValue) -> Result<Sha256Digest> {
    Ok(Sha256Digest::from_bytes(
        byte_string(value)?
            .try_into()
            .map_err(|_| "digest must be 32 bytes")?,
    ))
}
fn opaque(value: &CborValue) -> Result<OpaqueId> {
    checked(OpaqueId::new(text(value)?))
}
fn version(value: &CborValue) -> Result<Version> {
    let [major, minor, patch] = array(value)? else {
        return Err("version must have three components".into());
    };
    Ok(Version::new(
        u32_value(major)?,
        u32_value(minor)?,
        u32_value(patch)?,
    ))
}
fn mode(value: &CborValue) -> Result<DeviceMode> {
    checked(DeviceMode::new(text(value)?))
}
fn range(value: &CborValue) -> Result<ByteRange> {
    checked(ByteRange::new(
        uint(field(value, "start")?)?,
        uint(field(value, "length")?)?,
    ))
}
fn boot_field(value: &CborValue) -> Result<BootMetadataField> {
    match text(value)? {
        "activeSlot" => Ok(BootMetadataField::ActiveSlot),
        "bootCount" => Ok(BootMetadataField::BootCount),
        "updaterCommand" => Ok(BootMetadataField::UpdaterCommand),
        "lockState" => Ok(BootMetadataField::LockState),
        _ => Err("unknown boot metadata field".into()),
    }
}
fn target(value: &CborValue) -> Result<SemanticTarget> {
    let [(key, value)] = map(value)? else {
        return Err("semantic target must have one field".into());
    };
    match text(key)? {
        "partition" => Ok(SemanticTarget::Partition(checked(PartitionId::new(text(
            value,
        )?))?)),
        "rawRegion" => Ok(SemanticTarget::RawRegion(checked(RegionId::new(text(
            value,
        )?))?)),
        "bootMetadata" => Ok(SemanticTarget::BootMetadata(boot_field(value)?)),
        "device" if value == &CborValue::Null => Ok(SemanticTarget::Device),
        _ => Err("unknown semantic target".into()),
    }
}
fn workflow_effect(value: &CborValue) -> Result<WorkflowEffect> {
    WorkflowEffect::parse(text(value)?).ok_or_else(|| "unknown workflow effect".into())
}
fn step(value: &CborValue) -> Result<PublicFlashStep> {
    Ok(PublicFlashStep {
        step_id: checked(StepId::new(text(field(value, "stepId")?)?))?,
        kind: FlashStepKind::parse(text(field(value, "kind")?)?).ok_or("unknown step kind")?,
        effect: workflow_effect(field(value, "effect")?)?,
        cancellation: CancellationPolicy::parse(text(field(value, "cancellation")?)?)
            .ok_or("unknown cancellation policy")?,
        binding: BindingRequirement::parse(text(field(value, "binding")?)?)
            .ok_or("unknown binding requirement")?,
        semantic_target: optional(field(value, "semanticTarget")?, target)?,
        content_digest: optional(field(value, "contentDigest")?, digest)?,
        expected_mode_before: optional(field(value, "expectedModeBefore")?, mode)?,
        expected_mode_after: optional(field(value, "expectedModeAfter")?, mode)?,
        private_action_digest: digest(field(value, "privateActionDigest")?)?,
    })
}
fn action(value: &CborValue) -> Result<PrivateActionRecord> {
    Ok(PrivateActionRecord {
        action_id: checked(ActionId::new(text(field(value, "actionId")?)?))?,
        step_id: checked(StepId::new(text(field(value, "stepId")?)?))?,
        role: match text(field(value, "role")?)? {
            "primaryEffect" => PrivateActionRole::PrimaryEffect,
            "readOnlyTransportSubAction" => PrivateActionRole::ReadOnlyTransportSubAction,
            _ => return Err("unknown action role".into()),
        },
        effect_class: workflow_effect(field(value, "effectClass")?)?,
        declared_target: optional(field(value, "declaredTarget")?, target)?,
        declared_range: optional(field(value, "declaredRange")?, range)?,
        content_digest: optional(field(value, "contentDigest")?, digest)?,
        body: field(value, "body")?.clone(),
    })
}
fn binding(value: &CborValue) -> Result<AuthorityBindingRef> {
    Ok(AuthorityBindingRef {
        authority_namespace: checked(AuthorityNamespace::new(text(field(
            value,
            "authorityNamespace",
        )?)?))?,
        binding_id: opaque(field(value, "bindingId")?)?,
        binding_revision: uint(field(value, "bindingRevision")?)?,
        stable_identity_digest: digest(field(value, "stableIdentityDigest")?)?,
    })
}
fn provider(value: &CborValue) -> Result<ProviderIdentity> {
    Ok(ProviderIdentity {
        id: opaque(field(value, "id")?)?,
        version: version(field(value, "version")?)?,
        implementation_digest: digest(field(value, "implementationDigest")?)?,
    })
}
fn profile(value: &CborValue) -> Result<DeviceProfileIdentity> {
    Ok(DeviceProfileIdentity {
        id: opaque(field(value, "id")?)?,
        version: version(field(value, "version")?)?,
        digest: digest(field(value, "digest")?)?,
    })
}
fn artifact(value: &CborValue) -> Result<ArtifactIdentity> {
    let format = field(value, "format")?;
    Ok(ArtifactIdentity {
        artifact_id: opaque(field(value, "artifactId")?)?,
        format: ArtifactFormat {
            id: opaque(field(format, "id")?)?,
            version: version(field(format, "version")?)?,
        },
        content_digest: digest(field(value, "contentDigest")?)?,
        size_bytes: uint(field(value, "sizeBytes")?)?,
        manifest_digest: digest(field(value, "manifestDigest")?)?,
    })
}
fn toolchain(value: &CborValue) -> Result<ToolchainIdentity> {
    Ok(ToolchainIdentity {
        id: opaque(field(value, "id")?)?,
        kind: ToolchainKind::parse(text(field(value, "kind")?)?).ok_or("unknown toolchain kind")?,
        version: version(field(value, "version")?)?,
        backend_digest: digest(field(value, "backendDigest")?)?,
        upstream_ref: optional(field(value, "upstreamRef")?, |v| Ok(text(v)?.to_string()))?,
    })
}
fn maturity(value: &CborValue) -> Result<MaturityState> {
    let blocker = || -> Result<String> { Ok(text(field(value, "blocker")?)?.to_string()) };
    Ok(match text(field(value, "state")?)? {
        "productionVerified" => MaturityState::ProductionVerified,
        "hardwareCampaign" => MaturityState::HardwareCampaign {
            campaign: text(field(value, "campaign")?)?.to_string(),
        },
        "hardwareGated" => MaturityState::HardwareGated {
            blocker: blocker()?,
        },
        "planOnly" => MaturityState::PlanOnly {
            blocker: blocker()?,
        },
        "researchOnly" => MaturityState::ResearchOnly {
            blocker: blocker()?,
        },
        "unavailable" => MaturityState::Unavailable { reason: blocker()? },
        _ => return Err("unknown maturity state".into()),
    })
}
fn authority_support(value: &CborValue) -> Result<AuthoritySupportBinding> {
    let state = field(value, "state")?;
    Ok(AuthoritySupportBinding {
        key_digest: digest(field(value, "keyDigest")?)?,
        state: match text(field(state, "state")?)? {
            "productionVerified" => AuthoritySupportState::ProductionVerified,
            "hardwareCampaign" => AuthoritySupportState::HardwareCampaign {
                campaign: text(field(state, "campaign")?)?.to_string(),
            },
            "hardwareGated" => AuthoritySupportState::HardwareGated {
                blocker: text(field(state, "blocker")?)?.to_string(),
            },
            _ => return Err("unknown authority support state".into()),
        },
    })
}
fn recovery_contract(value: &CborValue) -> Result<RecoveryContractRef> {
    Ok(RecoveryContractRef {
        id: opaque(field(value, "id")?)?,
        version: version(field(value, "version")?)?,
        digest: digest(field(value, "digest")?)?,
    })
}
fn postflight(value: &CborValue) -> Result<PostflightPolicy> {
    Ok(PostflightPolicy {
        require_exact_target_lineage: boolean(field(value, "requireExactTargetLineage")?)?,
        require_mode: optional(field(value, "requireMode")?, mode)?,
        required_runtime_facts: map(field(value, "requiredRuntimeFacts")?)?
            .iter()
            .map(|(key, value)| Ok((opaque(key)?, text(value)?.to_string())))
            .collect::<Result<_>>()?,
    })
}
fn impact(value: &CborValue) -> Result<DataImpactState> {
    match text(value)? {
        "preserved" => Ok(DataImpactState::Preserved),
        "overwritten" => Ok(DataImpactState::Overwritten),
        "unknown" => Ok(DataImpactState::Unknown),
        _ => Err("unknown data impact".into()),
    }
}
fn effects(value: &CborValue) -> Result<EffectSet> {
    let data = field(value, "dataImpact")?;
    Ok(EffectSet {
        persistent: list(field(value, "persistent")?, persistent)?,
        transient: list(field(value, "transient")?, transient)?,
        data_impact: DataImpact {
            userdata: impact(field(data, "userdata")?)?,
            calibration: impact(field(data, "calibration")?)?,
            non_volatile_config: impact(field(data, "nonVolatileConfig")?)?,
            secure_storage: impact(field(data, "secureStorage")?)?,
        },
    })
}
fn typed_value(value: &CborValue) -> Result<TypedValue> {
    let [(key, value)] = map(value)? else {
        return Err("typed value must have one field".into());
    };
    match text(key)? {
        "text" => Ok(TypedValue::Text(text(value)?.to_string())),
        "bool" => Ok(TypedValue::Bool(boolean(value)?)),
        "integer" => Ok(TypedValue::Integer(match value {
            CborValue::Negative(n) => *n,
            CborValue::Unsigned(n) => checked(i64::try_from(*n))?,
            _ => return Err("expected integer".into()),
        })),
        _ => Err("unknown typed value".into()),
    }
}
fn persistent(value: &CborValue) -> Result<PersistentEffect> {
    Ok(match text(field(value, "kind")?)? {
        "erasePartition" => PersistentEffect::ErasePartition {
            partition: checked(PartitionId::new(text(field(value, "partition")?)?))?,
            range: range(field(value, "range")?)?,
        },
        "writePartition" => PersistentEffect::WritePartition {
            partition: checked(PartitionId::new(text(field(value, "partition")?)?))?,
            range: range(field(value, "range")?)?,
            content: digest(field(value, "content")?)?,
        },
        "writeRawRegion" => PersistentEffect::WriteRawRegion {
            region: checked(RegionId::new(text(field(value, "region")?)?))?,
            range: range(field(value, "range")?)?,
            content: digest(field(value, "content")?)?,
        },
        "replacePartitionTable" => PersistentEffect::ReplacePartitionTable {
            layout_digest: digest(field(value, "layoutDigest")?)?,
        },
        "changeBootMetadata" => PersistentEffect::ChangeBootMetadata {
            field: boot_field(field(value, "field")?)?,
            expected_value: typed_value(field(value, "expectedValue")?)?,
        },
        _ => return Err("unknown persistent effect".into()),
    })
}
fn transient(value: &CborValue) -> Result<TransientEffect> {
    Ok(match text(field(value, "kind")?)? {
        "enterMode" => TransientEffect::EnterMode {
            from: mode(field(value, "from")?)?,
            to: mode(field(value, "to")?)?,
        },
        "loadEphemeralAgent" => {
            let region = field(value, "memoryRegion")?;
            TransientEffect::LoadEphemeralAgent {
                stage: checked(AgentStage::new(text(field(value, "stage")?)?))?,
                memory_region: MemoryRegion {
                    base_address: uint(field(region, "baseAddress")?)?,
                    length: uint(field(region, "length")?)?,
                },
                content: digest(field(value, "content")?)?,
            }
        }
        "usbDetachReattach" => TransientEffect::UsbDetachReattach {
            expectation_digest: digest(field(value, "expectationDigest")?)?,
        },
        "reboot" => TransientEffect::Reboot {
            target_mode: mode(field(value, "targetMode")?)?,
        },
        _ => return Err("unknown transient effect".into()),
    })
}
