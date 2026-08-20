//! The AF-V3 vertical, end to end:
//!
//! ```text
//! PAC inspect → USB discover/probe → profile candidate → PlanAssessment
//!   → evidence requirements → start unavailable
//! ```
//!
//! Every assertion here is an AF-V3 acceptance line from architecture.md 22,
//! plus the wrong-device scenarios from 19.2.

use arkforge_artifact::manifest::ParserConfidence;
use arkforge_artifact::{dayu200, fixture, pac};
use arkforge_core::digest::sha256;
use arkforge_core::effect::DataImpactState;
use arkforge_core::identity::{
    HostPlatform, MaturityState, ToolchainIdentity, ToolchainKind, Version,
};
use arkforge_core::ids::{OpaqueId, PlanId};
use arkforge_core::plan::{ExecutionAvailability, ExecutionPurpose};
use arkforge_core::profile::{self, DeviceProfile};
use arkforge_core::{AuthorityBindingRef, AuthorityNamespace};
use arkforge_provider::rockchip::RockchipProvider;
use arkforge_provider::unisoc::{UnisocProvider, publish_af_v3_maturity};
use arkforge_provider::{
    FlashIntent, FlashProvider, MaterializeRequest, MaturityRegistry, ProbeContext, ProviderProbe,
};
use arkforge_transport::replay::TranscriptTransport;
use arkforge_transport::transcript::{self, TranscriptProvenance};
use arkforge_transport::{DeviceTransport, TypedDiscoveryFilter};

const DAYU600_PROFILE: &str = include_str!("../../../profiles/dayu600.yaml");
const DAYU200_PROFILE: &str = include_str!("../../../profiles/dayu200.yaml");
const DAYU600_TRANSCRIPT: &str =
    include_str!("../../../transcripts/dayu600-research-synthetic.yaml");
const DAYU200_TRANSCRIPT: &str =
    include_str!("../../../transcripts/dayu200-gj4-ecamp-96effff15.yaml");

/// A container shaped like a firmware package. It is not a PAC file and no test
/// here claims it is one.
fn synthetic_container() -> Vec<u8> {
    let mut bytes = b"BP_R1.0.0".to_vec();
    bytes.extend_from_slice(&[0u8; 7]);
    for index in 0..10u8 {
        let start = bytes.len();
        bytes.push(0x02);
        bytes.push(index);
        for character in format!("IMG_{index}").chars() {
            bytes.push(character as u8);
            bytes.push(0);
        }
        while bytes.len() - start < 32 {
            bytes.push(0);
        }
    }
    bytes.extend(fixture::fixture_body("dayu600-payload", 12_000));
    bytes.extend_from_slice(&[0xffu8; 2048]);
    bytes
}

fn research_toolchain() -> ToolchainIdentity {
    ToolchainIdentity {
        id: OpaqueId::new("research-inspect").unwrap(),
        kind: ToolchainKind::Replay,
        version: Version::new(0, 1, 0),
        backend_digest: sha256(b"research"),
        upstream_ref: None,
    }
}

fn probe_dayu600(profile: &DeviceProfile, provider: &UnisocProvider) -> ProviderProbe {
    let transport = TranscriptTransport::new(transcript::parse(DAYU600_TRANSCRIPT).unwrap());
    let observations = transport
        .discover(&TypedDiscoveryFilter::default(), 0)
        .unwrap();
    provider
        .probe(&ProbeContext {
            transport: &transport,
            observation: &observations[0],
            profile,
        })
        .unwrap()
}

fn registry(profile: &DeviceProfile, provider: &UnisocProvider) -> MaturityRegistry {
    let mut registry = MaturityRegistry::new();
    publish_af_v3_maturity(
        &mut registry,
        provider,
        profile,
        &research_toolchain(),
        &HostPlatform::new("macos", "aarch64").unwrap(),
        sha256(b"driver"),
        sha256(b"evidence"),
    )
    .unwrap();
    registry
}

#[test]
fn the_dayu600_profile_loads_and_states_every_thing_it_does_not_know() {
    let profile = profile::load(DAYU600_PROFILE).expect("a research profile must be loadable");
    assert_eq!(profile.id.as_str(), "org.openharmony.dayu600");
    assert_eq!(profile.soc.family.as_str(), "uis7885");

    assert!(profile.allowed_targets.is_empty(), "nothing is writable");
    assert!(
        profile.mode_transitions.is_empty(),
        "no transition observed"
    );
    assert_eq!(profile.storage.logical_block_size, None);
    assert_eq!(profile.read_domain.erased_medium_filler, None);
    assert_eq!(profile.data_impact.userdata, DataImpactState::Unknown);

    let ids: Vec<&str> = profile
        .execution_blockers()
        .iter()
        .map(|blocker| blocker.id())
        .collect();
    for expected in [
        "PROF-B01", "PROF-B02", "PROF-B03", "PROF-B04", "PROF-B05", "PROF-B06",
    ] {
        assert!(ids.contains(&expected), "{expected} missing from {ids:?}");
    }
    assert!(!profile.permits_executable_plan());
}

#[test]
fn pac_inspection_is_research_only_and_carries_the_exact_unknown_list() {
    let container = synthetic_container();
    let (manifest, report) = pac::inspect(container.as_slice()).unwrap();

    assert_eq!(manifest.confidence, ParserConfidence::ResearchOnly);
    assert!(manifest.partition_table.is_none());
    assert!(!report.candidates.is_empty());

    let ids: Vec<&str> = manifest
        .execution_relevant_unknowns
        .iter()
        .map(|unknown| unknown.id.as_str())
        .collect();
    assert_eq!(ids.len(), 12, "the unknown list is exact, not indicative");
    for expected in [
        "UNI-U01", "UNI-U02", "UNI-U03", "UNI-U04", "UNI-U05", "UNI-U06", "UNI-U07", "UNI-U08",
        "UNI-U09", "UNI-U10", "UNI-U11", "UNI-U12",
    ] {
        assert!(ids.contains(&expected), "{expected} missing");
    }
}

#[test]
fn the_vertical_ends_in_an_assessment_with_evidence_requirements() {
    let profile = profile::load(DAYU600_PROFILE).unwrap();
    let provider = UnisocProvider::new();
    let (manifest, _) = pac::inspect(synthetic_container().as_slice()).unwrap();
    let probe = probe_dayu600(&profile, &provider);
    let registry = registry(&profile, &provider);

    let materialization = provider
        .materialize(
            &MaterializeRequest {
                plan_id: PlanId::new("PLAN-DAYU600").unwrap(),
                execution_purpose: ExecutionPurpose::PrimaryFlash,
                intent: FlashIntent::FullRestore,
                artifact: &manifest,
                artifact_id: OpaqueId::new("ART-DAYU600").unwrap(),
                profile: &profile,
                probe: &probe,
                authority_binding: AuthorityBindingRef {
                    authority_namespace: AuthorityNamespace::new("arkdeck").unwrap(),
                    binding_id: OpaqueId::new("TGT-UNKNOWN").unwrap(),
                    binding_revision: 0,
                    stable_identity_digest: probe.facts_digest,
                },
                toolchain: research_toolchain(),
                host_platform: HostPlatform::new("macos", "aarch64").unwrap(),
                driver_facts_digest: sha256(b"driver"),
                evidence_set_digest: sha256(b"evidence"),
                created_at_epoch_ms: 1_754_380_800_000,
                plan_lifetime_ms: 3_600_000,
            },
            &registry,
        )
        .unwrap();

    let assessment = materialization
        .assessment()
        .expect("DAYU600 can only ever produce an assessment");
    assert!(materialization.executable().is_none());

    match &assessment.availability {
        ExecutionAvailability::Unavailable { reason } => {
            assert!(reason.contains("17.5"), "{reason}");
        }
        other => panic!("execution must be unavailable, got {other:?}"),
    }

    // Every unknown names what would close it, and at grade A: architecture.md
    // 17.4 needs parser, official-tool behaviour and a device fact to agree.
    assert_eq!(
        assessment.evidence_requirements.len(),
        assessment.unknowns.len()
    );
    assert!(
        assessment
            .evidence_requirements
            .iter()
            .all(|requirement| requirement.minimum_grade == 'A')
    );

    // The unknowns come from three independent sources.
    let ids: Vec<&str> = assessment
        .unknowns
        .iter()
        .map(|unknown| unknown.id.as_str())
        .collect();
    assert!(ids.iter().any(|id| id.starts_with("UNI-U")), "{ids:?}");
    assert!(ids.iter().any(|id| id.starts_with("PROF-B")), "{ids:?}");
    assert!(
        ids.contains(&"UNI-G01"),
        "the evidence gate itself: {ids:?}"
    );

    // The assessment declares unknown data impact — not "read only", which
    // would be a claim that nothing is touched.
    assert_eq!(assessment.known_effects.data_impact.unknown_axes().len(), 4);
    assert!(assessment.known_effects.persistent.is_empty());

    // And an EffectSet with unknown impact can never be sealed into a plan.
    assert!(assessment.known_effects.validate_executable().is_err());
}

#[test]
fn maturity_for_dayu600_is_research_only_and_blocks_execution() {
    let profile = profile::load(DAYU600_PROFILE).unwrap();
    let provider = UnisocProvider::new();
    let registry = registry(&profile, &provider);
    let state = registry.lookup(&arkforge_core::identity::MaturityKey {
        provider: provider.identity().clone(),
        profile: profile.identity().unwrap(),
        artifact_format: provider.artifact_format(),
        toolchain: research_toolchain(),
        host_platform: HostPlatform::new("macos", "aarch64").unwrap(),
        driver_facts_digest: sha256(b"driver"),
        evidence_set_digest: sha256(b"evidence"),
    });
    assert!(matches!(state, MaturityState::ResearchOnly { .. }));
    assert!(!state.permits_executable_plan());
}

#[test]
fn the_dayu600_transcript_is_not_evidence_and_says_so() {
    // AF-V3 acceptance: "未把 plan-only 记为真机刷写通过". The one structural
    // guard against that is the transcript's provenance.
    let parsed = transcript::parse(DAYU600_TRANSCRIPT).unwrap();
    assert_eq!(parsed.provenance, TranscriptProvenance::Synthetic);
    assert!(!parsed.provenance.supports_protocol_claims());
    assert!(parsed.source.contains("no device was observed"));

    // Contrast: the DAYU200 campaign transcript is derived from published
    // receipts — stronger than synthetic, still not a capture.
    let dayu200 = transcript::parse(DAYU200_TRANSCRIPT).unwrap();
    assert_eq!(
        dayu200.provenance,
        TranscriptProvenance::DerivedFromPublishedReceipts
    );
    assert!(!dayu200.provenance.supports_protocol_claims());
}

// ---------------------------------------------------------------------------
// Wrong device (architecture.md 19.2)
// ---------------------------------------------------------------------------

#[test]
fn the_unisoc_provider_refuses_a_dayu200_device() {
    let profile = profile::load(DAYU600_PROFILE).unwrap();
    let provider = UnisocProvider::new();
    let transport = TranscriptTransport::new(transcript::parse(DAYU200_TRANSCRIPT).unwrap());
    let observations = transport
        .discover(&TypedDiscoveryFilter::default(), 0)
        .unwrap();
    // The DAYU200 campaign's observation is in `hdc-normal`, which the DAYU600
    // profile happens to declare — so this must be refused later, by the
    // artifact and effect facts, not by a mode name.
    let probe = provider.probe(&ProbeContext {
        transport: &transport,
        observation: &observations[0],
        profile: &profile,
    });
    assert!(probe.is_ok(), "a shared mode name is not an identification");

    // What the probe does say is that nothing is confirmed.
    let probe = probe.unwrap();
    let confirmation = probe
        .protocol_facts
        .iter()
        .find(|(key, _)| key.as_str() == "identityConfirmation")
        .expect("the probe must state its confidence");
    assert!(
        confirmation.1.starts_with("unconfirmed"),
        "{confirmation:?}"
    );
}

#[test]
fn the_unisoc_provider_refuses_a_device_in_a_mode_it_does_not_declare() {
    let profile = profile::load(DAYU600_PROFILE).unwrap();
    let provider = UnisocProvider::new();
    // A Rockchip loader observation: `rockusb-loader` is not a DAYU600 mode.
    let mut transcript = transcript::parse(DAYU200_TRANSCRIPT).unwrap();
    transcript.records.retain(|record| {
        record
            .observation
            .as_ref()
            .map(|observation| observation.mode.as_str() == "rockusb-loader")
            .unwrap_or(false)
    });
    for (index, record) in transcript.records.iter_mut().enumerate() {
        record.sequence = index as u32 + 1;
        record.kind = arkforge_transport::transcript::RecordKind::Observation;
    }
    let transport = TranscriptTransport::new(transcript);
    let observations = transport
        .discover(&TypedDiscoveryFilter::default(), 0)
        .unwrap();
    assert!(!observations.is_empty());

    let error = provider
        .probe(&ProbeContext {
            transport: &transport,
            observation: &observations[0],
            profile: &profile,
        })
        .unwrap_err();
    assert!(
        format!("{error}").contains("will not guess"),
        "the provider must refuse rather than assume: {error}"
    );
}

#[test]
fn the_rockchip_provider_refuses_a_pac_artifact() {
    let dayu200 = profile::load(DAYU200_PROFILE).unwrap();
    let provider = RockchipProvider::new();
    let (pac_manifest, _) = pac::inspect(synthetic_container().as_slice()).unwrap();

    let transport = TranscriptTransport::new(transcript::parse(DAYU200_TRANSCRIPT).unwrap());
    let observations = transport
        .discover(&TypedDiscoveryFilter::default(), 0)
        .unwrap();
    let probe = provider
        .probe(&ProbeContext {
            transport: &transport,
            observation: &observations[0],
            profile: &dayu200,
        })
        .unwrap();

    let report = provider.validate(&pac_manifest, &dayu200, &probe).unwrap();
    assert!(!report.is_clean());
    let ids: Vec<&str> = report
        .violations
        .iter()
        .map(|violation| violation.id.as_str())
        .collect();
    assert!(
        ids.contains(&"UNI-V01") || ids.contains(&"RK-V01"),
        "{ids:?}"
    );
}

#[test]
fn the_unisoc_provider_refuses_a_rockchip_artifact() {
    let profile = profile::load(DAYU600_PROFILE).unwrap();
    let provider = UnisocProvider::new();
    let rockchip_manifest = dayu200::inspect(fixture::dayu200_archive().as_slice()).unwrap();
    let probe = probe_dayu600(&profile, &provider);

    let report = provider
        .validate(&rockchip_manifest, &profile, &probe)
        .unwrap();
    assert!(!report.is_clean());
    let ids: Vec<&str> = report
        .violations
        .iter()
        .map(|violation| violation.id.as_str())
        .collect();
    assert!(ids.contains(&"UNI-V01"), "{ids:?}");
    assert!(ids.contains(&"UNI-V02"), "{ids:?}");
}

#[test]
fn a_dayu200_plan_cannot_be_built_against_the_dayu600_profile() {
    // The reverse wrong-device case: a real Rockchip artifact, a real Rockchip
    // provider, but the wrong profile. It must fail on facts, not on a name.
    let dayu600 = profile::load(DAYU600_PROFILE).unwrap();
    let provider = RockchipProvider::new();
    let manifest = dayu200::inspect(fixture::dayu200_archive().as_slice()).unwrap();
    let transport = TranscriptTransport::new(transcript::parse(DAYU200_TRANSCRIPT).unwrap());
    let observations = transport
        .discover(&TypedDiscoveryFilter::default(), 0)
        .unwrap();
    let probe = provider
        .probe(&ProbeContext {
            transport: &transport,
            observation: &observations[0],
            profile: &dayu600,
        })
        .unwrap();

    let report = provider.validate(&manifest, &dayu600, &probe).unwrap();
    assert!(!report.is_clean());
    // The DAYU600 profile does not accept the Rockchip artifact format.
    assert!(
        report
            .violations
            .iter()
            .any(|violation| violation.id.as_str() == "RK-V02")
    );
}
