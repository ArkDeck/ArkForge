//! `profiles/dayu200.yaml` must agree, value for value, with the ArkDeck facts
//! it was pinned from.
//!
//! AF-V1 acceptance: "Profile 含 readDomain 与 per-target 验证强度，与 AD-006
//! 一致" and "current DAYU200 archive facts parity". The parity anchors are
//! ArkDeck `RockchipFlashProfile.dayu200` (write order, offsets, member map,
//! membership-less write-forbidden set) and `partition-mapping.json`.

use arkforge_core::effect::{DataImpactState, DeviceMode};
use arkforge_core::ids::PartitionId;
use arkforge_core::profile::{self, HardwareRevisionPolicy, WriteDomainDeclaration};
use arkforge_core::verification::{ReadDomainDeclaration, VerificationStrength};

fn load() -> arkforge_core::profile::DeviceProfile {
    let source = include_str!("../../../profiles/dayu200.yaml");
    profile::load(source).expect("profiles/dayu200.yaml must load and validate")
}

#[test]
fn the_nine_writable_targets_match_arkdeck_exactly() {
    let profile = load();
    // ArkDeck RockchipFlashProfile.dayu200 `mappedPartitions`.
    let expected: [(&str, u32, u64, &str); 9] = [
        ("uboot", 1, 8192, "uboot.img"),
        ("resource", 2, 28672, "resource.img"),
        ("boot_linux", 3, 40960, "boot_linux.img"),
        ("ramdisk", 4, 237_568, "ramdisk.img"),
        ("system", 5, 245_760, "system.img"),
        ("vendor", 6, 4_440_064, "vendor.img"),
        ("updater", 7, 6_742_016, "updater.img"),
        ("chip_ckm", 8, 6_938_624, "chip_ckm.img"),
        ("userdata", 9, 19_955_712, "userdata.img"),
    ];
    assert_eq!(
        profile.allowed_targets.len(),
        expected.len(),
        "the writable set must be exactly the nine mapped partitions"
    );
    for (partition, order, offset, member) in expected {
        let target = profile
            .allowed_target(&PartitionId::new(partition).unwrap())
            .unwrap_or_else(|| panic!("{partition} must be an allowed target"));
        assert_eq!(target.write_order, order, "{partition} write order");
        assert_eq!(target.offset_sectors, offset, "{partition} offset");
        assert_eq!(
            target.source_member.as_deref(),
            Some(member),
            "{partition} source member"
        );
    }
}

#[test]
fn the_membership_less_partitions_are_protected() {
    let profile = load();
    // ArkDeck `membershiplessPartitionsWriteForbidden`.
    let expected = [
        "misc",
        "bootctrl",
        "sys-prod",
        "chip-prod",
        "eng_system",
        "eng_chipset",
    ];
    let observed: Vec<&str> = profile
        .protected_targets
        .iter()
        .map(|target| target.as_str())
        .collect();
    assert_eq!(observed, expected);
}

#[test]
fn orphan_images_in_the_archive_are_not_writable() {
    // `chip_prod.img` and `sys_prod.img` exist in the pinned archive but map to
    // no writable target: an image being present is not permission to write it
    // (architecture.md 16.3).
    let profile = load();
    let referenced: Vec<&str> = profile
        .allowed_targets
        .iter()
        .filter_map(|target| target.source_member.as_deref())
        .collect();
    for orphan in ["chip_prod.img", "sys_prod.img", "MiniLoaderAll.bin"] {
        assert!(
            !referenced.contains(&orphan),
            "{orphan} must not back a writable target"
        );
    }
}

#[test]
fn the_read_domain_encodes_ad006() {
    let profile = load();
    // The write face reaches the whole medium…
    assert_eq!(profile.read_domain.write, WriteDomainDeclaration::FullDisk);
    // …and the read face is measured every execution, never pinned here.
    assert_eq!(
        profile.read_domain.read,
        ReadDomainDeclaration::CharacterizeAtRuntime
    );
    assert_eq!(profile.read_domain.erased_medium_filler, Some(0xCC));

    // The profile source must not pin a window size: the 65536-sector figure is
    // one session's observation (AD-006), and a constant here would turn it
    // into a fact about all DAYU200 boards.
    let source = include_str!("../../../profiles/dayu200.yaml");
    for line in source.lines() {
        let statement = line.split('#').next().unwrap_or("");
        assert!(
            !statement.contains("65536"),
            "profile must not pin an observed read-window size: {line}"
        );
    }
}

#[test]
fn every_target_carries_fallback_evidence_because_the_read_face_may_not_reach_it() {
    let profile = load();
    for target in &profile.allowed_targets {
        let fallback = target.verification.fallback;
        assert!(
            fallback.write_completion_semantics && fallback.build_postflight,
            "{} must declare both fallbacks; most offsets sit past the observed read window",
            target.partition
        );
        // A readback claim is permitted, but only as a ceiling that applies
        // when the runtime characterization says the read face covers it.
        assert_eq!(
            target.verification.max_strength_when_readable,
            VerificationStrength::FullHash,
            "{}",
            target.partition
        );
    }
}

#[test]
fn userdata_impact_is_declared_overwritten() {
    let profile = load();
    assert_eq!(profile.data_impact.userdata, DataImpactState::Overwritten);
    assert!(profile.data_impact.unknown_axes().is_empty());
}

#[test]
fn mode_aliases_resolve_and_transitions_declare_transient_tolerance() {
    let profile = load();
    for (alias, canonical) in [
        ("normal", "hdc-normal"),
        ("loader", "rockusb-loader"),
        ("maskrom", "rockusb-maskrom"),
    ] {
        let mode = DeviceMode::new(alias).unwrap();
        assert_eq!(
            profile.mode(&mode).map(|declared| declared.id.as_str()),
            Some(canonical),
            "alias {alias}"
        );
    }

    let enter = profile
        .transition(
            &DeviceMode::new("hdc-normal").unwrap(),
            &DeviceMode::new("rockusb-loader").unwrap(),
        )
        .expect("the enter-updater transition must be declared");
    assert!(enter.rebind.require_disconnect);
    assert!(enter.rebind.tolerate_transient_malformed);
    assert!(enter.rebind.tolerance_window_ms >= 30_000);

    // Returning to HDC-normal is a full boot and gets a much longer window: a
    // single global constant is the flake source architecture.md 8.3 names.
    let back = profile
        .transition(
            &DeviceMode::new("rockusb-loader").unwrap(),
            &DeviceMode::new("hdc-normal").unwrap(),
        )
        .expect("the reboot-to-normal transition must be declared");
    assert!(back.rebind.tolerance_window_ms > enter.rebind.tolerance_window_ms);
}

#[test]
fn the_hardware_revision_set_is_explicit() {
    let profile = load();
    match &profile.hardware_revisions {
        HardwareRevisionPolicy::Allow(revisions) => {
            assert!(!revisions.is_empty());
            assert!(!revisions.iter().any(|revision| revision == "*"));
        }
        other => panic!("a production profile must not use {other:?} without evidence"),
    }
}

#[test]
fn af_v1_declares_no_recovery_coverage_it_cannot_execute() {
    let profile = load();
    assert!(
        !profile.recovery.supports_complete_overwrite,
        "AF-V1 has no durable engine; claiming coverage would be a claim ahead of the code"
    );
    assert!(!profile.recovery.unsupported_states.is_empty());
}

#[test]
fn the_profile_digest_is_reproducible() {
    assert_eq!(load().digest().unwrap(), load().digest().unwrap());
    let identity = load().identity().unwrap();
    assert_eq!(identity.id.as_str(), "org.openharmony.dayu200");
}
