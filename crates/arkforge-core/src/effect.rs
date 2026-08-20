//! EffectSet — what a plan will do to a device, stated before it is authorized.
//!
//! architecture.md 5.5. The type system carries two invariants that would
//! otherwise be review comments: a persistent write range is always exact
//! (there is no wildcard to construct), and every data-impact axis is a
//! three-state where `Unknown` is representable so it can be *rejected* rather
//! than silently defaulted.

use crate::digest::{CanonicalCbor, CborError, CborValue, Domain, Sha256Digest, digest_canonical};
use crate::ids::{PartitionId, RegionId};
use core::fmt;

/// An exact byte range. There is deliberately no unbounded variant: a
/// destructive effect whose end is unknown cannot be described here, so it
/// cannot reach a plan (architecture.md 5.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteRange {
    pub start: u64,
    pub length: u64,
}

impl ByteRange {
    pub fn new(start: u64, length: u64) -> Result<Self, EffectError> {
        if length == 0 {
            return Err(EffectError::EmptyRange);
        }
        start
            .checked_add(length)
            .ok_or(EffectError::RangeOverflow { start, length })?;
        Ok(ByteRange { start, length })
    }

    pub fn end_exclusive(&self) -> u64 {
        self.start + self.length
    }

    pub fn contains_range(&self, other: &ByteRange) -> bool {
        other.start >= self.start && other.end_exclusive() <= self.end_exclusive()
    }

    pub fn overlaps(&self, other: &ByteRange) -> bool {
        self.start < other.end_exclusive() && other.start < self.end_exclusive()
    }
}

impl CanonicalCbor for ByteRange {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("start", CborValue::Unsigned(self.start)),
            ("length", CborValue::Unsigned(self.length)),
        ])
    }
}

/// A boot-metadata field a provider may set. Closed on purpose: a free-text
/// field name would let a provider describe an effect ArkDeck cannot classify.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BootMetadataField {
    ActiveSlot,
    BootCount,
    UpdaterCommand,
    LockState,
}

impl BootMetadataField {
    pub fn as_str(self) -> &'static str {
        match self {
            BootMetadataField::ActiveSlot => "activeSlot",
            BootMetadataField::BootCount => "bootCount",
            BootMetadataField::UpdaterCommand => "updaterCommand",
            BootMetadataField::LockState => "lockState",
        }
    }
}

/// A typed value for a boot-metadata assertion. No floats (architecture.md 15.4).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum TypedValue {
    Text(String),
    Integer(i64),
    Bool(bool),
}

impl CanonicalCbor for TypedValue {
    fn to_cbor(&self) -> CborValue {
        match self {
            TypedValue::Text(value) => {
                CborValue::map(vec![("text", CborValue::text(value.clone()))])
            }
            TypedValue::Integer(value) => {
                CborValue::map(vec![("integer", CborValue::integer(*value))])
            }
            TypedValue::Bool(value) => CborValue::map(vec![("bool", CborValue::Bool(*value))]),
        }
    }
}

/// A device mode. Modes are Profile-declared facts, not transport guesses
/// (architecture.md 11.3).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceMode(String);

impl DeviceMode {
    pub fn new(value: impl Into<String>) -> Result<Self, EffectError> {
        let value = value.into();
        if value.is_empty() || value.len() > 64 {
            return Err(EffectError::InvalidModeName(value));
        }
        let conforming = value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !conforming {
            return Err(EffectError::InvalidModeName(value));
        }
        Ok(DeviceMode(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeviceMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl CanonicalCbor for DeviceMode {
    fn to_cbor(&self) -> CborValue {
        CborValue::text(self.0.clone())
    }
}

/// The stage of an ephemeral agent loaded into device memory.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AgentStage(String);

impl AgentStage {
    pub fn new(value: impl Into<String>) -> Result<Self, EffectError> {
        let value = value.into();
        if value.is_empty() || value.len() > 64 {
            return Err(EffectError::InvalidAgentStage(value));
        }
        Ok(AgentStage(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl CanonicalCbor for AgentStage {
    fn to_cbor(&self) -> CborValue {
        CborValue::text(self.0.clone())
    }
}

/// A device memory region an ephemeral agent occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MemoryRegion {
    pub base_address: u64,
    pub length: u64,
}

impl CanonicalCbor for MemoryRegion {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("baseAddress", CborValue::Unsigned(self.base_address)),
            ("length", CborValue::Unsigned(self.length)),
        ])
    }
}

/// An effect that survives power loss.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum PersistentEffect {
    ErasePartition {
        partition: PartitionId,
        range: ByteRange,
    },
    WritePartition {
        partition: PartitionId,
        range: ByteRange,
        content: Sha256Digest,
    },
    WriteRawRegion {
        region: RegionId,
        range: ByteRange,
        content: Sha256Digest,
    },
    ReplacePartitionTable {
        layout_digest: Sha256Digest,
    },
    ChangeBootMetadata {
        field: BootMetadataField,
        expected_value: TypedValue,
    },
}

impl PersistentEffect {
    pub fn partition(&self) -> Option<&PartitionId> {
        match self {
            PersistentEffect::ErasePartition { partition, .. }
            | PersistentEffect::WritePartition { partition, .. } => Some(partition),
            _ => None,
        }
    }

    pub fn range(&self) -> Option<&ByteRange> {
        match self {
            PersistentEffect::ErasePartition { range, .. }
            | PersistentEffect::WritePartition { range, .. }
            | PersistentEffect::WriteRawRegion { range, .. } => Some(range),
            _ => None,
        }
    }
}

impl CanonicalCbor for PersistentEffect {
    fn to_cbor(&self) -> CborValue {
        match self {
            PersistentEffect::ErasePartition { partition, range } => CborValue::map(vec![
                ("kind", CborValue::text("erasePartition")),
                ("partition", partition.to_cbor()),
                ("range", range.to_cbor()),
            ]),
            PersistentEffect::WritePartition {
                partition,
                range,
                content,
            } => CborValue::map(vec![
                ("kind", CborValue::text("writePartition")),
                ("partition", partition.to_cbor()),
                ("range", range.to_cbor()),
                ("content", content.to_cbor()),
            ]),
            PersistentEffect::WriteRawRegion {
                region,
                range,
                content,
            } => CborValue::map(vec![
                ("kind", CborValue::text("writeRawRegion")),
                ("region", region.to_cbor()),
                ("range", range.to_cbor()),
                ("content", content.to_cbor()),
            ]),
            PersistentEffect::ReplacePartitionTable { layout_digest } => CborValue::map(vec![
                ("kind", CborValue::text("replacePartitionTable")),
                ("layoutDigest", layout_digest.to_cbor()),
            ]),
            PersistentEffect::ChangeBootMetadata {
                field,
                expected_value,
            } => CborValue::map(vec![
                ("kind", CborValue::text("changeBootMetadata")),
                ("field", CborValue::text(field.as_str())),
                ("expectedValue", expected_value.to_cbor()),
            ]),
        }
    }
}

/// An effect that does not survive power loss but is still externally visible.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum TransientEffect {
    EnterMode {
        from: DeviceMode,
        to: DeviceMode,
    },
    LoadEphemeralAgent {
        stage: AgentStage,
        memory_region: MemoryRegion,
        content: Sha256Digest,
    },
    UsbDetachReattach {
        expectation_digest: Sha256Digest,
    },
    Reboot {
        target_mode: DeviceMode,
    },
}

impl CanonicalCbor for TransientEffect {
    fn to_cbor(&self) -> CborValue {
        match self {
            TransientEffect::EnterMode { from, to } => CborValue::map(vec![
                ("kind", CborValue::text("enterMode")),
                ("from", from.to_cbor()),
                ("to", to.to_cbor()),
            ]),
            TransientEffect::LoadEphemeralAgent {
                stage,
                memory_region,
                content,
            } => CborValue::map(vec![
                ("kind", CborValue::text("loadEphemeralAgent")),
                ("stage", stage.to_cbor()),
                ("memoryRegion", memory_region.to_cbor()),
                ("content", content.to_cbor()),
            ]),
            TransientEffect::UsbDetachReattach { expectation_digest } => CborValue::map(vec![
                ("kind", CborValue::text("usbDetachReattach")),
                ("expectationDigest", expectation_digest.to_cbor()),
            ]),
            TransientEffect::Reboot { target_mode } => CborValue::map(vec![
                ("kind", CborValue::text("reboot")),
                ("targetMode", target_mode.to_cbor()),
            ]),
        }
    }
}

/// What happens to one class of on-device data.
///
/// `Unknown` exists so a provider can *say* it does not know, which is what
/// makes the plan non-executable. Removing it would turn a missing fact into a
/// silent "preserved".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DataImpactState {
    Preserved,
    Overwritten,
    Unknown,
}

impl DataImpactState {
    pub fn as_str(self) -> &'static str {
        match self {
            DataImpactState::Preserved => "preserved",
            DataImpactState::Overwritten => "overwritten",
            DataImpactState::Unknown => "unknown",
        }
    }
}

/// The data-impact axes architecture.md 5.5 requires a plan to settle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DataImpact {
    pub userdata: DataImpactState,
    pub calibration: DataImpactState,
    pub non_volatile_config: DataImpactState,
    pub secure_storage: DataImpactState,
}

impl DataImpact {
    pub fn all_unknown() -> Self {
        DataImpact {
            userdata: DataImpactState::Unknown,
            calibration: DataImpactState::Unknown,
            non_volatile_config: DataImpactState::Unknown,
            secure_storage: DataImpactState::Unknown,
        }
    }

    /// The axes that are still `Unknown`, named for the blocker message.
    pub fn unknown_axes(&self) -> Vec<&'static str> {
        let mut axes = Vec::new();
        if self.userdata == DataImpactState::Unknown {
            axes.push("userdata");
        }
        if self.calibration == DataImpactState::Unknown {
            axes.push("calibration");
        }
        if self.non_volatile_config == DataImpactState::Unknown {
            axes.push("nonVolatileConfig");
        }
        if self.secure_storage == DataImpactState::Unknown {
            axes.push("secureStorage");
        }
        axes
    }
}

impl CanonicalCbor for DataImpact {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("userdata", CborValue::text(self.userdata.as_str())),
            ("calibration", CborValue::text(self.calibration.as_str())),
            (
                "nonVolatileConfig",
                CborValue::text(self.non_volatile_config.as_str()),
            ),
            (
                "secureStorage",
                CborValue::text(self.secure_storage.as_str()),
            ),
        ])
    }
}

/// The complete declared effect of a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectSet {
    pub persistent: Vec<PersistentEffect>,
    pub transient: Vec<TransientEffect>,
    pub data_impact: DataImpact,
}

impl EffectSet {
    pub fn read_only() -> Self {
        EffectSet {
            persistent: Vec::new(),
            transient: Vec::new(),
            data_impact: DataImpact {
                userdata: DataImpactState::Preserved,
                calibration: DataImpactState::Preserved,
                non_volatile_config: DataImpactState::Preserved,
                secure_storage: DataImpactState::Preserved,
            },
        }
    }

    pub fn is_destructive(&self) -> bool {
        !self.persistent.is_empty()
    }

    pub fn digest(&self) -> Result<Sha256Digest, CborError> {
        digest_canonical(Domain::EffectSet, self)
    }

    /// Rejects the effect sets architecture.md 5.5 says cannot form an
    /// executable plan. Range containment against a Profile allowlist is a
    /// Profile-level check and lives with the Profile.
    pub fn validate_executable(&self) -> Result<(), EffectError> {
        let unknown = self.data_impact.unknown_axes();
        if !unknown.is_empty() {
            return Err(EffectError::UnknownDataImpact(
                unknown.iter().map(|axis| axis.to_string()).collect(),
            ));
        }
        for (left_index, left) in self.persistent.iter().enumerate() {
            for right in self.persistent.iter().skip(left_index + 1) {
                if let (Some(left_partition), Some(right_partition)) =
                    (left.partition(), right.partition())
                {
                    if left_partition != right_partition {
                        continue;
                    }
                    if let (Some(left_range), Some(right_range)) = (left.range(), right.range())
                        && left_range.overlaps(right_range)
                    {
                        return Err(EffectError::OverlappingEffects {
                            partition: left_partition.to_string(),
                            first: *left_range,
                            second: *right_range,
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

impl CanonicalCbor for EffectSet {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            (
                "persistent",
                CborValue::array(self.persistent.iter().map(|e| e.to_cbor()).collect()),
            ),
            (
                "transient",
                CborValue::array(self.transient.iter().map(|e| e.to_cbor()).collect()),
            ),
            ("dataImpact", self.data_impact.to_cbor()),
        ])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectError {
    EmptyRange,
    RangeOverflow {
        start: u64,
        length: u64,
    },
    InvalidModeName(String),
    InvalidAgentStage(String),
    UnknownDataImpact(Vec<String>),
    OverlappingEffects {
        partition: String,
        first: ByteRange,
        second: ByteRange,
    },
}

impl fmt::Display for EffectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EffectError::EmptyRange => f.write_str("byte range length must be non-zero"),
            EffectError::RangeOverflow { start, length } => {
                write!(f, "byte range {start}+{length} overflows u64")
            }
            EffectError::InvalidModeName(name) => {
                write!(
                    f,
                    "device mode must be lowercase ascii/digits/hyphen: {name:?}"
                )
            }
            EffectError::InvalidAgentStage(name) => write!(f, "invalid agent stage {name:?}"),
            EffectError::UnknownDataImpact(axes) => write!(
                f,
                "data impact is unknown for {}; an executable plan requires every axis settled",
                axes.join(", ")
            ),
            EffectError::OverlappingEffects {
                partition,
                first,
                second,
            } => write!(
                f,
                "partition {partition} has overlapping effects {first:?} and {second:?}"
            ),
        }
    }
}

impl std::error::Error for EffectError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::sha256;

    fn partition(name: &str) -> PartitionId {
        PartitionId::new(name).unwrap()
    }

    #[test]
    fn zero_length_and_overflowing_ranges_are_unconstructable() {
        assert_eq!(ByteRange::new(0, 0), Err(EffectError::EmptyRange));
        assert!(matches!(
            ByteRange::new(u64::MAX, 2),
            Err(EffectError::RangeOverflow { .. })
        ));
    }

    #[test]
    fn unknown_data_impact_blocks_an_executable_plan() {
        let effects = EffectSet {
            persistent: vec![],
            transient: vec![],
            data_impact: DataImpact::all_unknown(),
        };
        let error = effects.validate_executable().unwrap_err();
        match error {
            EffectError::UnknownDataImpact(axes) => {
                assert_eq!(
                    axes,
                    vec![
                        "userdata".to_string(),
                        "calibration".to_string(),
                        "nonVolatileConfig".to_string(),
                        "secureStorage".to_string()
                    ]
                );
            }
            other => panic!("unexpected error {other:?}"),
        }
    }

    #[test]
    fn overlapping_writes_to_one_partition_are_rejected() {
        let effects = EffectSet {
            persistent: vec![
                PersistentEffect::WritePartition {
                    partition: partition("system"),
                    range: ByteRange::new(0, 100).unwrap(),
                    content: sha256(b"a"),
                },
                PersistentEffect::WritePartition {
                    partition: partition("system"),
                    range: ByteRange::new(50, 100).unwrap(),
                    content: sha256(b"b"),
                },
            ],
            transient: vec![],
            data_impact: DataImpact {
                userdata: DataImpactState::Preserved,
                calibration: DataImpactState::Preserved,
                non_volatile_config: DataImpactState::Preserved,
                secure_storage: DataImpactState::Preserved,
            },
        };
        assert!(matches!(
            effects.validate_executable(),
            Err(EffectError::OverlappingEffects { .. })
        ));
    }

    #[test]
    fn adjacent_writes_to_one_partition_are_allowed() {
        let effects = EffectSet {
            persistent: vec![
                PersistentEffect::WritePartition {
                    partition: partition("system"),
                    range: ByteRange::new(0, 50).unwrap(),
                    content: sha256(b"a"),
                },
                PersistentEffect::WritePartition {
                    partition: partition("system"),
                    range: ByteRange::new(50, 50).unwrap(),
                    content: sha256(b"b"),
                },
            ],
            transient: vec![],
            data_impact: DataImpact {
                userdata: DataImpactState::Preserved,
                calibration: DataImpactState::Preserved,
                non_volatile_config: DataImpactState::Preserved,
                secure_storage: DataImpactState::Preserved,
            },
        };
        assert!(effects.validate_executable().is_ok());
    }

    #[test]
    fn effect_digest_is_independent_of_construction_order_of_map_keys() {
        let effects = EffectSet::read_only();
        assert_eq!(effects.digest().unwrap(), effects.digest().unwrap());
    }

    #[test]
    fn device_mode_rejects_uppercase_and_spaces() {
        assert!(DeviceMode::new("download-loader").is_ok());
        assert!(DeviceMode::new("DownloadLoader").is_err());
        assert!(DeviceMode::new("host normal").is_err());
    }
}
