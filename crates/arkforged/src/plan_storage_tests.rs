use super::*;
use crate::plan_storage;
use arkforge_core::authority::*;
use arkforge_core::digest::{CanonicalCbor, CborValue, decode_canonical, sha256};
use arkforge_core::effect::*;
use arkforge_core::identity::*;
use arkforge_core::ids::*;
use arkforge_core::plan::*;
use arkforge_core::projection::*;
use arkforge_core::step::*;
use arkforge_engine::PlanStore;
use std::path::PathBuf;

struct Root(PathBuf);
impl Root {
    fn new(label: &str) -> Self {
        let mut nonce = [0; 32];
        arkforge_platform::fill_random(&mut nonce).unwrap();
        Self(std::env::temp_dir().join(format!("af-plan-{label}-{}", sha256(&nonce).to_hex())))
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture() -> StoredPlan {
    let action = PrivateActionRecord {
        action_id: ActionId::new("ACT-1").unwrap(),
        step_id: StepId::new("STEP-1").unwrap(),
        role: PrivateActionRole::PrimaryEffect,
        effect_class: WorkflowEffect::ReadOnly,
        declared_target: Some(SemanticTarget::Device),
        declared_range: None,
        content_digest: None,
        body: CborValue::map(vec![
            ("command", CborValue::bytes(vec![0x12, 0, 0, 0])),
            (
                "arguments",
                CborValue::array(vec![CborValue::integer(-1), CborValue::Bool(true)]),
            ),
        ]),
    };
    let step = PublicFlashStep {
        step_id: action.step_id.clone(),
        kind: FlashStepKind::ProbeDevice,
        effect: WorkflowEffect::ReadOnly,
        cancellation: CancellationPolicy::CancellableImmediately,
        binding: BindingRequirement::ExactBoundTarget,
        semantic_target: action.declared_target.clone(),
        content_digest: None,
        expected_mode_before: Some(DeviceMode::new("loader").unwrap()),
        expected_mode_after: None,
        private_action_digest: action.digest().unwrap(),
    };
    let private_plan = StoredProviderPlan {
        actions: vec![action],
    };
    let effects = EffectSet::read_only();
    let projection =
        validate_projection(std::slice::from_ref(&step), &private_plan, &effects).unwrap();
    let envelope = FlashPlanEnvelope::seal(PlanSealInput {
        schema_version: PlanSchemaVersion::CURRENT,
        plan_id: PlanId::new("PLAN-STORAGE").unwrap(),
        execution_purpose: ExecutionPurpose::SupersedingRecovery,
        authority_binding: AuthorityBindingRef {
            authority_namespace: AuthorityNamespace::new("test").unwrap(),
            binding_id: OpaqueId::new("BOUND-1").unwrap(),
            binding_revision: 2,
            stable_identity_digest: sha256(b"device"),
        },
        provider: ProviderIdentity {
            id: OpaqueId::new("provider").unwrap(),
            version: Version::new(1, 2, 3),
            implementation_digest: sha256(b"provider"),
        },
        profile: DeviceProfileIdentity {
            id: OpaqueId::new("profile").unwrap(),
            version: Version::new(4, 5, 6),
            digest: sha256(b"profile"),
        },
        artifact: ArtifactIdentity {
            artifact_id: OpaqueId::new("artifact").unwrap(),
            format: ArtifactFormat {
                id: OpaqueId::new("tar").unwrap(),
                version: Version::new(1, 0, 0),
            },
            content_digest: sha256(b"archive"),
            size_bytes: 7654321,
            manifest_digest: sha256(b"manifest"),
        },
        toolchain: ToolchainIdentity {
            id: OpaqueId::new("native").unwrap(),
            kind: ToolchainKind::NativeProtocol,
            version: Version::new(2, 0, 1),
            backend_digest: sha256(b"native"),
            upstream_ref: Some("pinned-source".into()),
        },
        maturity: MaturityState::HardwareCampaign {
            campaign: "storage-regression".into(),
        },
        authority_support: AuthoritySupportBinding {
            key_digest: sha256(b"authority"),
            state: AuthoritySupportState::HardwareCampaign {
                campaign: "authority-storage".into(),
            },
        },
        negotiated_capabilities: NegotiatedCapabilities {
            entries: vec![
                (OpaqueId::new("read").unwrap(), true),
                (OpaqueId::new("write").unwrap(), false),
            ],
        },
        public_steps: vec![step],
        effect_set: effects,
        projection,
        recovery_contract: Some(RecoveryContractRef {
            id: OpaqueId::new("coverage").unwrap(),
            version: Version::new(2, 1, 3),
            digest: sha256(b"coverage"),
        }),
        postflight: PostflightPolicy {
            require_exact_target_lineage: true,
            require_mode: Some(DeviceMode::new("normal").unwrap()),
            required_runtime_facts: vec![(
                OpaqueId::new("build").unwrap(),
                "expected-build".into(),
            )],
        },
        created_at_epoch_ms: 100,
        expires_at_epoch_ms: 200,
    })
    .unwrap();
    StoredPlan {
        envelope,
        private_plan,
    }
}

#[test]
fn durable_plan_round_trip_preserves_sealed_action_bytes_and_all_bindings() {
    let root = Root::new("roundtrip");
    let original = fixture();
    {
        let mut store = open_store(&root.0).unwrap();
        store.insert(original.clone()).unwrap();
        store.insert(original.clone()).unwrap();
    }
    let loaded = open_store(&root.0).unwrap();
    let plan = loaded
        .get(&original.envelope.plan_id, original.envelope.plan_digest)
        .unwrap();
    assert_eq!(
        StoredPlan::to_storage_bytes(plan).unwrap(),
        StoredPlan::to_storage_bytes(&original).unwrap()
    );
    assert_eq!(loaded.len(), 1);
    assert!(matches!(
        loaded.get(&original.envelope.plan_id, sha256(b"wrong expectation")),
        Err(EngineError::PlanDigestMismatch { .. })
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&root.0).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(root.0.join(plan_storage::filename(plan)))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

fn set(value: &mut CborValue, key: &str, replacement: CborValue) {
    let CborValue::Map(entries) = value else {
        panic!("map expected")
    };
    entries
        .iter_mut()
        .find(|(name, _)| name == &CborValue::text(key))
        .unwrap()
        .1 = replacement;
}

#[test]
fn corrupted_or_incompatible_committed_files_fail_closed_without_rewriting_them() {
    let root = Root::new("corrupt");
    let original = fixture();
    open_store(&root.0)
        .unwrap()
        .insert(original.clone())
        .unwrap();
    let path = root.0.join(plan_storage::filename(&original));
    let bytes = std::fs::read(&path).unwrap();
    let mut bad_version = decode_canonical(&bytes).unwrap();
    set(&mut bad_version, "storageVersion", CborValue::Unsigned(2));
    let mut bad_digest = decode_canonical(&bytes).unwrap();
    set(&mut bad_digest, "planDigest", sha256(b"wrong").to_cbor());
    let mut bad_body = decode_canonical(&bytes).unwrap();
    let mut altered_action = original.private_plan.actions[0].to_cbor();
    set(
        &mut altered_action,
        "body",
        CborValue::text("different command"),
    );
    set(
        &mut bad_body,
        "privateActions",
        CborValue::array(vec![altered_action]),
    );
    let mut unknown_field = decode_canonical(&bytes).unwrap();
    if let CborValue::Map(ref mut entries) = unknown_field {
        entries.push((CborValue::text("future"), CborValue::Null));
    }
    let mut bad_schema = decode_canonical(&original.envelope.digest_body_bytes().unwrap()).unwrap();
    set(&mut bad_schema, "schemaVersion", CborValue::Unsigned(2));
    let mut unsupported_envelope = decode_canonical(&bytes).unwrap();
    set(
        &mut unsupported_envelope,
        "envelope",
        CborValue::bytes(bad_schema.to_canonical_bytes().unwrap()),
    );
    let mut cases = vec![bytes[..bytes.len() - 1].to_vec(), vec![]];
    cases.extend(
        [
            bad_version,
            bad_digest,
            bad_body,
            unknown_field,
            unsupported_envelope,
        ]
        .iter()
        .map(|value| value.to_canonical_bytes().unwrap()),
    );
    for damaged in cases {
        std::fs::write(&path, &damaged).unwrap();
        assert!(open_store(&root.0).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), damaged);
    }
    std::fs::write(&path, bytes).unwrap();
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(StoredPlan::MAX_STORAGE_BYTES + 1).unwrap();
    drop(file);
    assert!(open_store(&root.0).is_err());
}

#[test]
fn an_existing_id_cannot_be_replaced_and_a_failed_insert_is_not_visible() {
    let root = Root::new("collision");
    let original = fixture();
    let mut store = open_store(&root.0).unwrap();
    store.insert(original.clone()).unwrap();
    let path = root.0.join(plan_storage::filename(&original));
    let initial = std::fs::read(&path).unwrap();
    let mut different = original.clone();
    different.envelope.expires_at_epoch_ms += 1;
    different.envelope.plan_digest = different.envelope.recompute_digest().unwrap();
    assert!(store.insert(different.clone()).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), initial);
    let mut other_store = open_store(&root.0).unwrap();
    assert!(other_store.insert(different).is_err());
    let mut corrupt_projection = original;
    corrupt_projection.envelope.per_action_digests.clear();
    assert!(store.insert(corrupt_projection).is_err());
    assert_eq!(store.len(), 1);

    let broken = Root::new("unwritable");
    let mut empty = open_store(&broken.0).unwrap();
    std::fs::remove_dir(&broken.0).unwrap();
    std::fs::write(&broken.0, b"not a directory").unwrap();
    assert!(empty.insert(fixture()).is_err());
    assert!(empty.is_empty());
    std::fs::remove_file(&broken.0).unwrap();
}

#[test]
fn abandoned_temporary_bytes_are_never_adopted_and_filenames_are_bound() {
    let root = Root::new("interrupted");
    let original = fixture();
    open_store(&root.0).unwrap();
    std::fs::write(
        root.0.join("abandoned.pending"),
        StoredPlan::to_storage_bytes(&original).unwrap(),
    )
    .unwrap();
    assert!(open_store(&root.0).unwrap().is_empty());
    std::fs::write(
        root.0.join("wrong.cbor"),
        StoredPlan::to_storage_bytes(&original).unwrap(),
    )
    .unwrap();
    assert!(open_store(&root.0).is_err());
}

#[cfg(unix)]
#[test]
fn plan_store_does_not_follow_symbolic_links() {
    let root = Root::new("symlink");
    let other = Root::new("outside");
    open_store(&root.0).unwrap();
    std::fs::create_dir(&other.0).unwrap();
    let original = fixture();
    let outside = other.0.join("outside.cbor");
    std::fs::write(&outside, StoredPlan::to_storage_bytes(&original).unwrap()).unwrap();
    std::os::unix::fs::symlink(&outside, root.0.join(plan_storage::filename(&original))).unwrap();
    assert!(open_store(&root.0).is_err());
}

fn open_store(root: &std::path::Path) -> Result<PlanStore, EngineError> {
    PlanStore::open(FilePlanStorage::new(root))
}
