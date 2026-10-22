use super::*;

pub(super) const RESCUE_PLAN_SCHEMA: &str = "arkforge.rescue-plan/v1";
pub(super) const RESCUE_RECEIPT_SCHEMA: &str = "arkforge.rescue-receipt/v1";
pub(super) const RESCUE_INTENT_SCHEMA: &str = "arkforge.rescue-intent/v1";
pub(super) const PLAN_LIFETIME: Duration = Duration::from_secs(15 * 60);
pub(super) const MAX_READ_BYTES: u64 = 512 * 1024 * 1024;
pub(super) const PROFILE_SOURCE: &str = include_str!("../../../../../profiles/dayu200.yaml");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RescueError {
    pub code: &'static str,
    pub message: String,
    pub exit_code: i32,
    pub retryable: bool,
}

impl RescueError {
    pub(super) fn new(code: &'static str, message: impl Into<String>, exit_code: i32) -> Self {
        Self {
            code,
            message: message.into(),
            exit_code,
            retryable: false,
        }
    }

    pub(super) fn retryable(mut self) -> Self {
        self.retryable = true;
        self
    }

    pub(super) fn invalid(message: impl Into<String>) -> Self {
        Self::new("INVALID_ARGUMENT", message, 2)
    }

    pub(super) fn refused(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(code, message, 3)
    }

    pub(super) fn io(context: &str, error: impl fmt::Display) -> Self {
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
    pub(super) fn from_rockusb(device: &RockUsbDevice) -> Result<Self, RescueError> {
        let facts = device_facts_value(device);
        let bytes = facts
            .to_canonical_bytes()
            .map_err(|error| RescueError::io("encode rescue device facts", error))?;
        let digest = digest_in_domain(Domain::RescueDeviceFacts, &bytes);
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

pub(super) fn device_facts_value(device: &RockUsbDevice) -> CborValue {
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

    pub(super) fn descriptors(
        &self,
    ) -> Result<Vec<arkforge_usb::UsbInterfaceDescriptor>, RescueError> {
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

pub(super) fn port_refusal(operation: &str, failure: RockUsbPortFailure) -> RescueError {
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

    pub(super) fn acknowledgements(&self) -> Vec<String> {
        let mut values = vec!["rescue:native-rockusb".to_string()];
        match self {
            Self::WritePartition { partition, .. } => {
                values.push(format!("overwrite:partition={partition}"));
            }
            Self::ResetDevice => values.push("reset:device".into()),
        }
        values
    }

    pub(super) fn to_value(&self) -> CborValue {
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

    pub(super) fn from_value(value: &CborValue) -> Result<Self, RescueError> {
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
