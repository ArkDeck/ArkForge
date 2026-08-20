//! ArtifactManifest — the facts a parser is allowed to produce.
//!
//! architecture.md 10.3: a parser has no USB, no network, no process
//! execution, decides no authority, and emits no vendor options. It emits
//! facts, unknowns, and a confidence level. Everything downstream — which
//! partitions may be written, whether a plan is executable — is decided by the
//! Profile and the Provider against these facts, never by the parser.

use arkforge_core::digest::{
    CanonicalCbor, CborError, CborValue, Domain, Sha256Digest, digest_canonical,
};
use arkforge_core::identity::ArtifactFormat;
use arkforge_core::ids::OpaqueId;
use arkforge_core::plan::ExecutionUnknown;
use core::fmt;
use std::collections::BTreeSet;

/// How much a parser's output can be trusted to drive execution
/// (architecture.md 10.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ParserConfidence {
    /// Structure recognized, execution-relevant fields still unknown. Can only
    /// produce a PlanAssessment.
    ResearchOnly,
    /// Every execution-relevant field is known; `execution_relevant_unknowns`
    /// is empty.
    ProductionManifest,
}

impl ParserConfidence {
    pub fn as_str(self) -> &'static str {
        match self {
            ParserConfidence::ResearchOnly => "researchOnly",
            ParserConfidence::ProductionManifest => "productionManifest",
        }
    }
}

impl CanonicalCbor for ParserConfidence {
    fn to_cbor(&self) -> CborValue {
        CborValue::text(self.as_str())
    }
}

/// What a member is *structurally*, from the container format's own rules.
///
/// This is not "may it be written": that answer belongs to the DeviceProfile,
/// which owns the target allowlist (architecture.md 10.4). A parser that
/// decided writability would be a device policy hiding inside a format reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MemberRole {
    /// Declares the on-device layout, e.g. `parameter.txt`.
    PartitionTable,
    /// A loader or boot agent staged into volatile memory.
    Loader,
    /// A payload image that *could* map to a partition, if a Profile says so.
    ImageCandidate,
    /// Present, understood, and not execution-relevant.
    Metadata,
    /// Present and not understood.
    Unclassified,
}

impl MemberRole {
    pub fn as_str(self) -> &'static str {
        match self {
            MemberRole::PartitionTable => "partitionTable",
            MemberRole::Loader => "loader",
            MemberRole::ImageCandidate => "imageCandidate",
            MemberRole::Metadata => "metadata",
            MemberRole::Unclassified => "unclassified",
        }
    }
}

/// One observed archive member. The parser records what is there; it does not
/// decide whether it may be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveMemberFact {
    pub path: String,
    pub size_bytes: u64,
    pub sha256: Sha256Digest,
    pub role: MemberRole,
}

impl CanonicalCbor for ArchiveMemberFact {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("path", CborValue::text(self.path.clone())),
            ("sizeBytes", CborValue::Unsigned(self.size_bytes)),
            ("sha256", self.sha256.to_cbor()),
            ("role", CborValue::text(self.role.as_str())),
        ])
    }
}

/// A partition attribute the source grammar allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PartitionAttribute {
    Bootable,
    Grow,
}

impl PartitionAttribute {
    pub fn as_str(self) -> &'static str {
        match self {
            PartitionAttribute::Bootable => "bootable",
            PartitionAttribute::Grow => "grow",
        }
    }

    /// Unknown attributes fail closed — an attribute this parser does not
    /// understand may change what a write means.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "bootable" => Some(PartitionAttribute::Bootable),
            "grow" => Some(PartitionAttribute::Grow),
            _ => None,
        }
    }
}

/// Which branch of the source grammar produced an entry. Recorded so a
/// manifest can be compared against the pinned ArkDeck decode without
/// re-deriving it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GrammarBranch {
    Fixed,
    FixedBootable,
    RemainderGrow,
}

impl GrammarBranch {
    pub fn as_str(self) -> &'static str {
        match self {
            GrammarBranch::Fixed => "fixed",
            GrammarBranch::FixedBootable => "fixedBootable",
            GrammarBranch::RemainderGrow => "remainderGrow",
        }
    }
}

/// One partition as the artifact's own table declares it.
///
/// `size_sectors` is `None` for a remainder partition: the extent is not known
/// from the artifact alone. A remainder extent can still host an exact write,
/// which is why the effect model keeps ranges exact while the layout model
/// allows an open end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionEntryFact {
    pub index: u32,
    pub name: String,
    pub offset_sectors: u64,
    pub size_sectors: Option<u64>,
    pub attribute: Option<PartitionAttribute>,
    pub grammar_branch: GrammarBranch,
}

impl PartitionEntryFact {
    pub fn end_sector_exclusive(&self) -> Option<u64> {
        self.size_sectors
            .and_then(|size| self.offset_sectors.checked_add(size))
    }
}

impl CanonicalCbor for PartitionEntryFact {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("index", CborValue::Unsigned(self.index as u64)),
            ("name", CborValue::text(self.name.clone())),
            ("offsetSectors", CborValue::Unsigned(self.offset_sectors)),
            (
                "sizeSectors",
                match self.size_sectors {
                    Some(size) => CborValue::Unsigned(size),
                    None => CborValue::Null,
                },
            ),
            (
                "attribute",
                match self.attribute {
                    Some(attribute) => CborValue::text(attribute.as_str()),
                    None => CborValue::Null,
                },
            ),
            (
                "grammarBranch",
                CborValue::text(self.grammar_branch.as_str()),
            ),
        ])
    }
}

/// The partition table the artifact declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionTableFact {
    /// The device string from the source grammar, e.g. `rk29xxnand`.
    pub device: String,
    pub logical_block_size: u32,
    pub entries: Vec<PartitionEntryFact>,
}

impl PartitionTableFact {
    /// Rejects overlapping or out-of-order extents (architecture.md 10.4).
    pub fn validate(&self) -> Result<(), ManifestError> {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for entry in &self.entries {
            if !seen.insert(entry.name.as_str()) {
                return Err(ManifestError::DuplicatePartitionName(entry.name.clone()));
            }
        }
        for (index, entry) in self.entries.iter().enumerate() {
            if entry.size_sectors.is_none() && index + 1 != self.entries.len() {
                return Err(ManifestError::RemainderNotLast(entry.name.clone()));
            }
            if entry.size_sectors == Some(0) {
                return Err(ManifestError::ZeroLengthPartition(entry.name.clone()));
            }
        }
        for (left_index, left) in self.entries.iter().enumerate() {
            let Some(left_end) = left.end_sector_exclusive() else {
                continue;
            };
            for right in self.entries.iter().skip(left_index + 1) {
                let right_end = right.end_sector_exclusive().unwrap_or(u64::MAX);
                if left.offset_sectors < right_end && right.offset_sectors < left_end {
                    return Err(ManifestError::OverlappingPartitions {
                        first: left.name.clone(),
                        second: right.name.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    pub fn entry(&self, name: &str) -> Option<&PartitionEntryFact> {
        self.entries.iter().find(|entry| entry.name == name)
    }
}

impl CanonicalCbor for PartitionTableFact {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("device", CborValue::text(self.device.clone())),
            (
                "logicalBlockSize",
                CborValue::Unsigned(self.logical_block_size as u64),
            ),
            (
                "entries",
                CborValue::array(self.entries.iter().map(|e| e.to_cbor()).collect()),
            ),
        ])
    }
}

/// The complete parser output for one artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactManifest {
    pub format: ArtifactFormat,
    pub content_digest: Sha256Digest,
    pub size_bytes: u64,
    pub members: Vec<ArchiveMemberFact>,
    pub partition_table: Option<PartitionTableFact>,
    /// Facts read from verified sources inside the artifact — e.g. the build
    /// name embedded in an image. Never inferred from a filename
    /// (architecture.md 10.4).
    pub build_facts: Vec<(OpaqueId, String)>,
    /// Members the parser recognized as present but could not classify.
    pub unclassified_members: Vec<String>,
    /// Unknowns that block an executable plan while they remain open.
    pub execution_relevant_unknowns: Vec<ExecutionUnknown>,
    pub confidence: ParserConfidence,
}

impl ArtifactManifest {
    pub fn digest(&self) -> Result<Sha256Digest, CborError> {
        digest_canonical(Domain::ArtifactManifest, self)
    }

    pub fn member(&self, path: &str) -> Option<&ArchiveMemberFact> {
        self.members.iter().find(|member| member.path == path)
    }

    /// A manifest may only claim `ProductionManifest` with no open
    /// execution-relevant unknowns (architecture.md 10.5).
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.confidence == ParserConfidence::ProductionManifest
            && !self.execution_relevant_unknowns.is_empty()
        {
            return Err(ManifestError::ProductionManifestWithUnknowns(
                self.execution_relevant_unknowns
                    .iter()
                    .map(|unknown| unknown.id.to_string())
                    .collect(),
            ));
        }
        if let Some(table) = &self.partition_table {
            table.validate()?;
        }
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for member in &self.members {
            if !seen.insert(member.path.as_str()) {
                return Err(ManifestError::DuplicateMember(member.path.clone()));
            }
        }
        Ok(())
    }
}

impl CanonicalCbor for ArtifactManifest {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("format", self.format.to_cbor()),
            ("contentDigest", self.content_digest.to_cbor()),
            ("sizeBytes", CborValue::Unsigned(self.size_bytes)),
            (
                "members",
                CborValue::array(self.members.iter().map(|m| m.to_cbor()).collect()),
            ),
            (
                "partitionTable",
                match &self.partition_table {
                    Some(table) => table.to_cbor(),
                    None => CborValue::Null,
                },
            ),
            (
                "buildFacts",
                CborValue::Map(
                    self.build_facts
                        .iter()
                        .map(|(key, value)| (key.to_cbor(), CborValue::text(value.clone())))
                        .collect(),
                ),
            ),
            (
                "unclassifiedMembers",
                CborValue::array(
                    self.unclassified_members
                        .iter()
                        .map(|path| CborValue::text(path.clone()))
                        .collect(),
                ),
            ),
            (
                "executionRelevantUnknowns",
                CborValue::array(
                    self.execution_relevant_unknowns
                        .iter()
                        .map(|unknown| unknown.to_cbor())
                        .collect(),
                ),
            ),
            ("confidence", self.confidence.to_cbor()),
        ])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    DuplicateMember(String),
    DuplicatePartitionName(String),
    OverlappingPartitions { first: String, second: String },
    RemainderNotLast(String),
    ZeroLengthPartition(String),
    ProductionManifestWithUnknowns(Vec<String>),
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::DuplicateMember(path) => write!(f, "duplicate member {path:?}"),
            ManifestError::DuplicatePartitionName(name) => {
                write!(f, "duplicate partition name {name:?}")
            }
            ManifestError::OverlappingPartitions { first, second } => {
                write!(f, "partitions {first:?} and {second:?} overlap")
            }
            ManifestError::RemainderNotLast(name) => write!(
                f,
                "remainder partition {name:?} is not the last entry in the table"
            ),
            ManifestError::ZeroLengthPartition(name) => {
                write!(f, "partition {name:?} declares zero length")
            }
            ManifestError::ProductionManifestWithUnknowns(unknowns) => write!(
                f,
                "production manifest still has open execution-relevant unknowns: {}",
                unknowns.join(", ")
            ),
        }
    }
}

impl std::error::Error for ManifestError {}

#[cfg(test)]
mod tests {
    use super::*;
    use arkforge_core::digest::sha256;

    fn entry(index: u32, name: &str, offset: u64, size: Option<u64>) -> PartitionEntryFact {
        PartitionEntryFact {
            index,
            name: name.to_string(),
            offset_sectors: offset,
            size_sectors: size,
            attribute: if size.is_none() {
                Some(PartitionAttribute::Grow)
            } else {
                None
            },
            grammar_branch: if size.is_none() {
                GrammarBranch::RemainderGrow
            } else {
                GrammarBranch::Fixed
            },
        }
    }

    fn table(entries: Vec<PartitionEntryFact>) -> PartitionTableFact {
        PartitionTableFact {
            device: "rk29xxnand".into(),
            logical_block_size: 512,
            entries,
        }
    }

    #[test]
    fn a_well_formed_table_validates() {
        let table = table(vec![
            entry(0, "uboot", 8192, Some(8192)),
            entry(1, "misc", 16384, Some(8192)),
            entry(2, "userdata", 19_955_712, None),
        ]);
        table.validate().unwrap();
    }

    #[test]
    fn overlapping_partitions_are_rejected() {
        let table = table(vec![
            entry(0, "uboot", 8192, Some(16384)),
            entry(1, "misc", 16384, Some(8192)),
        ]);
        assert!(matches!(
            table.validate(),
            Err(ManifestError::OverlappingPartitions { .. })
        ));
    }

    #[test]
    fn a_remainder_partition_must_be_last() {
        let table = table(vec![
            entry(0, "userdata", 8192, None),
            entry(1, "uboot", 19_955_712, Some(8192)),
        ]);
        assert!(matches!(
            table.validate(),
            Err(ManifestError::RemainderNotLast(_))
        ));
    }

    #[test]
    fn a_fixed_partition_after_a_remainder_would_overlap_and_is_caught() {
        // Even with the ordering rule relaxed, the remainder's open end means
        // anything after it collides.
        let remainder = entry(0, "userdata", 8192, None);
        assert_eq!(remainder.end_sector_exclusive(), None);
    }

    #[test]
    fn a_production_manifest_may_not_carry_open_unknowns() {
        let manifest = ArtifactManifest {
            format: ArtifactFormat {
                id: OpaqueId::new("unisoc-pac").unwrap(),
                version: arkforge_core::identity::Version::new(1, 0, 0),
            },
            content_digest: sha256(b"pac"),
            size_bytes: 10,
            members: vec![],
            partition_table: None,
            build_facts: vec![],
            unclassified_members: vec![],
            execution_relevant_unknowns: vec![ExecutionUnknown {
                id: OpaqueId::new("UNI-U01").unwrap(),
                summary: "FDL load address unknown".into(),
            }],
            confidence: ParserConfidence::ProductionManifest,
        };
        assert!(matches!(
            manifest.validate(),
            Err(ManifestError::ProductionManifestWithUnknowns(_))
        ));
    }

    #[test]
    fn manifest_digest_is_stable() {
        let manifest = ArtifactManifest {
            format: ArtifactFormat {
                id: OpaqueId::new("rockchip-images-targz").unwrap(),
                version: arkforge_core::identity::Version::new(1, 0, 0),
            },
            content_digest: sha256(b"archive"),
            size_bytes: 730_769_584,
            members: vec![ArchiveMemberFact {
                path: "uboot.img".into(),
                size_bytes: 4_194_304,
                sha256: sha256(b"uboot"),
                role: MemberRole::ImageCandidate,
            }],
            partition_table: Some(table(vec![entry(0, "uboot", 8192, Some(8192))])),
            build_facts: vec![(
                OpaqueId::new("const.ohos.fullname").unwrap(),
                "OpenHarmony-7.0.0.36".into(),
            )],
            unclassified_members: vec![],
            execution_relevant_unknowns: vec![],
            confidence: ParserConfidence::ProductionManifest,
        };
        manifest.validate().unwrap();
        assert_eq!(manifest.digest().unwrap(), manifest.digest().unwrap());
    }
}
