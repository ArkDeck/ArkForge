//! Explicit native RockUSB rescue.
//!
//! Rescue is deliberately separate from normal [`crate::jobs`] execution. It
//! has no ArkDeck authority, does not mint a `FlashPlan`, and cannot produce a
//! normal action receipt. Its safety contract is instead:
//!
//! - inspect one exact Loader observation;
//! - seal device, profile, layout, image and native-build facts into a plan;
//! - require the exact plan digest and acknowledgement set at apply;
//! - durably record a single-use intent before mutating USB I/O;
//! - never replay a plan after an interrupted or unknown outcome.

use crate::dispatch::{NativeRockUsbPort, device_from_descriptor};
use arkforge_artifact::cas::{CasQuota, ContentAddressedStore};
use arkforge_artifact::manifest::PartitionTableFact;
use arkforge_core::digest::{
    CanonicalCbor, CborValue, Domain, Sha256Digest, decode_canonical, digest_in_domain, sha256,
};
use arkforge_core::profile::DeviceProfile;
use arkforge_provider::rockchip_execute::{
    ROCKUSB_SECTOR_BYTES, RockUsbDevice, RockUsbLocation, RockUsbMutationReceipt,
    RockUsbObservation, RockUsbPort, RockUsbPortFailure, StagedImage, observed_layout_digest,
    validate_partition_table_for_profile,
};
use core::fmt;
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const RESCUE_PLAN_SCHEMA: &str = "arkforge.rescue-plan/v1";
const RESCUE_RECEIPT_SCHEMA: &str = "arkforge.rescue-receipt/v1";
const RESCUE_INTENT_SCHEMA: &str = "arkforge.rescue-intent/v1";
const PLAN_LIFETIME: Duration = Duration::from_secs(15 * 60);
const MAX_READ_BYTES: u64 = 512 * 1024 * 1024;
const PROFILE_SOURCE: &str = include_str!("../../../profiles/dayu200.yaml");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RescueError {
    pub code: &'static str,
    pub message: String,
    pub exit_code: i32,
    pub retryable: bool,
}

impl RescueError {
    fn new(code: &'static str, message: impl Into<String>, exit_code: i32) -> Self {
        Self {
            code,
            message: message.into(),
            exit_code,
            retryable: false,
        }
    }

    fn retryable(mut self) -> Self {
        self.retryable = true;
        self
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new("INVALID_ARGUMENT", message, 2)
    }

    fn refused(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(code, message, 3)
    }

    fn io(context: &str, error: impl fmt::Display) -> Self {
        Self::new("RESCUE_STORE_IO", format!("{context}: {error}"), 10)
    }
}

impl fmt::Display for RescueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for RescueError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RescueDevice {
    pub device_id: String,
    pub facts_digest: Sha256Digest,
    pub vendor_id: u16,
    pub product_id: u16,
    pub location_id: u32,
    pub mode: String,
    pub serial_present: bool,
}

impl RescueDevice {
    fn from_rockusb(device: &RockUsbDevice) -> Result<Self, RescueError> {
        let facts = device_facts_value(device);
        let bytes = facts
            .to_canonical_bytes()
            .map_err(|error| RescueError::io("encode rescue device facts", error))?;
        let digest = digest_in_domain(Domain::DeviceFacts, &bytes);
        let RockUsbLocation::IokitTopology(location_id) = device.location;
        Ok(Self {
            device_id: format!("rescue-device:{}", digest),
            facts_digest: digest,
            vendor_id: device.vendor_id,
            product_id: device.product_id,
            location_id,
            mode: device.mode.clone(),
            serial_present: device.serial.is_some(),
        })
    }
}

fn device_facts_value(device: &RockUsbDevice) -> CborValue {
    let RockUsbLocation::IokitTopology(location_id) = device.location;
    CborValue::map(vec![
        ("vendorId", CborValue::Unsigned(device.vendor_id as u64)),
        ("productId", CborValue::Unsigned(device.product_id as u64)),
        (
            "usbSpecification",
            device
                .usb_specification
                .map(|value| CborValue::Unsigned(value as u64))
                .unwrap_or(CborValue::Null),
        ),
        (
            "deviceRelease",
            device
                .device_release
                .map(|value| CborValue::Unsigned(value as u64))
                .unwrap_or(CborValue::Null),
        ),
        ("locationId", CborValue::Unsigned(location_id as u64)),
        ("mode", CborValue::text(device.mode.clone())),
        (
            "serial",
            device
                .serial
                .as_ref()
                .map(|value| CborValue::text(value.clone()))
                .unwrap_or(CborValue::Null),
        ),
        (
            "productName",
            device
                .product_name
                .as_ref()
                .map(|value| CborValue::text(value.clone()))
                .unwrap_or(CborValue::Null),
        ),
        (
            "vendorName",
            device
                .vendor_name
                .as_ref()
                .map(|value| CborValue::text(value.clone()))
                .unwrap_or(CborValue::Null),
        ),
    ])
}

pub trait RescueBackend: fmt::Debug {
    fn list_devices(&self) -> Result<Vec<RescueDevice>, RescueError>;

    fn open_device(
        &self,
        device_id: &str,
    ) -> Result<(RescueDevice, Box<dyn RockUsbPort>), RescueError>;
}

#[derive(Debug, Default)]
pub struct NativeRescueBackend;

impl NativeRescueBackend {
    pub fn new() -> Self {
        Self
    }

    fn descriptors(&self) -> Result<Vec<arkforge_usb::UsbInterfaceDescriptor>, RescueError> {
        NativeRockUsbPort::new()
            .matching_descriptors()
            .map_err(|error| port_refusal("enumerate Loader devices", error))
    }
}

impl RescueBackend for NativeRescueBackend {
    fn list_devices(&self) -> Result<Vec<RescueDevice>, RescueError> {
        let mut devices = Vec::new();
        for descriptor in self.descriptors()? {
            devices.push(RescueDevice::from_rockusb(&device_from_descriptor(
                descriptor,
            ))?);
        }
        devices.sort_by(|left, right| left.device_id.cmp(&right.device_id));
        Ok(devices)
    }

    fn open_device(
        &self,
        device_id: &str,
    ) -> Result<(RescueDevice, Box<dyn RockUsbPort>), RescueError> {
        for descriptor in self.descriptors()? {
            let rockusb = device_from_descriptor(descriptor.clone());
            let device = RescueDevice::from_rockusb(&rockusb)?;
            if device.device_id == device_id {
                let port = NativeRockUsbPort::for_descriptor(descriptor)
                    .map_err(|error| port_refusal("bind exact Loader device", error))?;
                // Descriptor identity alone is not readiness. Ask the exact
                // Loader before returning a port that can be used to plan.
                port.discover()
                    .map_err(|error| port_refusal("confirm Loader readiness", error))?;
                return Ok((device, Box::new(port)));
            }
        }
        Err(RescueError::refused(
            "DEVICE_NOT_FOUND",
            format!(
                "No current Loader observation matches {device_id}. Run 'arkforge rescue list' and select one exact ID."
            ),
        ))
    }
}

fn port_refusal(operation: &str, failure: RockUsbPortFailure) -> RescueError {
    match failure {
        RockUsbPortFailure::BeforeIo(message) => RescueError::refused(
            "NATIVE_USB_REFUSED",
            format!("Unable to {operation}: {message}"),
        )
        .retryable(),
        RockUsbPortFailure::AfterIo(message) => RescueError::new(
            "NATIVE_USB_READ_FAILED",
            format!("Unable to {operation} after USB I/O began: {message}"),
            7,
        )
        .retryable(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RescueInspection {
    pub device: RescueDevice,
    pub capacity_sectors: u64,
    pub capacity_evidence_digest: Sha256Digest,
    pub table: PartitionTableFact,
    pub layout_digest: Sha256Digest,
    pub layout_evidence_digest: Sha256Digest,
    pub profile_compatible: bool,
    pub profile_blocker: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RescueReadReceipt {
    pub device: RescueDevice,
    pub begin_sector: u64,
    pub sector_count: u64,
    pub bytes: u64,
    pub sha256: Sha256Digest,
    pub output: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RescueOperation {
    WritePartition {
        partition: String,
        begin_sector: u64,
        partition_sectors: u64,
        image_digest: Sha256Digest,
        image_size_bytes: u64,
    },
    ResetDevice,
}

impl RescueOperation {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::WritePartition { .. } => "write-partition",
            Self::ResetDevice => "reset-device",
        }
    }

    fn acknowledgements(&self) -> Vec<String> {
        let mut values = vec!["rescue:native-rockusb".to_string()];
        match self {
            Self::WritePartition { partition, .. } => {
                values.push(format!("overwrite:partition={partition}"));
            }
            Self::ResetDevice => values.push("reset:device".into()),
        }
        values
    }

    fn to_value(&self) -> CborValue {
        match self {
            Self::WritePartition {
                partition,
                begin_sector,
                partition_sectors,
                image_digest,
                image_size_bytes,
            } => CborValue::map(vec![
                ("kind", CborValue::text(self.as_str())),
                ("partition", CborValue::text(partition.clone())),
                ("beginSector", CborValue::Unsigned(*begin_sector)),
                ("partitionSectors", CborValue::Unsigned(*partition_sectors)),
                ("imageDigest", image_digest.to_cbor()),
                ("imageSizeBytes", CborValue::Unsigned(*image_size_bytes)),
            ]),
            Self::ResetDevice => CborValue::map(vec![("kind", CborValue::text(self.as_str()))]),
        }
    }

    fn from_value(value: &CborValue) -> Result<Self, RescueError> {
        let entries = value_map(value, "operation")?;
        let kind = text_field(entries, "kind")?;
        match kind {
            "write-partition" => {
                ensure_fields(
                    entries,
                    &[
                        "kind",
                        "partition",
                        "beginSector",
                        "partitionSectors",
                        "imageDigest",
                        "imageSizeBytes",
                    ],
                    "write-partition operation",
                )?;
                Ok(Self::WritePartition {
                    partition: text_field(entries, "partition")?.to_string(),
                    begin_sector: unsigned_field(entries, "beginSector")?,
                    partition_sectors: unsigned_field(entries, "partitionSectors")?,
                    image_digest: digest_field(entries, "imageDigest")?,
                    image_size_bytes: unsigned_field(entries, "imageSizeBytes")?,
                })
            }
            "reset-device" => {
                ensure_fields(entries, &["kind"], "reset-device operation")?;
                Ok(Self::ResetDevice)
            }
            other => Err(RescueError::refused(
                "PLAN_UNSUPPORTED",
                format!("Rescue plan names unsupported operation {other:?}."),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RescuePlan {
    pub created_at_epoch_ms: u64,
    pub expires_at_epoch_ms: u64,
    pub device_id: String,
    pub device_facts_digest: Sha256Digest,
    pub profile_id: String,
    pub profile_version: String,
    pub profile_digest: Sha256Digest,
    pub native_build_digest: Sha256Digest,
    pub observed_layout_digest: Option<Sha256Digest>,
    pub operation: RescueOperation,
}

impl CanonicalCbor for RescuePlan {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("schemaVersion", CborValue::text(RESCUE_PLAN_SCHEMA)),
            (
                "createdAtEpochMs",
                CborValue::Unsigned(self.created_at_epoch_ms),
            ),
            (
                "expiresAtEpochMs",
                CborValue::Unsigned(self.expires_at_epoch_ms),
            ),
            ("deviceId", CborValue::text(self.device_id.clone())),
            ("deviceFactsDigest", self.device_facts_digest.to_cbor()),
            ("profileId", CborValue::text(self.profile_id.clone())),
            (
                "profileVersion",
                CborValue::text(self.profile_version.clone()),
            ),
            ("profileDigest", self.profile_digest.to_cbor()),
            ("nativeBuildDigest", self.native_build_digest.to_cbor()),
            (
                "observedLayoutDigest",
                self.observed_layout_digest
                    .map(|digest| digest.to_cbor())
                    .unwrap_or(CborValue::Null),
            ),
            ("operation", self.operation.to_value()),
        ])
    }
}

impl RescuePlan {
    pub fn digest(&self) -> Result<Sha256Digest, RescueError> {
        let bytes = self
            .to_canonical_bytes()
            .map_err(|error| RescueError::io("encode rescue plan", error))?;
        Ok(digest_in_domain(Domain::RescuePlan, &bytes))
    }

    pub fn plan_id(&self) -> Result<String, RescueError> {
        Ok(format!("rescue-plan:{}", self.digest()?))
    }

    pub fn required_acknowledgements(&self) -> Vec<String> {
        self.operation.acknowledgements()
    }

    fn validate_shape(&self) -> Result<(), RescueError> {
        if self.expires_at_epoch_ms
            != self
                .created_at_epoch_ms
                .checked_add(PLAN_LIFETIME.as_millis() as u64)
                .ok_or_else(|| {
                    RescueError::refused("PLAN_INVALID", "Rescue plan lifetime overflows.")
                })?
        {
            return Err(RescueError::refused(
                "PLAN_INVALID",
                "The rescue plan does not carry the exact v1 lifetime.",
            ));
        }
        if self.device_id != format!("rescue-device:{}", self.device_facts_digest) {
            return Err(RescueError::refused(
                "PLAN_INVALID",
                "The rescue plan device ID does not match its sealed device facts digest.",
            ));
        }
        if self.profile_id.is_empty() || self.profile_version.is_empty() {
            return Err(RescueError::refused(
                "PLAN_INVALID",
                "The rescue plan profile binding is empty.",
            ));
        }
        match &self.operation {
            RescueOperation::WritePartition {
                partition,
                begin_sector,
                partition_sectors,
                image_size_bytes,
                ..
            } => {
                if partition.is_empty()
                    || *partition_sectors == 0
                    || *image_size_bytes == 0
                    || self.observed_layout_digest.is_none()
                    || begin_sector.checked_add(*partition_sectors).is_none()
                {
                    return Err(RescueError::refused(
                        "PLAN_INVALID",
                        "Write rescue plan has an empty or overflowing target, image, or layout binding.",
                    ));
                }
                let image_sectors = image_size_bytes / ROCKUSB_SECTOR_BYTES
                    + u64::from(!image_size_bytes.is_multiple_of(ROCKUSB_SECTOR_BYTES));
                if image_sectors > *partition_sectors {
                    return Err(RescueError::refused(
                        "PLAN_INVALID",
                        "Write rescue plan image exceeds its sealed partition extent.",
                    ));
                }
            }
            RescueOperation::ResetDevice if self.observed_layout_digest.is_some() => {
                return Err(RescueError::refused(
                    "PLAN_INVALID",
                    "Reset rescue plan must not claim a partition layout binding.",
                ));
            }
            RescueOperation::ResetDevice => {}
        }
        Ok(())
    }

    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, RescueError> {
        let value = decode_canonical(bytes).map_err(|error| {
            RescueError::refused("PLAN_INVALID", format!("Rescue plan is invalid: {error}"))
        })?;
        let entries = value_map(&value, "rescue plan")?;
        ensure_fields(
            entries,
            &[
                "schemaVersion",
                "createdAtEpochMs",
                "expiresAtEpochMs",
                "deviceId",
                "deviceFactsDigest",
                "profileId",
                "profileVersion",
                "profileDigest",
                "nativeBuildDigest",
                "observedLayoutDigest",
                "operation",
            ],
            "rescue plan",
        )?;
        if text_field(entries, "schemaVersion")? != RESCUE_PLAN_SCHEMA {
            return Err(RescueError::refused(
                "PLAN_UNSUPPORTED",
                "Rescue plan schema is not supported by this build.",
            ));
        }
        let observed_layout_digest = match field(entries, "observedLayoutDigest")? {
            CborValue::Null => None,
            value => Some(digest_value(value, "observedLayoutDigest")?),
        };
        let operation = RescueOperation::from_value(field(entries, "operation")?)?;
        let plan = Self {
            created_at_epoch_ms: unsigned_field(entries, "createdAtEpochMs")?,
            expires_at_epoch_ms: unsigned_field(entries, "expiresAtEpochMs")?,
            device_id: text_field(entries, "deviceId")?.to_string(),
            device_facts_digest: digest_field(entries, "deviceFactsDigest")?,
            profile_id: text_field(entries, "profileId")?.to_string(),
            profile_version: text_field(entries, "profileVersion")?.to_string(),
            profile_digest: digest_field(entries, "profileDigest")?,
            native_build_digest: digest_field(entries, "nativeBuildDigest")?,
            observed_layout_digest,
            operation,
        };
        plan.validate_shape()?;
        let reencoded = plan
            .to_canonical_bytes()
            .map_err(|error| RescueError::io("re-encode rescue plan", error))?;
        if reencoded != bytes {
            return Err(RescueError::refused(
                "PLAN_INVALID",
                "Rescue plan does not round-trip byte for byte.",
            ));
        }
        Ok(plan)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RescuePlanSummary {
    pub plan_id: String,
    pub plan_sha256: Sha256Digest,
    pub plan: RescuePlan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RescueDisposition {
    SemanticSuccess,
    ConfirmedNoEffect,
    OutcomeUnknown,
}

impl RescueDisposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SemanticSuccess => "semantic-success",
            Self::ConfirmedNoEffect => "confirmed-no-effect",
            Self::OutcomeUnknown => "outcome-unknown",
        }
    }

    pub fn exit_code(self) -> i32 {
        match self {
            Self::SemanticSuccess => 0,
            Self::ConfirmedNoEffect => 7,
            Self::OutcomeUnknown => 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RescueReceipt {
    pub plan_id: String,
    pub plan_digest: Sha256Digest,
    pub device_id: String,
    pub operation: String,
    pub disposition: RescueDisposition,
    pub evidence_digest: Sha256Digest,
    pub completed_at_epoch_ms: u64,
    pub detail: String,
    pub payload_bytes: Option<u64>,
    pub payload_digest: Option<Sha256Digest>,
}

impl CanonicalCbor for RescueReceipt {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("schemaVersion", CborValue::text(RESCUE_RECEIPT_SCHEMA)),
            ("planId", CborValue::text(self.plan_id.clone())),
            ("planDigest", self.plan_digest.to_cbor()),
            ("deviceId", CborValue::text(self.device_id.clone())),
            ("operation", CborValue::text(self.operation.clone())),
            ("disposition", CborValue::text(self.disposition.as_str())),
            ("evidenceDigest", self.evidence_digest.to_cbor()),
            (
                "completedAtEpochMs",
                CborValue::Unsigned(self.completed_at_epoch_ms),
            ),
            ("detail", CborValue::text(self.detail.clone())),
            (
                "payloadBytes",
                self.payload_bytes
                    .map(CborValue::Unsigned)
                    .unwrap_or(CborValue::Null),
            ),
            (
                "payloadDigest",
                self.payload_digest
                    .map(|digest| digest.to_cbor())
                    .unwrap_or(CborValue::Null),
            ),
        ])
    }
}

impl RescueReceipt {
    pub fn digest(&self) -> Result<Sha256Digest, RescueError> {
        let bytes = self
            .to_canonical_bytes()
            .map_err(|error| RescueError::io("encode rescue receipt", error))?;
        Ok(digest_in_domain(Domain::RescueReceipt, &bytes))
    }

    pub fn receipt_id(&self) -> Result<String, RescueError> {
        Ok(format!("rescue-receipt:{}", self.digest()?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RescueApplyResult {
    pub receipt: RescueReceipt,
}

impl RescueApplyResult {
    pub fn exit_code(&self) -> i32 {
        self.receipt.disposition.exit_code()
    }
}

#[derive(Debug)]
pub struct RescueManager<B: RescueBackend> {
    runtime_root: PathBuf,
    profile: DeviceProfile,
    profile_digest: Sha256Digest,
    native_build_digest: Sha256Digest,
    backend: B,
}

impl<B: RescueBackend> RescueManager<B> {
    pub fn new(
        runtime_root: impl Into<PathBuf>,
        native_build_digest: Sha256Digest,
        backend: B,
    ) -> Result<Self, RescueError> {
        let profile = arkforge_core::profile::load(PROFILE_SOURCE).map_err(|error| {
            RescueError::new(
                "SHIPPED_PROFILE_INVALID",
                format!("The shipped DAYU200 profile cannot be loaded: {error}"),
                10,
            )
        })?;
        let profile_digest = profile
            .digest()
            .map_err(|error| RescueError::io("digest shipped DAYU200 profile", error))?;
        Ok(Self {
            runtime_root: runtime_root.into(),
            profile,
            profile_digest,
            native_build_digest,
            backend,
        })
    }

    pub fn list_devices(&self) -> Result<Vec<RescueDevice>, RescueError> {
        self.backend.list_devices()
    }

    pub fn inspect(&self, device_id: &str) -> Result<RescueInspection, RescueError> {
        let (device, port) = self.backend.open_device(device_id)?;
        let capacity = port
            .capacity_sectors()
            .map_err(|error| port_refusal("read device capacity", error))?;
        let table = port
            .read_partition_table()
            .map_err(|error| port_refusal("read the device partition table", error))?;
        let compatibility = validate_partition_table_for_profile(&table.value, &self.profile);
        let (profile_compatible, profile_blocker) = match compatibility {
            Ok(()) => (true, None),
            Err(error) => (false, Some(error.to_string())),
        };
        Ok(RescueInspection {
            device,
            capacity_sectors: capacity.value,
            capacity_evidence_digest: capacity.evidence_digest,
            layout_digest: observed_layout_digest(&table.value),
            layout_evidence_digest: table.evidence_digest,
            table: table.value,
            profile_compatible,
            profile_blocker,
        })
    }

    pub fn read_sectors(
        &self,
        device_id: &str,
        begin_sector: u64,
        sector_count: u64,
        output: &Path,
    ) -> Result<RescueReadReceipt, RescueError> {
        if sector_count == 0 {
            return Err(RescueError::invalid(
                "Use a sector count greater than zero.",
            ));
        }
        let bytes_requested = sector_count
            .checked_mul(ROCKUSB_SECTOR_BYTES)
            .ok_or_else(|| {
                RescueError::invalid("The requested sector range overflows a byte count.")
            })?;
        if bytes_requested > MAX_READ_BYTES {
            return Err(RescueError::refused(
                "READ_TOO_LARGE",
                format!(
                    "One rescue read is limited to {MAX_READ_BYTES} bytes. Split the request into smaller ranges."
                ),
            ));
        }
        let end = begin_sector.checked_add(sector_count).ok_or_else(|| {
            RescueError::invalid("The requested sector range overflows the address space.")
        })?;
        if output.exists() {
            return Err(RescueError::refused(
                "OUTPUT_EXISTS",
                format!(
                    "{} already exists. Choose a new --out path; rescue read never overwrites a file.",
                    output.display()
                ),
            ));
        }
        if let Some(parent) = output.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            return Err(RescueError::refused(
                "OUTPUT_PARENT_MISSING",
                format!("Create {} before reading.", parent.display()),
            ));
        }

        let (device, port) = self.backend.open_device(device_id)?;
        let capacity = port
            .capacity_sectors()
            .map_err(|error| port_refusal("read device capacity", error))?;
        if end > capacity.value {
            return Err(RescueError::refused(
                "READ_OUT_OF_RANGE",
                format!(
                    "The device has {} sectors; requested range {begin_sector}+{sector_count} ends at {end}.",
                    capacity.value
                ),
            ));
        }
        let scratch = self.rescue_root().join("read-scratch");
        create_private_dir(&scratch)?;
        let observation = port
            .read_sectors(begin_sector, sector_count, &scratch)
            .map_err(|error| port_refusal("read sectors", error))?;
        if observation.value.len() as u64 != bytes_requested {
            return Err(RescueError::new(
                "NATIVE_READ_LENGTH_MISMATCH",
                format!(
                    "Native RockUSB returned {} bytes; this request requires exactly {bytes_requested}.",
                    observation.value.len()
                ),
                7,
            ));
        }
        let output_digest = sha256(&observation.value);
        if output_digest != observation.evidence_digest {
            return Err(RescueError::new(
                "NATIVE_READ_EVIDENCE_MISMATCH",
                format!(
                    "Native read evidence names {}; returned bytes hash to {output_digest}.",
                    observation.evidence_digest
                ),
                7,
            ));
        }
        let mut file = create_private_file_new(output)
            .map_err(|error| RescueError::io(&format!("create {}", output.display()), error))?;
        if let Err(error) = file
            .write_all(&observation.value)
            .and_then(|_| file.sync_all())
        {
            drop(file);
            let _ = fs::remove_file(output);
            return Err(RescueError::io(
                &format!("write {}", output.display()),
                error,
            ));
        }
        Ok(RescueReadReceipt {
            device,
            begin_sector,
            sector_count,
            bytes: observation.value.len() as u64,
            sha256: output_digest,
            output: output.to_path_buf(),
        })
    }

    pub fn plan_write(
        &self,
        device_id: &str,
        partition: &str,
        image_path: &Path,
        expected_image_digest: Sha256Digest,
        now_epoch_ms: u64,
    ) -> Result<RescuePlanSummary, RescueError> {
        let target = self
            .profile
            .allowed_targets
            .iter()
            .find(|target| target.partition.as_str() == partition)
            .ok_or_else(|| {
                RescueError::refused(
                    "TARGET_NOT_ALLOWED",
                    format!(
                        "The shipped profile does not allow rescue writes to {partition:?}. Choose one of its allowed targets."
                    ),
                )
            })?;
        let image_size = fs::metadata(image_path)
            .map_err(|error| RescueError::io(&format!("read {}", image_path.display()), error))?
            .len();
        if image_size == 0 {
            return Err(RescueError::refused(
                "IMAGE_EMPTY",
                "The rescue image is empty. Choose a non-empty image.",
            ));
        }

        let inspection = self.inspect(device_id)?;
        if !inspection.profile_compatible {
            return Err(RescueError::refused(
                "LAYOUT_NOT_ALLOWED",
                inspection.profile_blocker.unwrap_or_else(|| {
                    "The device partition table does not match the shipped profile.".into()
                }),
            ));
        }
        let entry = inspection
            .table
            .entries
            .iter()
            .find(|entry| entry.name == partition)
            .ok_or_else(|| {
                RescueError::refused(
                    "PARTITION_NOT_FOUND",
                    format!("The device declares no partition named {partition:?}."),
                )
            })?;
        if entry.offset_sectors != target.offset_sectors {
            return Err(RescueError::refused(
                "LAYOUT_NOT_ALLOWED",
                format!(
                    "Partition {partition} starts at {} on the device and {} in the shipped profile.",
                    entry.offset_sectors, target.offset_sectors
                ),
            ));
        }
        let partition_sectors = entry.size_sectors.unwrap_or_else(|| {
            inspection
                .capacity_sectors
                .saturating_sub(entry.offset_sectors)
        });
        let image_sectors = image_size / ROCKUSB_SECTOR_BYTES
            + u64::from(!image_size.is_multiple_of(ROCKUSB_SECTOR_BYTES));
        if partition_sectors == 0 || image_sectors > partition_sectors {
            return Err(RescueError::refused(
                "IMAGE_OVERRUNS_PARTITION",
                format!(
                    "The image needs {image_sectors} sectors and partition {partition} has {partition_sectors}."
                ),
            ));
        }

        let store = self.open_store()?;
        let image = File::open(image_path)
            .map_err(|error| RescueError::io(&format!("open {}", image_path.display()), error))?;
        let imported = store
            .import(image, image_size, Some(expected_image_digest))
            .map_err(|error| RescueError::new("IMAGE_IMPORT_REFUSED", error.to_string(), 3))?;

        let plan = RescuePlan {
            created_at_epoch_ms: now_epoch_ms,
            expires_at_epoch_ms: now_epoch_ms.saturating_add(PLAN_LIFETIME.as_millis() as u64),
            device_id: inspection.device.device_id.clone(),
            device_facts_digest: inspection.device.facts_digest,
            profile_id: self.profile.id.to_string(),
            profile_version: self.profile.version.to_string(),
            profile_digest: self.profile_digest,
            native_build_digest: self.native_build_digest,
            observed_layout_digest: Some(inspection.layout_digest),
            operation: RescueOperation::WritePartition {
                partition: partition.to_string(),
                begin_sector: entry.offset_sectors,
                partition_sectors,
                image_digest: imported.digest,
                image_size_bytes: imported.size_bytes,
            },
        };
        self.store_plan(plan)
    }

    pub fn plan_reset(
        &self,
        device_id: &str,
        now_epoch_ms: u64,
    ) -> Result<RescuePlanSummary, RescueError> {
        let (device, _port) = self.backend.open_device(device_id)?;
        let plan = RescuePlan {
            created_at_epoch_ms: now_epoch_ms,
            expires_at_epoch_ms: now_epoch_ms.saturating_add(PLAN_LIFETIME.as_millis() as u64),
            device_id: device.device_id,
            device_facts_digest: device.facts_digest,
            profile_id: self.profile.id.to_string(),
            profile_version: self.profile.version.to_string(),
            profile_digest: self.profile_digest,
            native_build_digest: self.native_build_digest,
            observed_layout_digest: None,
            operation: RescueOperation::ResetDevice,
        };
        self.store_plan(plan)
    }

    pub fn apply(
        &self,
        plan_id: &str,
        expected_plan_digest: Sha256Digest,
        acknowledgements: &[String],
        now_epoch_ms: u64,
    ) -> Result<RescueApplyResult, RescueError> {
        let (plan, plan_digest, plan_bytes) = self.load_plan(plan_id)?;
        if plan_digest != expected_plan_digest {
            return Err(RescueError::new(
                "PLAN_DIGEST_MISMATCH",
                format!(
                    "The stored rescue plan hashes to {plan_digest}; --expect-plan-sha256 supplied {expected_plan_digest}."
                ),
                4,
            ));
        }
        if plan.expires_at_epoch_ms <= now_epoch_ms {
            return Err(RescueError::refused(
                "PLAN_EXPIRED",
                "The rescue plan expired. Inspect the current device and create a new plan.",
            ));
        }
        if plan.created_at_epoch_ms > now_epoch_ms {
            return Err(RescueError::refused(
                "PLAN_NOT_YET_VALID",
                "The rescue plan was created in the future according to this host clock. Correct the clock and create a new plan.",
            ));
        }
        if plan.native_build_digest != self.native_build_digest {
            return Err(RescueError::refused(
                "NATIVE_BUILD_CHANGED",
                format!(
                    "The plan binds native build {}; this command is {}. Create a new rescue plan with this build.",
                    plan.native_build_digest, self.native_build_digest
                ),
            ));
        }
        if plan.profile_digest != self.profile_digest
            || plan.profile_id != self.profile.id.as_str()
            || plan.profile_version != self.profile.version.to_string()
        {
            return Err(RescueError::refused(
                "PROFILE_CHANGED",
                "The shipped profile no longer matches this rescue plan. Create a new plan.",
            ));
        }
        validate_acknowledgements(&plan, acknowledgements)?;

        let planned_write_layout = match &plan.operation {
            RescueOperation::WritePartition { .. } => {
                Some(plan.observed_layout_digest.ok_or_else(|| {
                    RescueError::refused(
                        "PLAN_INVALID",
                        "Write plan carries no observed layout digest.",
                    )
                })?)
            }
            RescueOperation::ResetDevice => None,
        };

        let (device, port) = self.backend.open_device(&plan.device_id)?;
        if device.facts_digest != plan.device_facts_digest {
            return Err(RescueError::refused(
                "DEVICE_CHANGED",
                "The selected Loader observation changed after planning. Create a new rescue plan.",
            ));
        }

        self.record_intent(plan_id, plan_digest, &plan_bytes, now_epoch_ms)?;
        let receipt = match &plan.operation {
            RescueOperation::WritePartition {
                partition,
                begin_sector,
                partition_sectors,
                image_digest,
                image_size_bytes,
            } => self.apply_write(
                plan_id,
                plan_digest,
                &device,
                port.as_ref(),
                partition,
                *begin_sector,
                *partition_sectors,
                *image_digest,
                *image_size_bytes,
                planned_write_layout.ok_or_else(|| {
                    RescueError::refused("PLAN_INVALID", "Write plan layout binding is missing.")
                })?,
                now_epoch_ms,
            ),
            RescueOperation::ResetDevice => {
                self.apply_reset(plan_id, plan_digest, &device, port.as_ref(), now_epoch_ms)
            }
        };
        let receipt = match receipt {
            Ok(receipt) => receipt,
            Err((disposition, detail, evidence_digest)) => RescueReceipt {
                plan_id: plan_id.to_string(),
                plan_digest,
                device_id: device.device_id,
                operation: plan.operation.as_str().to_string(),
                disposition,
                evidence_digest,
                completed_at_epoch_ms: now_epoch_ms,
                detail,
                payload_bytes: None,
                payload_digest: None,
            },
        };
        self.store_receipt(&receipt)?;
        Ok(RescueApplyResult { receipt })
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_write(
        &self,
        plan_id: &str,
        plan_digest: Sha256Digest,
        device: &RescueDevice,
        port: &dyn RockUsbPort,
        partition: &str,
        begin_sector: u64,
        partition_sectors: u64,
        image_digest: Sha256Digest,
        image_size_bytes: u64,
        planned_layout_digest: Sha256Digest,
        now_epoch_ms: u64,
    ) -> Result<RescueReceipt, (RescueDisposition, String, Sha256Digest)> {
        let capacity = mutation_precondition(port.capacity_sectors(), "read device capacity")?;
        let table = mutation_precondition(port.read_partition_table(), "read partition table")?;
        if let Err(error) = validate_partition_table_for_profile(&table.value, &self.profile) {
            return Err(confirmed_no_effect(error.to_string()));
        }
        let current_layout_digest = observed_layout_digest(&table.value);
        if current_layout_digest != planned_layout_digest {
            return Err(confirmed_no_effect(format!(
                "device layout changed from {planned_layout_digest} to {current_layout_digest}"
            )));
        }
        let Some(entry) = table
            .value
            .entries
            .iter()
            .find(|entry| entry.name == partition)
        else {
            return Err(confirmed_no_effect(format!(
                "the device no longer declares partition {partition:?}"
            )));
        };
        let current_sectors = entry
            .size_sectors
            .unwrap_or_else(|| capacity.value.saturating_sub(entry.offset_sectors));
        if entry.offset_sectors != begin_sector || current_sectors != partition_sectors {
            return Err(confirmed_no_effect(format!(
                "partition {partition} changed from {begin_sector}+{partition_sectors} to {}+{current_sectors}",
                entry.offset_sectors
            )));
        }

        let store = self
            .open_store()
            .map_err(|error| confirmed_no_effect(error.to_string()))?;
        if store
            .object_size(&image_digest)
            .map_err(|error| confirmed_no_effect(error.to_string()))?
            != image_size_bytes
            || !store
                .verify_object(&image_digest)
                .map_err(|error| confirmed_no_effect(error.to_string()))?
        {
            return Err(confirmed_no_effect(
                "the staged rescue image no longer matches its content address".into(),
            ));
        }
        let staging_dir = self.work_dir().join(plan_digest.to_string());
        create_private_dir(&staging_dir).map_err(|error| confirmed_no_effect(error.to_string()))?;
        let staging_path = staging_dir.join("image.bin");
        let mut source = store
            .open_object(&image_digest)
            .map_err(|error| confirmed_no_effect(error.to_string()))?;
        let mut target = create_private_file_new(&staging_path)
            .map_err(|error| confirmed_no_effect(error.to_string()))?;
        std::io::copy(&mut source, &mut target)
            .and_then(|_| target.sync_all())
            .map_err(|error| confirmed_no_effect(error.to_string()))?;
        let image = StagedImage {
            member: format!("rescue:{partition}"),
            path: staging_path,
            size_bytes: image_size_bytes,
            sha256: image_digest,
        };
        let mut validated = image
            .open_and_revalidate()
            .map_err(|error| confirmed_no_effect(error.to_string()))?;

        match port.write_partition(partition, begin_sector, &mut validated) {
            Ok(receipt)
                if receipt.semantic_success
                    && receipt.progress.as_ref().is_some_and(|progress| {
                        progress.payload_bytes == image_size_bytes
                            && progress.payload_digest == image_digest
                    }) =>
            {
                Ok(receipt_from_mutation(
                    plan_id,
                    plan_digest,
                    device,
                    "write-partition",
                    receipt,
                    now_epoch_ms,
                ))
            }
            Ok(receipt) => Err(outcome_unknown(format!(
                "Native write returned incomplete semantic evidence: {}",
                receipt.detail
            ))),
            Err(RockUsbPortFailure::BeforeIo(message)) => Err(confirmed_no_effect(message)),
            Err(RockUsbPortFailure::AfterIo(message)) => Err(outcome_unknown(message)),
        }
    }

    fn apply_reset(
        &self,
        plan_id: &str,
        plan_digest: Sha256Digest,
        device: &RescueDevice,
        port: &dyn RockUsbPort,
        now_epoch_ms: u64,
    ) -> Result<RescueReceipt, (RescueDisposition, String, Sha256Digest)> {
        match port.reset_device() {
            Ok(receipt) if receipt.semantic_success && receipt.progress.is_none() => {
                Ok(receipt_from_mutation(
                    plan_id,
                    plan_digest,
                    device,
                    "reset-device",
                    receipt,
                    now_epoch_ms,
                ))
            }
            Ok(receipt) => Err(outcome_unknown(format!(
                "Native reset returned incomplete semantic evidence: {}",
                receipt.detail
            ))),
            Err(RockUsbPortFailure::BeforeIo(message)) => Err(confirmed_no_effect(message)),
            Err(RockUsbPortFailure::AfterIo(message)) => Err(outcome_unknown(message)),
        }
    }

    fn store_plan(&self, plan: RescuePlan) -> Result<RescuePlanSummary, RescueError> {
        plan.validate_shape()?;
        let bytes = plan
            .to_canonical_bytes()
            .map_err(|error| RescueError::io("encode rescue plan", error))?;
        let digest = digest_in_domain(Domain::RescuePlan, &bytes);
        let plan_id = format!("rescue-plan:{digest}");
        let path = self.plans_dir().join(format!("{digest}.cbor"));
        create_private_dir(&self.plans_dir())?;
        write_content_addressed(&path, &bytes)?;
        Ok(RescuePlanSummary {
            plan_id,
            plan_sha256: digest,
            plan,
        })
    }

    fn load_plan(&self, plan_id: &str) -> Result<(RescuePlan, Sha256Digest, Vec<u8>), RescueError> {
        let digest_text = plan_id.strip_prefix("rescue-plan:").ok_or_else(|| {
            RescueError::invalid("Use a rescue plan ID in the form rescue-plan:<sha256>.")
        })?;
        let id_digest = Sha256Digest::parse_hex(digest_text).map_err(|error| {
            RescueError::invalid(format!("Rescue plan ID has an invalid digest: {error}"))
        })?;
        let path = self.plans_dir().join(format!("{id_digest}.cbor"));
        let bytes = fs::read(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                RescueError::new(
                    "PLAN_NOT_FOUND",
                    format!("No stored rescue plan matches {plan_id}."),
                    5,
                )
            } else {
                RescueError::io(&format!("read {}", path.display()), error)
            }
        })?;
        let observed = digest_in_domain(Domain::RescuePlan, &bytes);
        if observed != id_digest {
            return Err(RescueError::refused(
                "PLAN_DIGEST_MISMATCH",
                format!("Stored plan hashes to {observed}; its ID names {id_digest}."),
            ));
        }
        let plan = RescuePlan::from_canonical_bytes(&bytes)?;
        Ok((plan, observed, bytes))
    }

    fn record_intent(
        &self,
        plan_id: &str,
        plan_digest: Sha256Digest,
        plan_bytes: &[u8],
        now_epoch_ms: u64,
    ) -> Result<(), RescueError> {
        create_private_dir(&self.intents_dir())?;
        let path = self
            .intents_dir()
            .join(format!("{plan_digest}.intent.cbor"));
        let intent = CborValue::map(vec![
            ("schemaVersion", CborValue::text(RESCUE_INTENT_SCHEMA)),
            ("planId", CborValue::text(plan_id)),
            ("planDigest", plan_digest.to_cbor()),
            ("planBytesDigest", sha256(plan_bytes).to_cbor()),
            ("recordedAtEpochMs", CborValue::Unsigned(now_epoch_ms)),
        ])
        .to_canonical_bytes()
        .map_err(|error| RescueError::io("encode rescue intent", error))?;
        match create_private_file_new(&path) {
            Ok(mut file) => {
                file.write_all(&intent)
                    .and_then(|_| file.sync_all())
                    .map_err(|error| RescueError::io("persist rescue intent", error))?;
                sync_parent(&path)?;
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(RescueError::new(
                    "RESCUE_PLAN_ALREADY_APPLIED",
                    format!(
                        "{plan_id} already has a durable intent. Do not replay it; inspect the stored receipt or create a new plan after reconciling the device."
                    ),
                    6,
                ))
            }
            Err(error) => Err(RescueError::io("create rescue intent", error)),
        }
    }

    fn store_receipt(&self, receipt: &RescueReceipt) -> Result<(), RescueError> {
        create_private_dir(&self.receipts_dir())?;
        let bytes = receipt
            .to_canonical_bytes()
            .map_err(|error| RescueError::io("encode rescue receipt", error))?;
        let path = self
            .receipts_dir()
            .join(format!("{}.cbor", receipt.plan_digest));
        write_content_addressed(&path, &bytes)
    }

    fn open_store(&self) -> Result<ContentAddressedStore, RescueError> {
        ContentAddressedStore::open(
            self.rescue_root().join("store"),
            CasQuota::dayu200_default(),
        )
        .map_err(|error| RescueError::new("RESCUE_STORE_UNAVAILABLE", error.to_string(), 10))
    }

    fn rescue_root(&self) -> PathBuf {
        self.runtime_root.join("rescue")
    }

    fn plans_dir(&self) -> PathBuf {
        self.rescue_root().join("plans")
    }

    fn intents_dir(&self) -> PathBuf {
        self.rescue_root().join("intents")
    }

    fn receipts_dir(&self) -> PathBuf {
        self.rescue_root().join("receipts")
    }

    fn work_dir(&self) -> PathBuf {
        self.rescue_root().join("work")
    }
}

fn validate_acknowledgements(
    plan: &RescuePlan,
    acknowledgements: &[String],
) -> Result<(), RescueError> {
    let required: BTreeSet<String> = plan.required_acknowledgements().into_iter().collect();
    let supplied: BTreeSet<String> = acknowledgements.iter().cloned().collect();
    if supplied != required || supplied.len() != acknowledgements.len() {
        let missing: Vec<String> = required.difference(&supplied).cloned().collect();
        let unexpected: Vec<String> = supplied.difference(&required).cloned().collect();
        let duplicates = acknowledgements.len().saturating_sub(supplied.len());
        return Err(RescueError::new(
            "ACKNOWLEDGEMENT_REQUIRED",
            format!(
                "Supply exactly the sealed acknowledgement set. Missing: [{}]. Unexpected: [{}]. Duplicate count: {duplicates}.",
                missing.join(", "),
                unexpected.join(", ")
            ),
            4,
        )
        .retryable());
    }
    Ok(())
}

fn mutation_precondition<T>(
    result: Result<RockUsbObservation<T>, RockUsbPortFailure>,
    operation: &str,
) -> Result<RockUsbObservation<T>, (RescueDisposition, String, Sha256Digest)> {
    match result {
        Ok(value) => Ok(value),
        Err(RockUsbPortFailure::BeforeIo(message)) => Err(confirmed_no_effect(format!(
            "Unable to {operation}: {message}"
        ))),
        // The operation is read-only and happened before the mutation, so even
        // a transport failure here confirms the rescue mutation did not begin.
        Err(RockUsbPortFailure::AfterIo(message)) => Err(confirmed_no_effect(format!(
            "Unable to {operation}: {message}"
        ))),
    }
}

fn confirmed_no_effect(message: String) -> (RescueDisposition, String, Sha256Digest) {
    (
        RescueDisposition::ConfirmedNoEffect,
        message.clone(),
        sha256(message.as_bytes()),
    )
}

fn outcome_unknown(message: String) -> (RescueDisposition, String, Sha256Digest) {
    (
        RescueDisposition::OutcomeUnknown,
        message.clone(),
        sha256(message.as_bytes()),
    )
}

fn receipt_from_mutation(
    plan_id: &str,
    plan_digest: Sha256Digest,
    device: &RescueDevice,
    operation: &str,
    receipt: RockUsbMutationReceipt,
    completed_at_epoch_ms: u64,
) -> RescueReceipt {
    let disposition = if receipt.semantic_success {
        RescueDisposition::SemanticSuccess
    } else {
        RescueDisposition::OutcomeUnknown
    };
    let (payload_bytes, payload_digest) = receipt
        .progress
        .as_ref()
        .map(|progress| (Some(progress.payload_bytes), Some(progress.payload_digest)))
        .unwrap_or((None, None));
    RescueReceipt {
        plan_id: plan_id.to_string(),
        plan_digest,
        device_id: device.device_id.clone(),
        operation: operation.to_string(),
        disposition,
        evidence_digest: receipt.evidence_digest,
        completed_at_epoch_ms,
        detail: receipt.detail,
        payload_bytes,
        payload_digest,
    }
}

pub fn now_epoch_ms() -> Result<u64, RescueError> {
    Ok(SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|error| RescueError::new("CLOCK_INVALID", error.to_string(), 10))?
        .as_millis() as u64)
}

fn create_private_dir(path: &Path) -> Result<(), RescueError> {
    fs::create_dir_all(path)
        .map_err(|error| RescueError::io(&format!("create {}", path.display()), error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| RescueError::io(&format!("protect {}", path.display()), error))?;
    }
    Ok(())
}

fn create_private_file_new(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn write_content_addressed(path: &Path, bytes: &[u8]) -> Result<(), RescueError> {
    match create_private_file_new(path) {
        Ok(mut file) => {
            file.write_all(bytes)
                .and_then(|_| file.sync_all())
                .map_err(|error| RescueError::io(&format!("write {}", path.display()), error))?;
            sync_parent(path)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = fs::read(path).map_err(|read_error| {
                RescueError::io(&format!("read {}", path.display()), read_error)
            })?;
            if existing == bytes {
                Ok(())
            } else {
                Err(RescueError::new(
                    "RESCUE_STORE_COLLISION",
                    format!("{} already contains different bytes.", path.display()),
                    10,
                ))
            }
        }
        Err(error) => Err(RescueError::io(
            &format!("create {}", path.display()),
            error,
        )),
    }
}

fn sync_parent(path: &Path) -> Result<(), RescueError> {
    let parent = path
        .parent()
        .ok_or_else(|| RescueError::io("sync parent", "path has no parent"))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| RescueError::io(&format!("sync {}", parent.display()), error))
}

fn value_map<'a>(
    value: &'a CborValue,
    name: &str,
) -> Result<&'a [(CborValue, CborValue)], RescueError> {
    match value {
        CborValue::Map(entries) => Ok(entries),
        _ => Err(RescueError::refused(
            "PLAN_INVALID",
            format!("{name} is not a map."),
        )),
    }
}

fn field<'a>(
    entries: &'a [(CborValue, CborValue)],
    name: &str,
) -> Result<&'a CborValue, RescueError> {
    entries
        .iter()
        .find_map(|(key, value)| match key {
            CborValue::Text(key) if key == name => Some(value),
            _ => None,
        })
        .ok_or_else(|| {
            RescueError::refused("PLAN_INVALID", format!("Plan field {name:?} is missing."))
        })
}

fn text_field<'a>(
    entries: &'a [(CborValue, CborValue)],
    name: &str,
) -> Result<&'a str, RescueError> {
    match field(entries, name)? {
        CborValue::Text(value) => Ok(value),
        _ => Err(RescueError::refused(
            "PLAN_INVALID",
            format!("Plan field {name:?} is not text."),
        )),
    }
}

fn unsigned_field(entries: &[(CborValue, CborValue)], name: &str) -> Result<u64, RescueError> {
    match field(entries, name)? {
        CborValue::Unsigned(value) => Ok(*value),
        _ => Err(RescueError::refused(
            "PLAN_INVALID",
            format!("Plan field {name:?} is not an unsigned integer."),
        )),
    }
}

fn digest_field(
    entries: &[(CborValue, CborValue)],
    name: &str,
) -> Result<Sha256Digest, RescueError> {
    digest_value(field(entries, name)?, name)
}

fn digest_value(value: &CborValue, name: &str) -> Result<Sha256Digest, RescueError> {
    match value {
        CborValue::Bytes(bytes) if bytes.len() == 32 => {
            let mut digest = [0u8; 32];
            digest.copy_from_slice(bytes);
            Ok(Sha256Digest::from_bytes(digest))
        }
        _ => Err(RescueError::refused(
            "PLAN_INVALID",
            format!("Plan field {name:?} is not a 32-byte digest."),
        )),
    }
}

fn ensure_fields(
    entries: &[(CborValue, CborValue)],
    expected: &[&str],
    name: &str,
) -> Result<(), RescueError> {
    let actual: BTreeSet<&str> = entries
        .iter()
        .map(|(key, _)| match key {
            CborValue::Text(key) => Ok(key.as_str()),
            _ => Err(RescueError::refused(
                "PLAN_INVALID",
                format!("{name} contains a non-text field name."),
            )),
        })
        .collect::<Result<_, _>>()?;
    let expected: BTreeSet<&str> = expected.iter().copied().collect();
    if actual != expected {
        return Err(RescueError::refused(
            "PLAN_INVALID",
            format!("{name} contains missing or unknown fields."),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
}
