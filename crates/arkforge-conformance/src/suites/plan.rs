//! Plan lowering end to end on the DAYU200 fixture archive: artifact manifest,
//! profile, observation, private actions, public steps, projection digests and
//! the sealed plan — every digest with its exact preimage bytes.
//!
//! The maturity registry used for the executable half names a fixture
//! campaign (`AF-CONF-PLAN`). That state is sealed into the plan digest, so
//! nothing produced here can be mistaken for a production plan or a
//! production pass (architecture.md §5.5, AFD-0004).

use crate::cbor_repr::diag;
use crate::json::Json;
use crate::suites::case_id;
use crate::tree::{Case, Tree};
use arkforge_artifact::{dayu200, fixture};
use arkforge_core::digest::{CanonicalCbor, Domain, decode_canonical, digest_in_domain, sha256};
use arkforge_core::identity::{
    HostPlatform, MaturityKey, MaturityState, ToolchainIdentity, ToolchainKind, Version,
};
use arkforge_core::ids::{OpaqueId, PlanId};
use arkforge_core::plan::ExecutionPurpose;
use arkforge_core::profile;
use arkforge_core::{
    AuthorityBindingRef, AuthorityNamespace, AuthoritySupportBinding, AuthoritySupportState,
};
use arkforge_provider::rockchip::{RockchipProvider, publish_af_v1_maturity};
use arkforge_provider::{
    FlashIntent, FlashProvider, MaterializeRequest, MaturityRegistry, ProbeContext,
};
use arkforge_transport::replay::TranscriptTransport;
use arkforge_transport::{DeviceTransport, TypedDiscoveryFilter, transcript};

const SUITE: &str = "plan";

const PROFILE_SOURCE: &str = include_str!("../../../../profiles/dayu200.yaml");
const CAMPAIGN: &str = include_str!("../../../../transcripts/dayu200-gj4-ecamp-96effff15.yaml");

fn native_tool() -> ToolchainIdentity {
    ToolchainIdentity {
        id: OpaqueId::new("arkforged-native-rockusb").unwrap(),
        kind: ToolchainKind::NativeProtocol,
        version: Version::new(0, 1, 0),
        backend_digest: sha256(b"native arkforged build"),
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

fn host() -> HostPlatform {
    HostPlatform::new("macos", "aarch64").unwrap()
}

fn domain_text(domain: Domain) -> String {
    String::from_utf8_lossy(domain.as_bytes()).replace('\0', "\\0")
}

pub fn populate(tree: &mut Tree) {
    let mut number = 0u32;

    // ---- 1. artifact ----------------------------------------------------------
    let archive = fixture::dayu200_archive();
    let manifest = dayu200::inspect(archive.as_slice()).expect("fixture archive inspects");
    let manifest_body = manifest.to_canonical_bytes().unwrap();
    let manifest_digest = manifest.digest().unwrap();
    number += 1;
    let include_archive = archive.len() <= 512 * 1024;
    let mut files: Vec<(&str, Vec<u8>)> = vec![("manifest.cbor", manifest_body.clone())];
    if include_archive {
        files.push(("archive.tar.gz", archive.clone()));
    }
    tree.case(
        &Case {
            id: case_id("PLAN", number),
            suite: SUITE,
            title: "DAYU200 fixture archive → artifact manifest".to_string(),
            requirements: vec!["AF-ART-001", "AF-ART-002", "AF-ART-010"],
            kind: "digest",
            description: "Inspect the gzip/tar archive (streaming; no member is extracted \
                          to a path) and produce the manifest. manifestDigest = \
                          SHA-256(domain || deterministic_cbor(manifest))."
                .to_string(),
            input: Json::object(vec![
                ("archiveSha256", Json::str(sha256(&archive).to_hex())),
                ("archiveBytes", Json::Unsigned(archive.len() as u64)),
                (
                    "archiveSource",
                    Json::str(if include_archive {
                        "archive.tar.gz"
                    } else {
                        "crates/arkforge-artifact/src/fixture.rs dayu200_archive() (too large to commit)"
                    }),
                ),
            ]),
            expected: Json::object(vec![
                ("domain", Json::str(domain_text(Domain::ArtifactManifest))),
                ("manifestDigest", Json::str(manifest_digest.to_hex())),
                ("manifestBodySha256", Json::str(sha256(&manifest_body).to_hex())),
                ("diag", Json::str(diag(&decode_canonical(&manifest_body).unwrap()))),
            ]),
        },
        files,
    );

    // ---- 2. profile -----------------------------------------------------------
    let profile = profile::load(PROFILE_SOURCE).expect("published profile loads");
    let profile_body = profile.to_canonical_bytes().unwrap();
    let profile_digest = profile.digest().unwrap();
    let profile_identity = profile.identity().unwrap();
    number += 1;
    tree.case(
        &Case {
            id: case_id("PLAN", number),
            suite: SUITE,
            title: "profiles/dayu200.yaml → canonical profile model and digest".to_string(),
            requirements: vec!["AF-PROF-001", "AF-PROF-002"],
            kind: "digest",
            description: "Load the profile through the strict YAML reader, then \
                          profileDigest = SHA-256(domain || deterministic_cbor(profile)). \
                          The digest is over the canonical model, not the file bytes, so \
                          comments and key order in the YAML do not change it."
                .to_string(),
            input: Json::object(vec![
                ("file", Json::str("profiles/dayu200.yaml")),
                (
                    "fileSha256",
                    Json::str(sha256(PROFILE_SOURCE.as_bytes()).to_hex()),
                ),
            ]),
            expected: Json::object(vec![
                ("domain", Json::str(domain_text(Domain::DeviceProfile))),
                ("profileDigest", Json::str(profile_digest.to_hex())),
                ("identity", Json::str(diag(&profile_identity.to_cbor()))),
                (
                    "diag",
                    Json::str(diag(&decode_canonical(&profile_body).unwrap())),
                ),
            ]),
        },
        vec![("profile.cbor", profile_body)],
    );

    // ---- 3. transcript, observation, probe ----------------------------------------
    let campaign = transcript::parse(CAMPAIGN).expect("golden transcript parses");
    let transcript_digest = campaign.digest().unwrap();
    let transport = TranscriptTransport::new(campaign);
    let observations = transport
        .discover(&TypedDiscoveryFilter::default(), 0)
        .unwrap();
    assert_eq!(observations.len(), 1);
    let observation = &observations[0];
    let observation_body = observation.to_canonical_bytes().unwrap();
    let provider = RockchipProvider::new();
    let probe = provider
        .probe(&ProbeContext {
            transport: &transport,
            observation,
            profile: &profile,
        })
        .unwrap();
    number += 1;
    tree.case(
        &Case {
            id: case_id("PLAN", number),
            suite: SUITE,
            title: "golden transcript → first observation and probe facts".to_string(),
            requirements: vec!["AF-TRN-001", "AF-TRN-002", "AF-TRN-020"],
            kind: "digest",
            description: "Replay transcripts/dayu200-gj4-ecamp-96effff15.yaml through the \
                          transcript transport; discover yields exactly one observation."
                .to_string(),
            input: Json::object(vec![
                (
                    "transcript",
                    Json::str("transcripts/dayu200-gj4-ecamp-96effff15.yaml"),
                ),
                (
                    "transcriptFileSha256",
                    Json::str(sha256(CAMPAIGN.as_bytes()).to_hex()),
                ),
            ]),
            expected: Json::object(vec![
                ("transcriptDigest", Json::str(transcript_digest.to_hex())),
                (
                    "observationDiag",
                    Json::str(diag(&decode_canonical(&observation_body).unwrap())),
                ),
                ("probeFactsDigest", Json::str(probe.facts_digest.to_hex())),
                (
                    "protocolFacts",
                    Json::Object(
                        probe
                            .protocol_facts
                            .iter()
                            .map(|(k, v)| (k.as_str().to_string(), Json::str(v.clone())))
                            .collect(),
                    ),
                ),
                (
                    "profileCandidate",
                    match &probe.profile_candidate {
                        Some(identity) => Json::str(diag(&identity.to_cbor())),
                        None => Json::Null,
                    },
                ),
            ]),
        },
        vec![("observation.cbor", observation_body)],
    );

    // ---- 4. materialize ---------------------------------------------------------
    let toolchain = native_tool();
    let request = |plan_id: &str, support: AuthoritySupportState| MaterializeRequest {
        plan_id: PlanId::new(plan_id).unwrap(),
        execution_purpose: ExecutionPurpose::PrimaryFlash,
        intent: FlashIntent::FullRestore,
        artifact: &manifest,
        artifact_id: OpaqueId::new("ART-001").unwrap(),
        profile: &profile,
        probe: &probe,
        authority_binding: binding(),
        authority_support: AuthoritySupportBinding {
            key_digest: sha256(b"conformance authority support"),
            state: support,
        },
        toolchain: toolchain.clone(),
        host_platform: host(),
        driver_facts_digest: sha256(b"driver facts"),
        evidence_set_digest: sha256(b"AD-003,AD-005,AD-006"),
        created_at_epoch_ms: 1_754_380_800_000,
        plan_lifetime_ms: 3_600_000,
    };

    // 4a. the published AF-V1 registry: hardware-gated → assessment only.
    let mut gated = MaturityRegistry::new();
    publish_af_v1_maturity(
        &mut gated,
        &provider,
        &profile,
        &toolchain,
        &host(),
        sha256(b"driver facts"),
        sha256(b"AD-003,AD-005,AD-006"),
    )
    .unwrap();
    let assessed = provider
        .materialize_with_private_plan(
            &request(
                "PLAN-CONF-ASSESS",
                AuthoritySupportState::HardwareGated {
                    blocker: "conformance fixture".into(),
                },
            ),
            &gated,
        )
        .unwrap();
    let assessment = assessed
        .materialization
        .assessment()
        .expect("a hardware-gated combination yields an assessment");
    number += 1;
    tree.case(
        &Case {
            id: case_id("PLAN", number),
            suite: SUITE,
            title: "hardware-gated combination: assessment, never an executable plan".to_string(),
            requirements: vec!["AF-PLAN-001", "AF-PLAN-002", "AF-EFF-010"],
            kind: "derive",
            description: "With the published (gated) maturity the same inputs yield a \
                          PlanAssessment: no plan id, no plan digest, and every unknown \
                          paired with the evidence that would close it."
                .to_string(),
            input: Json::object(vec![
                ("mechanicsMaturity", Json::str("hardwareGated")),
                ("authoritySupport", Json::str("hardwareGated")),
            ]),
            expected: Json::object(vec![
                ("executable", Json::Bool(false)),
                ("availability", Json::str(assessment.availability.as_str())),
                (
                    "unknownCount",
                    Json::Unsigned(assessment.unknowns.len() as u64),
                ),
                (
                    "evidenceRequirementCount",
                    Json::Unsigned(assessment.evidence_requirements.len() as u64),
                ),
                (
                    "persistentEffectCount",
                    Json::Unsigned(assessment.known_effects.persistent.len() as u64),
                ),
                ("assessmentDiag", Json::str(diag(&assessment.to_cbor()))),
            ]),
        },
        Vec::new(),
    );

    // 4b. a named fixture campaign: executable, sealed as HardwareCampaign.
    let mut campaign_registry = MaturityRegistry::new();
    campaign_registry.publish(
        &MaturityKey {
            provider: provider.identity().clone(),
            profile: profile_identity.clone(),
            artifact_format: provider.descriptor().artifact_formats[0].clone(),
            toolchain: toolchain.clone(),
            host_platform: host(),
            driver_facts_digest: sha256(b"driver facts"),
            evidence_set_digest: sha256(b"AD-003,AD-005,AD-006"),
        },
        MaturityState::HardwareCampaign {
            campaign: "AF-CONF-PLAN".into(),
        },
    );
    let built = provider
        .materialize_with_private_plan(
            &request(
                "PLAN-CONF-001",
                AuthoritySupportState::HardwareCampaign {
                    campaign: "AF-CONF-PLAN".into(),
                },
            ),
            &campaign_registry,
        )
        .unwrap();
    let envelope = built
        .materialization
        .executable()
        .expect("a named campaign yields an executable plan")
        .clone();
    let private_plan = built
        .private_plan
        .expect("private plan accompanies an executable plan");
    envelope.verify_self_digest().unwrap();

    // Private actions.
    number += 1;
    let mut action_files: Vec<(&str, Vec<u8>)> = Vec::new();
    let mut action_json = Vec::new();
    let action_names: Vec<String> = (0..private_plan.actions.len())
        .map(|i| format!("action-{:02}.cbor", i + 1))
        .collect();
    for (index, action) in private_plan.actions.iter().enumerate() {
        let body = action.to_canonical_bytes().unwrap();
        action_json.push(Json::object(vec![
            ("actionId", Json::str(action.action_id.as_str())),
            ("stepId", Json::str(action.step_id.as_str())),
            ("role", Json::str(action.role.as_str())),
            (
                "privateActionDigest",
                Json::str(action.digest().unwrap().to_hex()),
            ),
            ("diag", Json::str(diag(&decode_canonical(&body).unwrap()))),
        ]));
        action_files.push((
            Box::leak(action_names[index].clone().into_boxed_str()),
            body,
        ));
    }
    tree.case(
        &Case {
            id: case_id("PLAN", number),
            suite: SUITE,
            title: "private execution plan: every action body and its digest".to_string(),
            requirements: vec!["AF-PROJ-001", "AF-PROJ-002"],
            kind: "digest",
            description: "privateActionDigest = SHA-256(domain || deterministic_cbor(action)). \
                          The private plan never crosses the agent/app API; only these \
                          digests do."
                .to_string(),
            input: Json::object(vec![
                ("planId", Json::str("PLAN-CONF-001")),
                ("intent", Json::str("fullRestore")),
            ]),
            expected: Json::object(vec![
                ("domain", Json::str(domain_text(Domain::PrivateAction))),
                (
                    "actionCount",
                    Json::Unsigned(private_plan.actions.len() as u64),
                ),
                ("actions", Json::Array(action_json)),
            ]),
        },
        action_files,
    );

    // Public steps.
    number += 1;
    let mut step_files: Vec<(&str, Vec<u8>)> = Vec::new();
    let mut step_json = Vec::new();
    let step_names: Vec<String> = (0..envelope.public_steps.len())
        .map(|i| format!("step-{:02}.cbor", i + 1))
        .collect();
    for (index, step) in envelope.public_steps.iter().enumerate() {
        let body = step.to_canonical_bytes().unwrap();
        step_json.push(Json::object(vec![
            ("stepId", Json::str(step.step_id.as_str())),
            ("kind", Json::str(step.kind.as_str())),
            (
                "publicStepDigest",
                Json::str(step.digest().unwrap().to_hex()),
            ),
            (
                "privateActionDigest",
                Json::str(step.private_action_digest.to_hex()),
            ),
            ("diag", Json::str(diag(&decode_canonical(&body).unwrap()))),
        ]));
        step_files.push((Box::leak(step_names[index].clone().into_boxed_str()), body));
    }
    tree.case(
        &Case {
            id: case_id("PLAN", number),
            suite: SUITE,
            title: "public steps: bodies and digests, in execution order".to_string(),
            requirements: vec!["AF-PLAN-010", "AF-PLAN-011"],
            kind: "digest",
            description: "publicStepDigest = SHA-256(domain || deterministic_cbor(step)). Each \
                          step binds the digest of the private action that implements it."
                .to_string(),
            input: Json::object(vec![("planId", Json::str("PLAN-CONF-001"))]),
            expected: Json::object(vec![
                ("domain", Json::str(domain_text(Domain::PublicStep))),
                (
                    "stepCount",
                    Json::Unsigned(envelope.public_steps.len() as u64),
                ),
                ("steps", Json::Array(step_json)),
            ]),
        },
        step_files,
    );

    // Projection digests.
    number += 1;
    let ordered: Vec<u8> = envelope
        .per_action_digests
        .iter()
        .flat_map(|b| b.private_action_digest.as_bytes().to_vec())
        .collect();
    let mapping = arkforge_core::digest::CborValue::array(
        envelope
            .per_action_digests
            .iter()
            .map(|b| b.to_cbor())
            .collect(),
    );
    let mapping_bytes = mapping.to_canonical_bytes().unwrap();
    assert_eq!(
        digest_in_domain(Domain::PublicProjection, &mapping_bytes),
        envelope.public_projection_digest
    );
    assert_eq!(
        arkforge_core::digest::digest_ordered(
            Domain::ProviderExecutionPlan,
            &envelope
                .per_action_digests
                .iter()
                .map(|b| b.private_action_digest)
                .collect::<Vec<_>>()
        ),
        envelope.provider_execution_plan_digest
    );
    tree.case(
        &Case {
            id: case_id("PLAN", number),
            suite: SUITE,
            title: "projection digests and their preimages".to_string(),
            requirements: vec!["AF-PROJ-010", "AF-PROJ-011", "AF-PROJ-012"],
            kind: "digest",
            description: "providerExecutionPlanDigest = SHA-256(domainA || d1 || d2 || …) over \
                          the ordered private action digests (`ordered-digests.bin` is the \
                          concatenation). publicProjectionDigest = SHA-256(domainB || \
                          deterministic_cbor(array of bindings)) (`mapping.cbor`)."
                .to_string(),
            input: Json::object(vec![("planId", Json::str("PLAN-CONF-001"))]),
            expected: Json::object(vec![
                (
                    "providerExecutionPlanDomain",
                    Json::str(domain_text(Domain::ProviderExecutionPlan)),
                ),
                (
                    "providerExecutionPlanDigest",
                    Json::str(envelope.provider_execution_plan_digest.to_hex()),
                ),
                (
                    "publicProjectionDomain",
                    Json::str(domain_text(Domain::PublicProjection)),
                ),
                (
                    "publicProjectionDigest",
                    Json::str(envelope.public_projection_digest.to_hex()),
                ),
                (
                    "bindings",
                    Json::Array(
                        envelope
                            .per_action_digests
                            .iter()
                            .map(|b| {
                                Json::object(vec![
                                    ("stepId", Json::str(b.step_id.as_str())),
                                    ("actionId", Json::str(b.action_id.as_str())),
                                    ("role", Json::str(b.role.as_str())),
                                    (
                                        "privateActionDigest",
                                        Json::str(b.private_action_digest.to_hex()),
                                    ),
                                ])
                            })
                            .collect(),
                    ),
                ),
                ("mappingDiag", Json::str(diag(&mapping))),
            ]),
        },
        vec![
            ("ordered-digests.bin", ordered),
            ("mapping.cbor", mapping_bytes),
        ],
    );

    // Effect set.
    number += 1;
    let effect_body = envelope.effect_set.to_canonical_bytes().unwrap();
    tree.case(
        &Case {
            id: case_id("PLAN", number),
            suite: SUITE,
            title: "effect set: body and digest".to_string(),
            requirements: vec!["AF-EFF-001", "AF-EFF-002"],
            kind: "digest",
            description: "effectSetDigest = SHA-256(domain || deterministic_cbor(effectSet)). \
                          Persistent effects, transient effects and the four data-impact \
                          axes."
                .to_string(),
            input: Json::object(vec![("planId", Json::str("PLAN-CONF-001"))]),
            expected: Json::object(vec![
                ("domain", Json::str(domain_text(Domain::EffectSet))),
                (
                    "effectSetDigest",
                    Json::str(envelope.effect_set.digest().unwrap().to_hex()),
                ),
                (
                    "persistentCount",
                    Json::Unsigned(envelope.effect_set.persistent.len() as u64),
                ),
                (
                    "transientCount",
                    Json::Unsigned(envelope.effect_set.transient.len() as u64),
                ),
                (
                    "diag",
                    Json::str(diag(&decode_canonical(&effect_body).unwrap())),
                ),
            ]),
        },
        vec![("effect-set.cbor", effect_body)],
    );

    // The sealed plan.
    number += 1;
    let plan_body = envelope.digest_body_bytes().unwrap();
    assert_eq!(
        digest_in_domain(Domain::Plan, &plan_body),
        envelope.plan_digest
    );
    tree.case(
        &Case {
            id: case_id("PLAN", number),
            suite: SUITE,
            title: "sealed plan: digest preimage and plan digest".to_string(),
            requirements: vec!["AF-PLAN-020", "AF-PLAN-021", "AF-PLAN-022"],
            kind: "digest",
            description: "planDigest = SHA-256(domain || `plan-body.cbor`). The body \
                          carries the maturity state (`hardwareCampaign` / AF-CONF-PLAN) and \
                          the authority-support state, so a campaign plan and a production \
                          plan with otherwise identical contents have different digests."
                .to_string(),
            input: Json::object(vec![
                ("planId", Json::str(envelope.plan_id.as_str())),
                (
                    "executionPurpose",
                    Json::str(envelope.execution_purpose.as_str()),
                ),
                (
                    "createdAtEpochMs",
                    Json::Unsigned(envelope.created_at_epoch_ms),
                ),
                (
                    "expiresAtEpochMs",
                    Json::Unsigned(envelope.expires_at_epoch_ms),
                ),
                (
                    "toolchainBackendDigest",
                    Json::str("sha256(\"native arkforged build\")"),
                ),
                (
                    "authorityBindingStableIdentity",
                    Json::str("sha256(\"dayu200-gj4\")"),
                ),
            ]),
            expected: Json::object(vec![
                ("domain", Json::str(domain_text(Domain::Plan))),
                ("planDigest", Json::str(envelope.plan_digest.to_hex())),
                ("planBodySha256", Json::str(sha256(&plan_body).to_hex())),
                ("planBodyLength", Json::Unsigned(plan_body.len() as u64)),
                (
                    "diag",
                    Json::str(diag(&decode_canonical(&plan_body).unwrap())),
                ),
            ]),
        },
        vec![("plan-body.cbor", plan_body)],
    );
}
