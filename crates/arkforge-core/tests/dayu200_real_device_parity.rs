//! Parity against the DAYU200's own answers, measured 2026-08-14.
//!
//! `docs/evidence/runs/2026-08-14-dayu200-read-only-capture.md` records a
//! read-only capture from a real board: its GPT, its USB identities in both
//! modes, and its `rl` read window. This suite pins this repository's decode to
//! what the device actually said.
//!
//! Before this, the DAYU200 parity claim was structural — the fixture had the
//! right shape and the right pinned values, but no device had confirmed them
//! here. Now one has, for the device side of the three-way agreement.

use arkforge_artifact::dayu200;
use arkforge_core::effect::DeviceMode;
use arkforge_core::ids::PartitionId;
use arkforge_core::profile::{self, DeviceProfile, IdentityFieldPolicy};

const CAPTURE: &str =
    include_str!("../../../docs/evidence/runs/2026-08-14-dayu200-read-only-capture.md");

/// The board's own `rkdeveloptool ppt` output, verbatim.
const DEVICE_PPT: &str = "\
**********Partition Info(GPT)**********
NO  LBA       Name
00  00002000  uboot
01  00004000  misc
02  00006000  bootctrl
03  00007000  resource
04  0000A000  boot_linux
05  0003A000  ramdisk
06  0003C000  system
07  0043C000  vendor
08  0063C000  sys-prod
09  00655000  chip-prod
10  0066E000  updater
11  0067E000  eng_system
12  00686000  eng_chipset
13  0069E000  chip_ckm
14  01308000  userdata
";

/// The `parameter.txt` CMDLINE this repository decodes.
const PINNED_CMDLINE: &str = arkforge_artifact::fixture::PINNED_CMDLINE;

fn load() -> DeviceProfile {
    profile::load(include_str!("../../../profiles/dayu200.yaml")).unwrap()
}

/// `(name, lba)` as the device reported them.
fn device_table() -> Vec<(String, u64)> {
    DEVICE_PPT
        .lines()
        .skip(2)
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let lba = u64::from_str_radix(fields[1], 16).expect("ppt prints hex LBAs");
            (fields[2].to_string(), lba)
        })
        .collect()
}

#[test]
fn the_parsed_partition_table_matches_the_device_value_for_value() {
    // The archive's `parameter.txt` decode versus what the board's own GPT
    // says. These are two independent sources and they agree exactly.
    let parsed = dayu200::parse_parameter(PINNED_CMDLINE, 512).unwrap();
    let device = device_table();

    assert_eq!(
        parsed.entries.len(),
        device.len(),
        "the archive declares {} partitions, the device has {}",
        parsed.entries.len(),
        device.len()
    );

    for (index, (name, lba)) in device.iter().enumerate() {
        let entry = &parsed.entries[index];
        assert_eq!(&entry.name, name, "entry {index} name");
        assert_eq!(
            entry.offset_sectors, *lba,
            "{name}: archive says {}, device says {lba}",
            entry.offset_sectors
        );
    }
}

#[test]
fn every_writable_target_exists_on_the_device() {
    let profile = load();
    let device = device_table();
    for target in &profile.allowed_targets {
        let found = device
            .iter()
            .find(|(name, _)| name == target.partition.as_str())
            .unwrap_or_else(|| {
                panic!("{} is writable but the device has no such partition", target.partition)
            });
        assert_eq!(
            found.1, target.offset_sectors,
            "{}: profile offset disagrees with the device",
            target.partition
        );
    }
}

#[test]
fn every_protected_target_exists_on_the_device_and_is_not_writable() {
    // The membership-less set is not a guess about what might be there: each
    // one is a partition the board really has.
    let profile = load();
    let device = device_table();
    for protected in &profile.protected_targets {
        assert!(
            device.iter().any(|(name, _)| name == protected.as_str()),
            "{protected} is protected but the device has no such partition"
        );
        assert!(
            profile
                .allowed_target(&PartitionId::new(protected.as_str()).unwrap())
                .is_none(),
            "{protected} must not also be writable"
        );
    }
    // Together they account for the whole device table.
    assert_eq!(
        profile.allowed_targets.len() + profile.protected_targets.len(),
        device.len(),
        "every partition on the device is either writable or protected"
    );
}

#[test]
fn both_measured_usb_identities_are_declared() {
    let profile = load();
    // HDC-normal, measured; agrees with ArkDeck's `dayu200NormalProductID`.
    assert_eq!(
        profile
            .mode_for_usb_identity(0x2207, 0x5000)
            .map(|mode| mode.as_str()),
        Some("hdc-normal")
    );
    // Loader, measured after `hdc target boot loader`.
    assert_eq!(
        profile
            .mode_for_usb_identity(0x2207, 0x350a)
            .map(|mode| mode.as_str()),
        Some("rockusb-loader")
    );
    // An identity nobody measured names no mode — which is why the capture
    // reported "recognizes 0 of them" before the Loader identity was added.
    assert_eq!(profile.mode_for_usb_identity(0x2207, 0xffff), None);
    assert_eq!(profile.mode_for_usb_identity(0x1234, 0x5000), None);
}

#[test]
fn the_mode_transitions_allow_the_identity_drift_the_board_actually_shows() {
    // Measured: the board changes both its serial and its port path when it
    // enters Loader. A policy of must-match here would reject a healthy device.
    let profile = load();
    let enter = profile
        .transition(
            &DeviceMode::new("hdc-normal").unwrap(),
            &DeviceMode::new("rockusb-loader").unwrap(),
        )
        .unwrap();
    assert_eq!(enter.serial_policy, IdentityFieldPolicy::MayChange);
    assert_eq!(enter.topology_policy, IdentityFieldPolicy::MayChange);

    let back = profile
        .transition(
            &DeviceMode::new("rockusb-loader").unwrap(),
            &DeviceMode::new("hdc-normal").unwrap(),
        )
        .unwrap();
    assert_eq!(back.serial_policy, IdentityFieldPolicy::MayChange);
    assert_eq!(back.topology_policy, IdentityFieldPolicy::MayChange);
}

#[test]
fn the_profile_still_refuses_to_pin_the_observed_read_window() {
    // Two independent sessions ten days apart both saw the boundary at sector
    // 65536. That is still not a licence to make it a constant: the Profile
    // says the read face is measured every execution.
    let profile = load();
    assert_eq!(
        profile.read_domain.read,
        arkforge_core::verification::ReadDomainDeclaration::CharacterizeAtRuntime
    );
    let source = include_str!("../../../profiles/dayu200.yaml");
    for line in source.lines() {
        let statement = line.split('#').next().unwrap_or("");
        assert!(
            !statement.contains("65536"),
            "the profile must not pin a measured window size: {line}"
        );
    }
}

#[test]
fn the_capture_records_that_nothing_was_written() {
    // The evidence file is part of the claim. If a future capture adds a write,
    // this assertion is where it has to be argued for.
    assert!(CAPTURE.contains("destructive dispatch = 0"));
    assert!(CAPTURE.contains("未写入任何分区"));
    for forbidden in ["wlx ", "rkdeveloptool wl "] {
        assert!(
            !CAPTURE.contains(forbidden),
            "the capture must not record a write command: {forbidden}"
        );
    }
}

#[test]
fn the_capture_records_the_read_window_boundary_and_its_meaning() {
    // The finding that matters most: the board was running from `system` and
    // `vendor` while `rl` answered uniform 0xCC for both.
    assert!(CAPTURE.contains("65536"));
    assert!(CAPTURE.contains("uniform `0xCC`"));
    assert!(
        CAPTURE.contains("不等于「未写入」"),
        "the capture must state what uniform filler past the window does not mean"
    );
    assert!(CAPTURE.contains("OpenHarmony-7.0.0.37"));
}
