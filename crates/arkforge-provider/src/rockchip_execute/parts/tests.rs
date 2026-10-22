use super::*;
use std::cell::RefCell;

const PROFILE_SOURCE: &str = include_str!("../../../../../profiles/dayu200.yaml");

fn profile() -> DeviceProfile {
    arkforge_core::profile::load(PROFILE_SOURCE).expect("the shipped profile parses")
}
fn device_table() -> PartitionTableFact {
    let rows = [
        ("uboot", 8_192),
        ("misc", 16_384),
        ("bootctrl", 24_576),
        ("resource", 28_672),
        ("boot_linux", 40_960),
        ("ramdisk", 237_568),
        ("system", 245_760),
        ("vendor", 4_440_064),
        ("sys-prod", 6_537_216),
        ("chip-prod", 6_639_616),
        ("updater", 6_742_016),
        ("eng_system", 6_815_744),
        ("eng_chipset", 6_848_512),
        ("chip_ckm", 6_938_624),
        ("userdata", 19_955_712),
    ];
    let entries = rows
        .iter()
        .enumerate()
        .map(|(index, (name, offset))| {
            let next = rows.get(index + 1).map(|(_, next)| *next);
            arkforge_artifact::manifest::PartitionEntryFact {
                index: index as u32,
                name: (*name).to_string(),
                offset_sectors: *offset,
                size_sectors: next.map(|next| next - *offset),
                attribute: None,
                grammar_branch: if next.is_some() {
                    arkforge_artifact::manifest::GrammarBranch::Fixed
                } else {
                    arkforge_artifact::manifest::GrammarBranch::RemainderGrow
                },
            }
        })
        .collect();
    PartitionTableFact {
        device: "native-rockusb".into(),
        logical_block_size: ROCKUSB_SECTOR_BYTES as u32,
        entries,
    }
}

#[test]
fn native_partition_table_uses_profile_units() {
    let table = device_table();
    assert_eq!(table.entries.len(), 15);
    assert_eq!(table.entries[0].offset_sectors, 8_192);
    assert_eq!(table.entries[6].offset_sectors, 245_760);
    assert_eq!(table.entries[6].size_sectors, Some(4_194_304));
    assert_eq!(table.entries[14].offset_sectors, 19_955_712);
    assert_eq!(table.entries[14].size_sectors, None);
}

/// layout, which is the whole premise of a name-addressed write.
#[test]
fn the_device_and_the_archive_agree_on_every_partition_start() {
    let device = device_table();
    let profile = profile();
    for target in &profile.allowed_targets {
        let entry = device
            .entries
            .iter()
            .find(|entry| entry.name == target.partition.as_str())
            .unwrap_or_else(|| panic!("device has no {}", target.partition));
        assert_eq!(
            entry.offset_sectors, target.offset_sectors,
            "{} start",
            target.partition
        );
    }
    check_conformance(&device, &profile).unwrap();
}

#[test]
fn a_device_partition_the_profile_never_named_is_refused() {
    let mut device = device_table();
    device
        .entries
        .push(arkforge_artifact::manifest::PartitionEntryFact {
            index: 15,
            name: "somebody-elses-partition".into(),
            offset_sectors: 30_000_000,
            size_sectors: None,
            attribute: None,
            grammar_branch: arkforge_artifact::manifest::GrammarBranch::RemainderGrow,
        });
    assert_eq!(
        check_conformance(&device, &profile()),
        Err(ExecutionError::DeviceDeclaresUnknownPartitions(vec![
            "somebody-elses-partition".into()
        ]))
    );
}

fn range() -> ByteRange {
    ByteRange::new(245_760 * 512, 4096).unwrap()
}

/// AD-006, as a test. Filler read through a windowed face is a skip, and
/// the skip is not any grade of verified.
#[test]
fn filler_outside_a_windowed_read_face_is_a_skip_not_a_failure() {
    let read = vec![0xCC; 4096];
    let verdict = classify_readback(
        &read,
        range(),
        sha256(b"whatever the image was"),
        VerificationStrength::FullHash,
        Some(0xCC),
        &MeasuredReadDomain::Windowed {
            detail: "far end read 0xCC".into(),
        },
        false,
    );
    assert!(matches!(
        verdict,
        VerificationOutcome::TypedSkip {
            reason: TypedSkipReason::OutsideReadDomain,
            ..
        }
    ));
    assert_eq!(verdict.verified_strength(), None);
    assert!(!verdict.is_failure());
}

#[test]
fn a_windowed_range_with_a_filler_endpoint_skips_after_one_sector() {
    let root = std::env::temp_dir().join(format!(
        "arkforge-readback-probe-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    let mut record = record_for_test();
    record.declared_range = Some(range());
    record.content_digest = Some(sha256(b"the full image"));
    let port = NativeRecordingPort::default();
    let mut session = ExecutionSession::new(BTreeMap::new());
    session.set_read_domain(MeasuredReadDomain::Windowed {
        detail: "far end read 0xCC".into(),
    });

    let observed = readback_partition(
        &record,
        &mut session,
        &port,
        &root,
        "system",
        245_760,
        VerificationStrength::FullHash,
        Some(0xCC),
    )
    .unwrap();
    assert!(matches!(
        observed.verification,
        Some(VerificationOutcome::TypedSkip {
            reason: TypedSkipReason::OutsideReadDomain,
            ..
        })
    ));
    assert_eq!(port.reads.borrow().as_slice(), &[(245_767, 1)]);
    assert_eq!(
        observed
            .facts
            .iter()
            .find(|(key, _)| key.as_str() == "readbackBytes")
            .map(|(_, value)| value.as_str()),
        Some("512")
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn filler_inside_a_full_read_face_is_a_named_failure_not_a_hash_mismatch() {
    let read = vec![0xCC; 4096];
    let verdict = classify_readback(
        &read,
        range(),
        sha256(b"image"),
        VerificationStrength::FullHash,
        Some(0xCC),
        &MeasuredReadDomain::Full,
        true,
    );
    assert_eq!(
        verdict,
        VerificationOutcome::Failed {
            range: range(),
            classification: FailureClassification::ErasedMediumFiller,
        }
    );
}

#[test]
fn real_content_that_hashes_correctly_verifies_at_the_declared_strength() {
    let mut read = vec![0u8; 4096];
    for (index, byte) in read.iter_mut().enumerate() {
        *byte = (index % 251) as u8;
    }
    let verdict = classify_readback(
        &read,
        range(),
        sha256(&read),
        VerificationStrength::PrefixHash,
        Some(0xCC),
        &MeasuredReadDomain::Full,
        true,
    );
    assert_eq!(
        verdict.verified_strength(),
        Some(VerificationStrength::PrefixHash),
        "a prefix hash may not be reported as anything stronger"
    );
}

#[test]
fn real_content_that_hashes_wrongly_is_a_content_mismatch() {
    let mut read = vec![0u8; 4096];
    read[10] = 7;
    let verdict = classify_readback(
        &read,
        range(),
        sha256(b"something else"),
        VerificationStrength::FullHash,
        Some(0xCC),
        &MeasuredReadDomain::Full,
        true,
    );
    assert_eq!(
        verdict,
        VerificationOutcome::Failed {
            range: range(),
            classification: FailureClassification::ContentMismatch,
        }
    );
}

/// A write whose partition the Profile does not allow must never reach the
/// port. The recording port proves "never reached", not "refused after".
#[test]
fn a_write_to_a_partition_the_profile_protects_never_reaches_usb() {
    let profile = profile();
    let port = NativeRecordingPort::default();
    let mut session = ExecutionSession::new(BTreeMap::new());
    session.set_observed_table(PartitionTableFact {
        device: "native-rockusb".into(),
        logical_block_size: 512,
        entries: Vec::new(),
    });

    let error = write_partition(
        &record_for_test(),
        &mut session,
        &profile,
        &port,
        "misc",
        "misc.img",
        16_384,
    )
    .unwrap_err();
    assert_eq!(error, ExecutionError::TargetNotAllowed("misc".into()));
    assert!(
        port.writes.borrow().is_empty(),
        "native USB was reached for a protected partition"
    );
}

#[test]
fn a_write_before_the_table_was_read_never_reaches_usb() {
    let profile = profile();
    let port = NativeRecordingPort::default();
    let mut session = ExecutionSession::new(BTreeMap::new());
    let error = write_partition(
        &record_for_test(),
        &mut session,
        &profile,
        &port,
        "uboot",
        "uboot.img",
        8192,
    )
    .unwrap_err();
    assert_eq!(error, ExecutionError::TableNotObservedYet);
    assert!(port.writes.borrow().is_empty());
}

#[test]
fn parallel_validation_pins_the_file_and_rechecks_its_inode_fingerprint() {
    let path = std::env::temp_dir().join(format!(
        "arkforge-validated-image-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let bytes = b"one exact staged image";
    std::fs::write(&path, bytes).unwrap();
    let image = StagedImage {
        member: "uboot.img".into(),
        path: path.clone(),
        size_bytes: bytes.len() as u64,
        sha256: sha256(bytes),
    };
    let mut session = ExecutionSession::new(BTreeMap::from([("uboot.img".into(), image)]));
    session.begin_parallel_staged_validation();
    let (mut validated, _wait_ms, parallel) = session.take_validated_image("uboot.img").unwrap();
    assert!(parallel);

    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&path, permissions).unwrap();
    assert!(validated.prepare_for_write().is_err());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        let _ = std::fs::set_permissions(&path, permissions);
    }
    let _ = std::fs::remove_file(path);
}

#[derive(Debug, Default)]
struct NativeRecordingPort {
    reads: RefCell<Vec<(u64, u64)>>,
    writes: RefCell<Vec<(String, u64, PathBuf)>>,
}

impl RockUsbPort for NativeRecordingPort {
    fn discover(&self) -> Result<RockUsbObservation<Vec<RockUsbDevice>>, RockUsbPortFailure> {
        Err(RockUsbPortFailure::BeforeIo("not used by this test".into()))
    }

    fn read_partition_table(
        &self,
    ) -> Result<RockUsbObservation<PartitionTableFact>, RockUsbPortFailure> {
        Err(RockUsbPortFailure::BeforeIo("not used by this test".into()))
    }

    fn read_sectors(
        &self,
        begin_sector: u64,
        sectors: u64,
        _scratch: &Path,
    ) -> Result<RockUsbObservation<Vec<u8>>, RockUsbPortFailure> {
        self.reads.borrow_mut().push((begin_sector, sectors));
        let value = vec![0xCC; sectors as usize * ROCKUSB_SECTOR_BYTES as usize];
        Ok(RockUsbObservation {
            evidence_digest: sha256(&value),
            value,
        })
    }

    fn write_partition(
        &self,
        partition: &str,
        begin_sector: u64,
        image: &mut ValidatedImage,
    ) -> Result<RockUsbMutationReceipt, RockUsbPortFailure> {
        let image = image.staged();
        self.writes
            .borrow_mut()
            .push((partition.to_string(), begin_sector, image.path.clone()));
        let bytes = std::fs::read(&image.path).unwrap();
        Ok(RockUsbMutationReceipt {
            semantic_success: true,
            evidence_digest: sha256(b"typed native write receipt"),
            duration_ms: 7,
            detail: "native WRITE_LBA confirmed".into(),
            progress: Some(RockUsbWriteProgress {
                payload_bytes: bytes.len() as u64,
                wire_sectors: ceil_div(bytes.len() as u64, ROCKUSB_SECTOR_BYTES),
                chunks: 1,
                chunk_sectors: 1,
                payload_digest: sha256(&bytes),
            }),
        })
    }
}

#[test]
fn native_write_uses_the_observed_lba_and_projects_typed_progress() {
    let root = std::env::temp_dir().join(format!(
        "arkforge-native-write-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let image_path = root.join("uboot.img");
    let bytes = vec![0xA5; 513];
    std::fs::write(&image_path, &bytes).unwrap();

    let mut session = ExecutionSession::new(BTreeMap::new());
    session.set_observed_table(device_table());
    session.stage(
        "uboot.img".into(),
        StagedImage {
            member: "uboot.img".into(),
            path: image_path.clone(),
            size_bytes: bytes.len() as u64,
            sha256: sha256(&bytes),
        },
    );
    let port = NativeRecordingPort::default();

    let outcome = write_partition(
        &record_for_test(),
        &mut session,
        &profile(),
        &port,
        "uboot",
        "uboot.img",
        8192,
    )
    .unwrap();

    assert_eq!(outcome.disposition, ActionDisposition::SemanticSuccess);
    assert_eq!(
        port.writes.borrow().as_slice(),
        &[("uboot".into(), 8192, image_path)]
    );
    let value = |key: &str| {
        outcome
            .facts
            .iter()
            .find(|(candidate, _)| candidate.as_str() == key)
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(value("writePayloadBytes"), Some("513"));
    assert_eq!(value("writeWireSectors"), Some("2"));
    assert_eq!(value("writeChunks"), Some("1"));
    let expected_digest = sha256(&bytes).to_hex();
    assert_eq!(value("writePayloadSha256"), Some(expected_digest.as_str()));

    let _ = std::fs::remove_dir_all(root);
}

fn record_for_test() -> PrivateActionRecord {
    use arkforge_core::projection::PrivateActionRole;
    use arkforge_core::step::WorkflowEffect;
    PrivateActionRecord {
        action_id: ActionId::new("ACT-001").unwrap(),
        step_id: StepId::new("STEP-001").unwrap(),
        role: PrivateActionRole::PrimaryEffect,
        effect_class: WorkflowEffect::Destructive,
        declared_target: None,
        declared_range: None,
        content_digest: None,
        body: CborValue::map(vec![]),
    }
}
