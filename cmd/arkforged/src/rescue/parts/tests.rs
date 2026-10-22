use super::*;
use arkforge_artifact::manifest::{GrammarBranch, PartitionEntryFact};
use arkforge_provider::rockchip_execute::RockUsbWriteProgress;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MutationBehavior {
    Succeed,
    BeforeIo,
    AfterIo,
}

#[derive(Debug)]
struct FakeState {
    writes: usize,
    resets: usize,
    behavior: MutationBehavior,
}

#[derive(Debug, Clone)]
struct FakeBackend {
    device: RescueDevice,
    state: Arc<Mutex<FakeState>>,
}

impl FakeBackend {
    fn new(behavior: MutationBehavior) -> Self {
        let facts_digest = sha256(b"fake-device-facts");
        Self {
            device: RescueDevice {
                device_id: format!("rescue-device:{facts_digest}"),
                facts_digest,
                vendor_id: 0x2207,
                product_id: 0x350a,
                location_id: 0x0112_0000,
                mode: "Loader".into(),
                serial_present: true,
            },
            state: Arc::new(Mutex::new(FakeState {
                writes: 0,
                resets: 0,
                behavior,
            })),
        }
    }
}

impl RescueBackend for FakeBackend {
    fn list_devices(&self) -> Result<Vec<RescueDevice>, RescueError> {
        Ok(vec![self.device.clone()])
    }

    fn open_device(
        &self,
        device_id: &str,
    ) -> Result<(RescueDevice, Box<dyn RockUsbPort>), RescueError> {
        if device_id != self.device.device_id {
            return Err(RescueError::refused(
                "DEVICE_NOT_FOUND",
                "fake device ID did not match",
            ));
        }
        Ok((
            self.device.clone(),
            Box::new(FakePort {
                state: Arc::clone(&self.state),
            }),
        ))
    }
}

#[derive(Debug)]
struct FakePort {
    state: Arc<Mutex<FakeState>>,
}

impl RockUsbPort for FakePort {
    fn discover(&self) -> Result<RockUsbObservation<Vec<RockUsbDevice>>, RockUsbPortFailure> {
        Ok(RockUsbObservation {
            value: Vec::new(),
            evidence_digest: sha256(b"discover"),
        })
    }

    fn capacity_sectors(&self) -> Result<RockUsbObservation<u64>, RockUsbPortFailure> {
        Ok(RockUsbObservation {
            value: 32_000_000,
            evidence_digest: sha256(b"capacity"),
        })
    }

    fn read_partition_table(
        &self,
    ) -> Result<RockUsbObservation<PartitionTableFact>, RockUsbPortFailure> {
        let value = device_table();
        Ok(RockUsbObservation {
            evidence_digest: observed_layout_digest(&value),
            value,
        })
    }

    fn read_sectors(
        &self,
        _begin_sector: u64,
        sectors: u64,
        _scratch: &Path,
    ) -> Result<RockUsbObservation<Vec<u8>>, RockUsbPortFailure> {
        let value = vec![0x5a; (sectors * ROCKUSB_SECTOR_BYTES) as usize];
        Ok(RockUsbObservation {
            evidence_digest: sha256(&value),
            value,
        })
    }

    fn write_partition(
        &self,
        _partition: &str,
        _begin_sector: u64,
        image: &mut arkforge_provider::rockchip_execute::ValidatedImage,
    ) -> Result<RockUsbMutationReceipt, RockUsbPortFailure> {
        let image = image.staged();
        let mut state = self.state.lock().unwrap();
        match state.behavior {
            MutationBehavior::BeforeIo => {
                Err(RockUsbPortFailure::BeforeIo("injected refusal".into()))
            }
            MutationBehavior::AfterIo => {
                state.writes += 1;
                Err(RockUsbPortFailure::AfterIo("injected disconnect".into()))
            }
            MutationBehavior::Succeed => {
                state.writes += 1;
                Ok(RockUsbMutationReceipt {
                    semantic_success: true,
                    evidence_digest: sha256(b"write-csw"),
                    duration_ms: 5,
                    detail: "fake matching CSW".into(),
                    progress: Some(RockUsbWriteProgress {
                        payload_bytes: image.size_bytes,
                        wire_sectors: image.size_bytes.div_ceil(ROCKUSB_SECTOR_BYTES),
                        chunks: 1,
                        chunk_sectors: 1,
                        payload_digest: image.sha256,
                    }),
                })
            }
        }
    }

    fn reset_device(&self) -> Result<RockUsbMutationReceipt, RockUsbPortFailure> {
        let mut state = self.state.lock().unwrap();
        state.resets += 1;
        Ok(RockUsbMutationReceipt {
            semantic_success: true,
            evidence_digest: sha256(b"reset-csw"),
            duration_ms: 1,
            detail: "fake reset".into(),
            progress: None,
        })
    }
}

#[test]
fn rescue_plan_is_canonical_and_requires_exact_effect_tokens() {
    let plan = sample_plan();
    let bytes = plan.to_canonical_bytes().unwrap();
    assert_eq!(RescuePlan::from_canonical_bytes(&bytes).unwrap(), plan);
    assert_eq!(
        plan.plan_id().unwrap(),
        format!("rescue-plan:{}", plan.digest().unwrap())
    );
    assert_eq!(
        plan.required_acknowledgements(),
        vec![
            "rescue:native-rockusb".to_string(),
            "overwrite:partition=boot_linux".to_string()
        ]
    );
    assert!(validate_acknowledgements(&plan, &[]).is_err());
    assert!(
        validate_acknowledgements(
            &plan,
            &["rescue:native-rockusb".into(), "unexpected".into()]
        )
        .is_err()
    );
    assert!(
        validate_acknowledgements(
            &plan,
            &[
                "rescue:native-rockusb".into(),
                "rescue:native-rockusb".into(),
                "overwrite:partition=boot_linux".into(),
            ]
        )
        .is_err()
    );
}

#[test]
fn malformed_operation_bindings_are_rejected_during_plan_decode() {
    let mut write_without_layout = sample_plan();
    write_without_layout.observed_layout_digest = None;
    let bytes = write_without_layout.to_canonical_bytes().unwrap();
    assert_eq!(
        RescuePlan::from_canonical_bytes(&bytes).unwrap_err().code,
        "PLAN_INVALID"
    );

    let mut reset_with_layout = sample_plan();
    reset_with_layout.operation = RescueOperation::ResetDevice;
    let bytes = reset_with_layout.to_canonical_bytes().unwrap();
    assert_eq!(
        RescuePlan::from_canonical_bytes(&bytes).unwrap_err().code,
        "PLAN_INVALID"
    );
}

#[test]
fn rescue_receipt_has_its_own_domain_and_identifier() {
    let receipt = RescueReceipt {
        plan_id: format!("rescue-plan:{}", sha256(b"plan")),
        plan_digest: sha256(b"plan"),
        device_id: format!("rescue-device:{}", sha256(b"facts")),
        operation: "reset-device".into(),
        disposition: RescueDisposition::SemanticSuccess,
        evidence_digest: sha256(b"reset-csw"),
        completed_at_epoch_ms: 2_000,
        detail: "matching reset CSW".into(),
        payload_bytes: None,
        payload_digest: None,
    };
    let bytes = receipt.to_canonical_bytes().unwrap();
    assert_ne!(receipt.digest().unwrap(), sha256(&bytes));
    assert_eq!(
        receipt.receipt_id().unwrap(),
        format!("rescue-receipt:{}", receipt.digest().unwrap())
    );
}

#[test]
fn missing_ack_refuses_before_intent_or_mutation() {
    let fixture = Fixture::new(MutationBehavior::Succeed);
    let summary = fixture.plan_write();
    let error = fixture
        .manager
        .apply(&summary.plan_id, summary.plan_sha256, &[], 1_001)
        .unwrap_err();
    assert_eq!(error.code, "ACKNOWLEDGEMENT_REQUIRED");
    assert_eq!(fixture.state().writes, 0);
    assert!(!fixture.intent_path(summary.plan_sha256).exists());
    fixture.remove();
}

#[test]
fn successful_plan_is_single_use_and_writes_once() {
    let fixture = Fixture::new(MutationBehavior::Succeed);
    let summary = fixture.plan_write();
    let acknowledgements = summary.plan.required_acknowledgements();
    let result = fixture
        .manager
        .apply(
            &summary.plan_id,
            summary.plan_sha256,
            &acknowledgements,
            1_001,
        )
        .unwrap();
    assert_eq!(
        result.receipt.disposition,
        RescueDisposition::SemanticSuccess
    );
    assert_eq!(fixture.state().writes, 1);
    assert!(fixture.intent_path(summary.plan_sha256).exists());

    let replay = fixture
        .manager
        .apply(
            &summary.plan_id,
            summary.plan_sha256,
            &acknowledgements,
            1_002,
        )
        .unwrap_err();
    assert_eq!(replay.code, "RESCUE_PLAN_ALREADY_APPLIED");
    assert_eq!(fixture.state().writes, 1);
    fixture.remove();
}

#[test]
fn post_io_failure_is_outcome_unknown_and_never_replayed() {
    let fixture = Fixture::new(MutationBehavior::AfterIo);
    let summary = fixture.plan_write();
    let acknowledgements = summary.plan.required_acknowledgements();
    let result = fixture
        .manager
        .apply(
            &summary.plan_id,
            summary.plan_sha256,
            &acknowledgements,
            1_001,
        )
        .unwrap();
    assert_eq!(
        result.receipt.disposition,
        RescueDisposition::OutcomeUnknown
    );
    assert_eq!(result.exit_code(), 8);
    assert_eq!(fixture.state().writes, 1);

    let replay = fixture
        .manager
        .apply(
            &summary.plan_id,
            summary.plan_sha256,
            &acknowledgements,
            1_002,
        )
        .unwrap_err();
    assert_eq!(replay.code, "RESCUE_PLAN_ALREADY_APPLIED");
    assert_eq!(fixture.state().writes, 1);
    fixture.remove();
}

#[test]
fn pre_io_failure_has_confirmed_no_effect_but_plan_stays_consumed() {
    let fixture = Fixture::new(MutationBehavior::BeforeIo);
    let summary = fixture.plan_write();
    let acknowledgements = summary.plan.required_acknowledgements();
    let result = fixture
        .manager
        .apply(
            &summary.plan_id,
            summary.plan_sha256,
            &acknowledgements,
            1_001,
        )
        .unwrap();
    assert_eq!(
        result.receipt.disposition,
        RescueDisposition::ConfirmedNoEffect
    );
    assert_eq!(result.exit_code(), 7);
    assert_eq!(fixture.state().writes, 0);
    assert!(fixture.intent_path(summary.plan_sha256).exists());
    fixture.remove();
}

struct Fixture {
    root: PathBuf,
    image: PathBuf,
    manager: RescueManager<FakeBackend>,
    state: Arc<Mutex<FakeState>>,
    device_id: String,
}

impl Fixture {
    fn new(behavior: MutationBehavior) -> Self {
        let root = temp_root();
        fs::create_dir_all(&root).unwrap();
        let image = root.join("boot_linux.img");
        fs::write(&image, vec![0x47; 513]).unwrap();
        let backend = FakeBackend::new(behavior);
        let state = Arc::clone(&backend.state);
        let device_id = backend.device.device_id.clone();
        let manager = RescueManager::new(&root, sha256(b"native-build"), backend).unwrap();
        Self {
            root,
            image,
            manager,
            state,
            device_id,
        }
    }

    fn plan_write(&self) -> RescuePlanSummary {
        self.manager
            .plan_write(
                &self.device_id,
                "boot_linux",
                &self.image,
                sha256(&fs::read(&self.image).unwrap()),
                1_000,
            )
            .unwrap()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().unwrap()
    }

    fn intent_path(&self, digest: Sha256Digest) -> PathBuf {
        self.root
            .join("rescue/intents")
            .join(format!("{digest}.intent.cbor"))
    }

    fn remove(self) {
        fs::remove_dir_all(self.root).unwrap();
    }
}

fn sample_plan() -> RescuePlan {
    let device_facts_digest = sha256(b"facts");
    RescuePlan {
        created_at_epoch_ms: 1_000,
        expires_at_epoch_ms: 901_000,
        device_id: format!("rescue-device:{device_facts_digest}"),
        device_facts_digest,
        profile_id: "org.openharmony.dayu200".into(),
        profile_version: "1.0.0".into(),
        profile_digest: sha256(b"profile"),
        native_build_digest: sha256(b"build"),
        observed_layout_digest: Some(sha256(b"layout")),
        operation: RescueOperation::WritePartition {
            partition: "boot_linux".into(),
            begin_sector: 40_960,
            partition_sectors: 196_608,
            image_digest: sha256(b"image"),
            image_size_bytes: 513,
        },
    }
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
            PartitionEntryFact {
                index: index as u32,
                name: (*name).to_string(),
                offset_sectors: *offset,
                size_sectors: next.map(|next| next - *offset),
                attribute: None,
                grammar_branch: if next.is_some() {
                    GrammarBranch::Fixed
                } else {
                    GrammarBranch::RemainderGrow
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

fn temp_root() -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "arkforge-rescue-test-{}-{nonce}-{}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ))
}
