//! The acceptance campaign that breaks the maturity ring, and the guards on it.
//!
//! `HardwareGated` names its own deadlock: a combination becomes
//! `ProductionVerified` only after a real DAYU200 flash passes through
//! ArkForge, a real flash needs an executable plan, and until this change only
//! `ProductionVerified` could back one. Measured 2026-08-17 against a live
//! daemon: `materializePlan` on the shipped DAYU200 profile returns a
//! `PlanAssessment` with `availability: Unavailable`, so no `plan_id` is ever
//! stored and `startExecution` answers `PLAN_NOT_STARTABLE`. The first flash
//! of any new combination was unreachable.
//!
//! The break is a named campaign rather than a relaxed gate. Admitting
//! `HardwareGated` into [`MaturityState::permits_executable_plan`] would have
//! made every unmeasured combination executable — deleting the gate rather
//! than passing through it.
//!
//! What these cases hold to:
//!
//! - a campaign is opt-in, and its absence still refuses;
//! - a transcript is never a campaign, whatever is asked for;
//! - a campaign run is not production evidence.

use arkforge_core::digest::sha256;
use arkforge_core::identity::{
    HostPlatform, MaturityKey, MaturityState, ToolchainIdentity, ToolchainKind, Version,
};
use arkforge_core::ids::OpaqueId;
use arkforge_core::profile::{self, DeviceProfile};
use arkforge_provider::rockchip::{publish_dayu200_maturity, RockchipProvider};
use arkforge_provider::{FlashProvider, MaturityRegistry};

const PROFILE_SOURCE: &str = include_str!("../../../profiles/dayu200.yaml");
fn profile() -> DeviceProfile {
    profile::load(PROFILE_SOURCE).unwrap()
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
        backend_digest: sha256(b"a transcript"),
        upstream_ref: None,
    }
}

fn published(toolchain: &ToolchainIdentity, campaign: Option<&str>) -> MaturityState {
    let provider = RockchipProvider::new();
    let profile = profile();
    let mut registry = MaturityRegistry::new();
    publish_dayu200_maturity(
        &mut registry,
        &provider,
        &profile,
        toolchain,
        &HostPlatform::new("macos", "aarch64").unwrap(),
        sha256(b"driver facts"),
        sha256(b"AD-003,AD-005,AD-006"),
        campaign,
    )
    .unwrap();
    registry.lookup(&MaturityKey {
        provider: provider.identity().clone(),
        profile: profile.identity().unwrap(),
        artifact_format: provider.descriptor().artifact_formats[0].clone(),
        toolchain: toolchain.clone(),
        host_platform: HostPlatform::new("macos", "aarch64").unwrap(),
        driver_facts_digest: sha256(b"driver facts"),
        evidence_set_digest: sha256(b"AD-003,AD-005,AD-006"),
    })
}

#[test]
fn without_a_campaign_the_combination_still_refuses() {
    // The default has to stay closed. If asking for nothing produced a
    // campaign, the gate would be gone for every operator who never read this
    // file.
    let state = published(&native_tool(), None);
    assert!(matches!(state, MaturityState::HardwareGated { .. }));
    assert!(!state.permits_executable_plan());
}

#[test]
fn a_named_campaign_permits_an_executable_plan() {
    let state = published(&native_tool(), Some("AFA-AC-6"));
    assert!(state.permits_executable_plan());
    assert_eq!(state.campaign(), Some("AFA-AC-6"));
}

#[test]
fn a_campaign_run_is_not_production_evidence() {
    // The whole point of naming the state rather than relaxing the gate. Its
    // writes are real; its support claim is not.
    let state = published(&native_tool(), Some("AFA-AC-6"));
    assert!(!state.is_production_evidence());
    assert!(MaturityState::ProductionVerified.is_production_evidence());
}

#[test]
fn a_campaign_reports_no_blocker() {
    // A campaign is a qualification on what the run proves, not an obstacle to
    // running it. Reporting it as a blocker would send an operator looking for
    // something to fix while the daemon was working as asked.
    assert_eq!(published(&native_tool(), Some("AFA-AC-6")).blocker(), None);
    assert!(published(&native_tool(), None).blocker().is_some());
}

#[test]
fn a_transcript_is_never_a_campaign() {
    // The AF-V1 reason is unchanged by any flag: a transcript is not a device.
    // A campaign against a recording would produce evidence about the
    // recording while naming a board, which is the one substitution the whole
    // replay transport exists to keep visible.
    let state = published(&replay_tool(), Some("AFA-AC-6"));
    assert!(matches!(state, MaturityState::PlanOnly { .. }));
    assert!(!state.permits_executable_plan());
    assert_eq!(state.campaign(), None);
}
