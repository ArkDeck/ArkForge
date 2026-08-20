//! The DAYU200 provider vertical, end to end:
//!
//! ```text
//! artifact import → inspect → profile validation → discover/probe
//!   → public/private plan materialization → plan/effect parity
//! ```
//!
//! It covers both assessment-only combinations and the executable native plan.

use arkforge_artifact::cas::{CasQuota, ContentAddressedStore, VolumeSpaceProbe};
use arkforge_artifact::{dayu200, fixture};
use arkforge_core::digest::{CborValue, Domain, digest_canonical, sha256};
use arkforge_core::effect::{DataImpactState, DeviceMode, PersistentEffect, TransientEffect};
use arkforge_core::identity::{
    HostPlatform, MaturityKey, MaturityState, ToolchainIdentity, ToolchainKind, Version,
};
use arkforge_core::ids::{OpaqueId, PartitionId, PlanId};
use arkforge_core::plan::ExecutionPurpose;
use arkforge_core::profile;
use arkforge_core::projection::{PrivateActionRole, validate_projection};
use arkforge_core::step::{FlashStepKind, SemanticTarget, WorkflowEffect};
use arkforge_core::{
    AuthorityBindingRef, AuthorityNamespace, AuthoritySupportBinding, AuthoritySupportState,
};
use arkforge_provider::rockchip::{RockchipProvider, publish_af_v1_maturity};
use arkforge_provider::{
    FlashIntent, FlashProvider, MaterializeRequest, MaturityRegistry, ProbeContext,
};
use arkforge_transport::replay::TranscriptTransport;
use arkforge_transport::{DeviceTransport, TypedDiscoveryFilter, transcript};
use std::io;
use std::path::{Path, PathBuf};

const PROFILE_SOURCE: &str = include_str!("../../../profiles/dayu200.yaml");
const CAMPAIGN: &str = include_str!("../../../transcripts/dayu200-gj4-ecamp-96effff15.yaml");

#[derive(Debug)]
struct AmpleSpace;

impl VolumeSpaceProbe for AmpleSpace {
    fn available_bytes(&self, _path: &Path) -> io::Result<u64> {
        Ok(64 * 1024 * 1024 * 1024)
    }
}

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "arkforge-vertical-{}-{}-{:?}",
            name,
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        TempRoot(path)
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn native_tool() -> ToolchainIdentity {
    ToolchainIdentity {
        id: OpaqueId::new("arkforged-native-rockusb").unwrap(),
        kind: ToolchainKind::NativeProtocol,
        version: Version::new(0, 1, 0),
        backend_digest: sha256(b"native arkforged build"),
        upstream_ref: None,
    }
}

fn replay_tool() -> ToolchainIdentity {
    ToolchainIdentity {
        id: OpaqueId::new("transcript-replay").unwrap(),
        kind: ToolchainKind::Replay,
        version: Version::new(1, 0, 0),
        backend_digest: sha256(CAMPAIGN.as_bytes()),
        upstream_ref: None,
    }
}

fn binding() -> AuthorityBindingRef {
    AuthorityBindingRef {
        authority_namespace: AuthorityNamespace::new("arkdeck").unwrap(),
        binding_id: OpaqueId::new("TGT-958780b2ffb7").unwrap(),
        binding_revision: 2,
        stable_identity_digest: sha256(b"dayu200-gj4"),
    }
}

/// Runs the read-only vertical and returns everything it produced.
struct Vertical {
    manifest: arkforge_artifact::manifest::ArtifactManifest,
    profile: profile::DeviceProfile,
    probe: arkforge_provider::ProviderProbe,
    provider: RockchipProvider,
}

fn run_vertical(root: &TempRoot) -> Vertical {
    let store = ContentAddressedStore::open_with_probe(
        &root.0,
        CasQuota::dayu200_default(),
        Box::new(AmpleSpace),
    )
    .unwrap();

    // 1. import
    let archive = fixture::dayu200_archive();
    let imported = store
        .import(archive.as_slice(), archive.len() as u64, None)
        .unwrap();
    store.acquire_lease(&imported.digest, "PLAN-001").unwrap();

    // 2. inspect — from the store, never from a caller path
    let manifest = dayu200::inspect(store.open_object(&imported.digest).unwrap()).unwrap();

    // 3. profile
    let profile = profile::load(PROFILE_SOURCE).unwrap();

    // 4. discover / probe, over the recorded campaign
    let transport = TranscriptTransport::new(transcript::parse(CAMPAIGN).unwrap());
    let observations = transport
        .discover(&TypedDiscoveryFilter::default(), 0)
        .unwrap();
    assert_eq!(observations.len(), 1);
    let provider = RockchipProvider::new();
    let probe = provider
        .probe(&ProbeContext {
            transport: &transport,
            observation: &observations[0],
            profile: &profile,
        })
        .unwrap();

    Vertical {
        manifest,
        profile,
        probe,
        provider,
    }
}

fn request<'a>(vertical: &'a Vertical, toolchain: ToolchainIdentity) -> MaterializeRequest<'a> {
    MaterializeRequest {
        plan_id: PlanId::new("PLAN-001").unwrap(),
        execution_purpose: ExecutionPurpose::PrimaryFlash,
        intent: FlashIntent::FullRestore,
        artifact: &vertical.manifest,
        artifact_id: OpaqueId::new("ART-001").unwrap(),
        profile: &vertical.profile,
        probe: &vertical.probe,
        authority_binding: binding(),
        authority_support: AuthoritySupportBinding {
            key_digest: sha256(b"test authority support"),
            state: AuthoritySupportState::ProductionVerified,
        },
        toolchain,
        host_platform: HostPlatform::new("macos", "aarch64").unwrap(),
        driver_facts_digest: sha256(b"driver facts"),
        evidence_set_digest: sha256(b"AD-003,AD-005,AD-006"),
        created_at_epoch_ms: 1_754_380_800_000,
        plan_lifetime_ms: 3_600_000,
    }
}

fn af_v1_registry(vertical: &Vertical, toolchain: &ToolchainIdentity) -> MaturityRegistry {
    let mut registry = MaturityRegistry::new();
    publish_af_v1_maturity(
        &mut registry,
        &vertical.provider,
        &vertical.profile,
        toolchain,
        &HostPlatform::new("macos", "aarch64").unwrap(),
        sha256(b"driver facts"),
        sha256(b"AD-003,AD-005,AD-006"),
    )
    .unwrap();
    registry
}

/// A registry that pretends the AF-V2 hardware campaign already passed.
///
/// It exists to exercise the executable branch of materialization; it is a test
/// double and nothing more. Publishing this state for real requires a real
/// DAYU200 pass (architecture.md 22 AF-V2).
fn hypothetical_production_registry(
    vertical: &Vertical,
    toolchain: &ToolchainIdentity,
) -> MaturityRegistry {
    let mut registry = MaturityRegistry::new();
    registry.publish(
        &MaturityKey {
            provider: vertical.provider.identity().clone(),
            profile: vertical.profile.identity().unwrap(),
            artifact_format: vertical.provider.descriptor().artifact_formats[0].clone(),
            toolchain: toolchain.clone(),
            host_platform: HostPlatform::new("macos", "aarch64").unwrap(),
            driver_facts_digest: sha256(b"driver facts"),
            evidence_set_digest: sha256(b"AD-003,AD-005,AD-006"),
        },
        MaturityState::ProductionVerified,
    );
    registry
}

#[test]
fn validation_is_clean_against_the_pinned_artifact_and_profile() {
    let root = TempRoot::new("validate");
    let vertical = run_vertical(&root);
    let report = vertical
        .provider
        .validate(&vertical.manifest, &vertical.profile, &vertical.probe)
        .unwrap();
    assert!(
        report.is_clean(),
        "three-way agreement should hold: {:?}",
        report.violations
    );
}

#[test]
fn af_v1_materializes_a_complete_plan_that_is_not_executable() {
    let root = TempRoot::new("assessment");
    let vertical = run_vertical(&root);
    let toolchain = native_tool();
    let registry = af_v1_registry(&vertical, &toolchain);

    let built = vertical
        .provider
        .materialize_with_private_plan(&request(&vertical, toolchain), &registry)
        .unwrap();

    // Not executable — and the reason is stated, not implied.
    let assessment = built
        .materialization
        .assessment()
        .expect("AF-V1 must not produce an executable plan for a hardware-gated combination");
    assert!(
        assessment
            .unknowns
            .iter()
            .any(|unknown| unknown.summary.contains("AF-V2"))
    );
    assert!(matches!(
        assessment.availability,
        arkforge_core::plan::ExecutionAvailability::Unavailable { .. }
    ));
    assert_eq!(
        assessment.evidence_requirements.len(),
        assessment.unknowns.len(),
        "every unknown names what would close it"
    );

    // …but the effect set is complete, so an operator can see exactly what a
    // future execution would do.
    assert_eq!(assessment.known_effects.persistent.len(), 9);
    assert_eq!(
        assessment.known_effects.data_impact.userdata,
        DataImpactState::Overwritten
    );
    // And the private plan exists and projects, so the assessment is auditable.
    assert!(built.private_plan.is_some());
}

#[test]
fn a_replay_toolchain_can_never_produce_an_executable_plan() {
    let root = TempRoot::new("replay");
    let vertical = run_vertical(&root);
    let toolchain = replay_tool();
    let registry = af_v1_registry(&vertical, &toolchain);
    let materialization = vertical
        .provider
        .materialize(&request(&vertical, toolchain), &registry)
        .unwrap();
    let assessment = materialization.assessment().expect("replay is plan-only");
    assert!(assessment.unknowns.iter().any(|unknown| {
        unknown
            .summary
            .contains("transcript replay is not a device")
    }));
}

#[test]
fn the_executable_branch_produces_a_fully_projected_sealed_plan() {
    let root = TempRoot::new("executable");
    let vertical = run_vertical(&root);
    let toolchain = native_tool();
    let registry = hypothetical_production_registry(&vertical, &toolchain);

    let built = vertical
        .provider
        .materialize_with_private_plan(&request(&vertical, toolchain), &registry)
        .unwrap();
    let plan = built
        .materialization
        .executable()
        .expect("a ProductionVerified combination yields an executable plan");
    let private_plan = built.private_plan.as_ref().unwrap();

    plan.verify_self_digest().unwrap();
    let recovery = plan
        .recovery_contract
        .as_ref()
        .expect("the executable complete-overwrite recipe publishes its coverage");
    assert_eq!(recovery.id.as_str(), "arkforge.rockchip.complete-overwrite");
    assert_eq!(recovery.version, Version::new(1, 0, 0));
    assert_eq!(
        recovery.digest,
        digest_canonical(Domain::RecoveryCoverage, &vertical.profile.recovery).unwrap()
    );

    // 1 ensure-mode + 1 probe + 1 validate-layout + 9 writes + 9 verifies
    // + 1 reboot + 1 postflight.
    assert_eq!(plan.public_steps.len(), 23);
    let kinds: Vec<FlashStepKind> = plan.public_steps.iter().map(|step| step.kind).collect();
    assert_eq!(kinds[0], FlashStepKind::EnsureMode);
    assert_eq!(kinds[1], FlashStepKind::ProbeDevice);
    assert_eq!(kinds[2], FlashStepKind::ValidateLayout);
    assert_eq!(kinds[3..12], [FlashStepKind::WriteTarget; 9]);
    assert_eq!(kinds[12..21], [FlashStepKind::VerifyTarget; 9]);
    assert_eq!(kinds[21], FlashStepKind::Reboot);
    assert_eq!(kinds[22], FlashStepKind::PostflightProbe);

    // Every private action is covered by a digest that crosses the boundary
    // (AF-V1 acceptance: "private action digest 覆盖").
    assert_eq!(plan.per_action_digests.len(), private_plan.actions.len());
    for action in &private_plan.actions {
        let digest = action.digest().unwrap();
        assert!(
            plan.per_action_digests
                .iter()
                .any(|binding| binding.private_action_digest == digest),
            "action {} is not covered by the plan's digests",
            action.action_id
        );
    }

    // Re-running the projection over the sealed plan reproduces its digests.
    let projection =
        validate_projection(&plan.public_steps, private_plan, &plan.effect_set).unwrap();
    assert_eq!(
        projection.provider_execution_plan_digest,
        plan.provider_execution_plan_digest
    );
    assert_eq!(
        projection.public_projection_digest,
        plan.public_projection_digest
    );
}

#[test]
fn a_plan_materialized_in_loader_starts_with_the_exact_loader_probe() {
    let root = TempRoot::new("loader-start");
    let vertical = run_vertical(&root);
    let toolchain = native_tool();
    let registry = hypothetical_production_registry(&vertical, &toolchain);
    let mut loader_probe = vertical.probe.clone();
    loader_probe.observation.mode = DeviceMode::new("rockusb-loader").unwrap();
    let mut loader_request = request(&vertical, toolchain);
    loader_request.probe = &loader_probe;

    let built = vertical
        .provider
        .materialize_with_private_plan(&loader_request, &registry)
        .unwrap();
    let plan = built
        .materialization
        .executable()
        .expect("the exact Loader observation is executable");

    assert_eq!(plan.public_steps.len(), 22);
    assert_eq!(plan.public_steps[0].kind, FlashStepKind::ProbeDevice);
    assert_eq!(
        plan.public_steps[0]
            .expected_mode_before
            .as_ref()
            .map(|mode| mode.as_str()),
        Some("rockusb-loader")
    );
    assert!(
        !plan
            .effect_set
            .transient
            .iter()
            .any(|effect| matches!(effect, TransientEffect::EnterMode { .. })),
        "an already-satisfied mode transition must not remain in the sealed effect set"
    );
    validate_projection(
        &plan.public_steps,
        built.private_plan.as_ref().unwrap(),
        &plan.effect_set,
    )
    .unwrap();
}

#[test]
fn the_effect_set_matches_the_profile_and_the_artifact() {
    let root = TempRoot::new("effects");
    let vertical = run_vertical(&root);
    let toolchain = native_tool();
    let registry = hypothetical_production_registry(&vertical, &toolchain);
    let plan = vertical
        .provider
        .materialize(&request(&vertical, toolchain), &registry)
        .unwrap();
    let plan = plan.executable().unwrap();

    // Nine writes, at the profile's offsets, with the artifact's hashes.
    let expected: [(&str, u64, &str); 9] = [
        ("uboot", 8192, "uboot.img"),
        ("resource", 28672, "resource.img"),
        ("boot_linux", 40960, "boot_linux.img"),
        ("ramdisk", 237_568, "ramdisk.img"),
        ("system", 245_760, "system.img"),
        ("vendor", 4_440_064, "vendor.img"),
        ("updater", 6_742_016, "updater.img"),
        ("chip_ckm", 6_938_624, "chip_ckm.img"),
        ("userdata", 19_955_712, "userdata.img"),
    ];
    assert_eq!(plan.effect_set.persistent.len(), expected.len());
    for (index, (partition, offset_sectors, member)) in expected.into_iter().enumerate() {
        match &plan.effect_set.persistent[index] {
            PersistentEffect::WritePartition {
                partition: observed,
                range,
                content,
            } => {
                assert_eq!(observed.as_str(), partition);
                assert_eq!(range.start, offset_sectors * 512, "{partition} start");
                let fact = vertical.manifest.member(member).unwrap();
                assert_eq!(range.length, fact.size_bytes, "{partition} length");
                assert_eq!(*content, fact.sha256, "{partition} content");
            }
            other => panic!("unexpected effect {other:?}"),
        }
    }

    // Two transient effects: the mode change in, and the reboot out.
    assert_eq!(plan.effect_set.transient.len(), 2);
    assert!(matches!(
        plan.effect_set.transient[0],
        TransientEffect::EnterMode { .. }
    ));
    assert!(matches!(
        plan.effect_set.transient[1],
        TransientEffect::Reboot { .. }
    ));

    // No protected partition appears anywhere in the effect set.
    for protected in &vertical.profile.protected_targets {
        assert!(
            !plan
                .effect_set
                .persistent
                .iter()
                .any(|effect| effect.partition() == Some(protected)),
            "{protected} is protected and must not be written"
        );
    }
}

#[test]
fn superseding_recovery_is_a_distinct_sealed_plan_not_a_primary_replay() {
    let root = TempRoot::new("recovery-purpose");
    let vertical = run_vertical(&root);
    let toolchain = native_tool();
    let registry = hypothetical_production_registry(&vertical, &toolchain);

    let primary = vertical
        .provider
        .materialize(&request(&vertical, toolchain.clone()), &registry)
        .unwrap();
    let mut recovery_request = request(&vertical, toolchain);
    recovery_request.execution_purpose = ExecutionPurpose::SupersedingRecovery;
    let recovery = vertical
        .provider
        .materialize(&recovery_request, &registry)
        .unwrap();
    let primary = primary.executable().unwrap();
    let recovery = recovery.executable().unwrap();

    assert_eq!(primary.execution_purpose, ExecutionPurpose::PrimaryFlash);
    assert_eq!(
        recovery.execution_purpose,
        ExecutionPurpose::SupersedingRecovery
    );
    assert_ne!(
        primary.plan_digest, recovery.plan_digest,
        "execution purpose is part of the immutable plan identity"
    );
    assert!(
        recovery.recovery_contract.is_some(),
        "a superseding recovery plan must carry the profile's coverage contract"
    );
}

fn collect_text_tokens(value: &CborValue, tokens: &mut Vec<String>) {
    match value {
        CborValue::Text(text) => tokens.push(text.clone()),
        CborValue::Array(values) => {
            for value in values {
                collect_text_tokens(value, tokens);
            }
        }
        CborValue::Map(entries) => {
            for (key, value) in entries {
                collect_text_tokens(key, tokens);
                collect_text_tokens(value, tokens);
            }
        }
        CborValue::Unsigned(_)
        | CborValue::Negative(_)
        | CborValue::Bytes(_)
        | CborValue::Bool(_)
        | CborValue::Null => {}
    }
}

#[test]
fn retired_vendor_vocabulary_never_appears_in_the_plan() {
    // architecture.md 25.3: the authority-facing surface carries no vendor
    // command, USB identity or partition address. Those live in private action
    // bodies only.
    let root = TempRoot::new("public-surface");
    let vertical = run_vertical(&root);
    let toolchain = native_tool();
    let registry = hypothetical_production_registry(&vertical, &toolchain);
    let built = vertical
        .provider
        .materialize_with_private_plan(&request(&vertical, toolchain), &registry)
        .unwrap();
    let plan = built.materialization.executable().unwrap();

    let public_bytes = plan
        .public_steps
        .iter()
        .map(|step| {
            String::from_utf8_lossy(
                &arkforge_core::digest::CanonicalCbor::to_canonical_bytes(step).unwrap(),
            )
            .into_owned()
        })
        .collect::<String>();
    for forbidden in ["rkdeveloptool", "wlx", "rl", "beginSector", "maskrom"] {
        assert!(
            !public_bytes.contains(forbidden),
            "public step encoding leaks {forbidden:?}"
        );
    }

    // Private actions also remain semantic: no subprocess or argv vocabulary
    // exists behind the public projection.
    let mut private_tokens = Vec::new();
    built
        .private_plan
        .as_ref()
        .unwrap()
        .actions
        .iter()
        .for_each(|action| collect_text_tokens(&action.body, &mut private_tokens));
    for retired in [
        "rkdeveloptool",
        "wlx",
        "rl",
        "rd",
        "ppt",
        "ld",
        "tool",
        "command",
    ] {
        assert!(
            !private_tokens.iter().any(|token| token == retired),
            "private action leaked exact token {retired:?}"
        );
    }
    assert!(
        private_tokens
            .iter()
            .any(|token| token == "write-partition")
    );
    assert!(
        private_tokens
            .iter()
            .any(|token| token == "readback-partition")
    );
}

#[test]
fn each_verify_step_carries_a_read_domain_characterization_sub_action() {
    // architecture.md 16.2 maps VerifyTarget to
    // "CharacterizeReadDomain + ReadbackPartition": the characterization is a
    // read-only sub-action inside the same public step, so it is digest-covered
    // without inventing a public step nobody authorized.
    let root = TempRoot::new("verify-subaction");
    let vertical = run_vertical(&root);
    let toolchain = native_tool();
    let registry = hypothetical_production_registry(&vertical, &toolchain);
    let built = vertical
        .provider
        .materialize_with_private_plan(&request(&vertical, toolchain), &registry)
        .unwrap();
    let plan = built.materialization.executable().unwrap();
    let private_plan = built.private_plan.as_ref().unwrap();

    let verify_steps: Vec<_> = plan
        .public_steps
        .iter()
        .filter(|step| step.kind == FlashStepKind::VerifyTarget)
        .collect();
    assert_eq!(verify_steps.len(), 9);

    for step in verify_steps {
        let sub_actions: Vec<_> = private_plan
            .actions
            .iter()
            .filter(|action| {
                action.step_id == step.step_id
                    && action.role == PrivateActionRole::ReadOnlyTransportSubAction
            })
            .collect();
        assert_eq!(
            sub_actions.len(),
            1,
            "step {} needs exactly one characterization sub-action",
            step.step_id
        );
        assert_eq!(sub_actions[0].effect_class, WorkflowEffect::ReadOnly);
    }
}

#[test]
fn an_artifact_with_an_unaccounted_member_blocks_execution() {
    // AF-V1 acceptance: unknown member fail closed. `updater_binary` is
    // discharged by the profile; a member nobody declared is not.
    let root = TempRoot::new("unknown-member");
    let mut vertical = run_vertical(&root);
    vertical
        .manifest
        .unclassified_members
        .push("mystery.blob".into());
    vertical
        .manifest
        .execution_relevant_unknowns
        .push(arkforge_core::plan::ExecutionUnknown {
            id: OpaqueId::new("RK-A02").unwrap(),
            summary: "unclassified archive members: mystery.blob".into(),
        });

    let toolchain = native_tool();
    let registry = hypothetical_production_registry(&vertical, &toolchain);
    let materialization = vertical
        .provider
        .materialize(&request(&vertical, toolchain), &registry)
        .unwrap();
    let assessment = materialization
        .assessment()
        .expect("an unaccounted member must block execution");
    assert!(
        assessment
            .unknowns
            .iter()
            .any(|unknown| unknown.summary.contains("mystery.blob"))
    );
}

#[test]
fn a_profile_offset_that_disagrees_with_the_artifact_table_blocks_execution() {
    let root = TempRoot::new("offset-drift");
    let mut vertical = run_vertical(&root);
    // Move the artifact's own view of `system` by one sector.
    let table = vertical.manifest.partition_table.as_mut().unwrap();
    let entry = table
        .entries
        .iter_mut()
        .find(|entry| entry.name == "system")
        .unwrap();
    entry.offset_sectors += 1;

    let report = vertical
        .provider
        .validate(&vertical.manifest, &vertical.profile, &vertical.probe)
        .unwrap();
    assert!(!report.is_clean());
    assert!(
        report
            .violations
            .iter()
            .any(|violation| violation.id.as_str() == "RK-V05")
    );

    let toolchain = native_tool();
    let registry = hypothetical_production_registry(&vertical, &toolchain);
    assert!(
        vertical
            .provider
            .materialize(&request(&vertical, toolchain), &registry)
            .unwrap()
            .assessment()
            .is_some()
    );
}

#[test]
fn a_missing_image_member_blocks_execution() {
    let root = TempRoot::new("missing-member");
    let mut vertical = run_vertical(&root);
    vertical
        .manifest
        .members
        .retain(|member| member.path != "vendor.img");

    let report = vertical
        .provider
        .validate(&vertical.manifest, &vertical.profile, &vertical.probe)
        .unwrap();
    assert!(
        report
            .violations
            .iter()
            .any(|violation| violation.id.as_str() == "RK-V07")
    );
}

#[test]
fn probing_never_touches_a_write_path() {
    // The probe context holds a transport and an observation. The replay
    // transport refuses any action the transcript did not record, so a probe
    // that tried to write would fail loudly rather than quietly succeed.
    let transport = TranscriptTransport::new(transcript::parse(CAMPAIGN).unwrap());
    for action in ["write-partition", "erase-partition", "wlx"] {
        assert!(
            transport.invocation(action, 0).is_err(),
            "{action} must not be replayable from a read-only campaign position"
        );
    }
}

#[test]
fn execution_side_spi_methods_refuse_in_this_build() {
    let provider = RockchipProvider::new();
    assert!(provider.execute_stored_action().is_err());
    assert!(provider.reconcile_read_only().is_err());
    assert!(provider.materialize_superseding_recovery().is_err());
}

#[test]
fn materialization_is_deterministic() {
    let root = TempRoot::new("deterministic");
    let vertical = run_vertical(&root);
    let toolchain = native_tool();
    let registry = hypothetical_production_registry(&vertical, &toolchain);
    let first = vertical
        .provider
        .materialize(&request(&vertical, toolchain.clone()), &registry)
        .unwrap();
    let second = vertical
        .provider
        .materialize(&request(&vertical, toolchain), &registry)
        .unwrap();
    assert_eq!(
        first.executable().unwrap().plan_digest,
        second.executable().unwrap().plan_digest
    );
}

#[test]
fn a_write_step_targets_only_partitions_the_profile_allows() {
    let root = TempRoot::new("allowlist");
    let vertical = run_vertical(&root);
    let toolchain = native_tool();
    let registry = hypothetical_production_registry(&vertical, &toolchain);
    let plan = vertical
        .provider
        .materialize(&request(&vertical, toolchain), &registry)
        .unwrap();
    let plan = plan.executable().unwrap();

    for step in plan
        .public_steps
        .iter()
        .filter(|step| step.kind == FlashStepKind::WriteTarget)
    {
        match &step.semantic_target {
            Some(SemanticTarget::Partition(partition)) => {
                assert!(
                    vertical.profile.allowed_target(partition).is_some(),
                    "{partition} is written but not on the profile allowlist"
                );
                assert!(
                    !vertical
                        .profile
                        .protected_targets
                        .contains(&PartitionId::new(partition.as_str()).unwrap())
                );
            }
            other => panic!("a write step must target a partition, found {other:?}"),
        }
    }
}
