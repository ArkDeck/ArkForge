//! DAYU200 `rockchip-images-targz` parser.
//!
//! architecture.md 10.4. Streams gzip/tar, hashes every member, decodes the
//! `parameter.txt` partition table, extracts build facts from inside a hashed
//! image rather than from a filename, and classifies members by the container
//! format's own rules.
//!
//! What this module must *not* do, and does not: decide which partitions may be
//! written. The nine-partition allowlist is a DeviceProfile fact
//! (architecture.md 10.4), so a Profile change cannot be defeated by a parser
//! that already made up its mind.

use crate::inflate::GzipReader;
use crate::manifest::{
    ArchiveMemberFact, ArtifactManifest, GrammarBranch, MemberRole, ParserConfidence,
    PartitionAttribute, PartitionEntryFact, PartitionTableFact,
};
use crate::tar::{ArchiveError, TarReader};
use arkforge_core::digest::{Sha256, Sha256Digest};
use arkforge_core::identity::{ArtifactFormat, Version};
use arkforge_core::ids::OpaqueId;
use arkforge_core::plan::ExecutionUnknown;
use core::fmt;
use std::collections::BTreeMap;
use std::io::Read;

/// The format identifier this parser claims.
pub const FORMAT_ID: &str = "rockchip-images-targz";
pub const FORMAT_VERSION: Version = Version::new(1, 0, 0);

/// The member that declares the on-device layout. A format-level name, not a
/// device-level one.
pub const PARTITION_TABLE_MEMBER: &str = "parameter.txt";
/// The maskrom-stage loader. Also a format-level name.
pub const LOADER_MEMBER: &str = "MiniLoaderAll.bin";

/// Runtime keys worth extracting from inside an image, and later compared
/// against what the booted device answers (architecture.md 16.4 postflight).
pub const BUILD_FACT_KEYS: [&str; 3] = [
    "const.ohos.fullname",
    "const.product.model",
    "const.product.name",
];

/// Bound on how much of one member is scanned for build facts.
///
/// The facts live in a properties blob near the head of a system image; hashing
/// still covers every byte, but scanning all 2 GiB for a handful of keys buys
/// nothing.
const BUILD_FACT_SCAN_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParameterError {
    MissingCmdline,
    MissingMtdparts,
    MissingDevice,
    EmptyPartitionList,
    MalformedPartition(String),
    UnknownAttribute { partition: String, attribute: String },
    BadHex { partition: String, field: String, value: String },
    NotUtf8,
    TooLarge(u64),
}

impl fmt::Display for ParameterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParameterError::MissingCmdline => f.write_str("parameter file has no CMDLINE line"),
            ParameterError::MissingMtdparts => {
                f.write_str("CMDLINE carries no mtdparts= assignment")
            }
            ParameterError::MissingDevice => f.write_str("mtdparts has no device prefix"),
            ParameterError::EmptyPartitionList => f.write_str("mtdparts declares no partitions"),
            ParameterError::MalformedPartition(text) => {
                write!(f, "malformed partition descriptor {text:?}")
            }
            ParameterError::UnknownAttribute {
                partition,
                attribute,
            } => write!(
                f,
                "partition {partition:?} carries unknown attribute {attribute:?}"
            ),
            ParameterError::BadHex {
                partition,
                field,
                value,
            } => write!(
                f,
                "partition {partition:?} has non-hex {field} {value:?}"
            ),
            ParameterError::NotUtf8 => f.write_str("parameter file is not UTF-8"),
            ParameterError::TooLarge(size) => {
                write!(f, "parameter file of {size} bytes is implausibly large")
            }
        }
    }
}

impl std::error::Error for ParameterError {}

/// Decodes the `mtdparts` partition table.
///
/// Grammar (ArkDeck `partition-mapping.json`, schema
/// `arkdeck-dayu200-partition-mapping-1.0.0`):
///
/// ```text
/// CMDLINE:mtdparts=<device>:<partition>[,<partition>...]
/// partition := (<hex-size>|-)@<hex-offset>(<name>[:<attribute>])
/// attribute := bootable | grow
/// ```
///
/// Numeric values are kept in the unit the source encodes — sectors — and are
/// never converted here. A parser that silently rebased units would make two
/// honest readers of the same file disagree about an address.
pub fn parse_parameter(text: &str, logical_block_size: u32) -> Result<PartitionTableFact, ParameterError> {
    let cmdline = text
        .lines()
        .map(|line| line.trim())
        .find(|line| line.starts_with("CMDLINE:"))
        .ok_or(ParameterError::MissingCmdline)?;
    let body = &cmdline["CMDLINE:".len()..];

    let mtdparts = body
        .split_whitespace()
        .find_map(|token| token.strip_prefix("mtdparts="))
        .ok_or(ParameterError::MissingMtdparts)?;

    let (device, list) = mtdparts
        .split_once(':')
        .ok_or(ParameterError::MissingDevice)?;
    if device.is_empty() {
        return Err(ParameterError::MissingDevice);
    }
    if list.is_empty() {
        return Err(ParameterError::EmptyPartitionList);
    }

    let mut entries = Vec::new();
    for (index, descriptor) in list.split(',').enumerate() {
        let descriptor = descriptor.trim();
        if descriptor.is_empty() {
            return Err(ParameterError::EmptyPartitionList);
        }
        entries.push(parse_partition(index as u32, descriptor)?);
    }
    Ok(PartitionTableFact {
        device: device.to_string(),
        logical_block_size,
        entries,
    })
}

fn parse_partition(index: u32, descriptor: &str) -> Result<PartitionEntryFact, ParameterError> {
    let malformed = || ParameterError::MalformedPartition(descriptor.to_string());

    let at = descriptor.find('@').ok_or_else(malformed)?;
    let size_text = &descriptor[..at];
    let rest = &descriptor[at + 1..];

    let open = rest.find('(').ok_or_else(malformed)?;
    if !rest.ends_with(')') {
        return Err(malformed());
    }
    let offset_text = &rest[..open];
    let inside = &rest[open + 1..rest.len() - 1];
    if inside.is_empty() {
        return Err(malformed());
    }

    let (name, attribute_text) = match inside.split_once(':') {
        Some((name, attribute)) => (name, Some(attribute)),
        None => (inside, None),
    };
    if name.is_empty() {
        return Err(malformed());
    }

    let attribute = match attribute_text {
        None => None,
        Some(text) => Some(PartitionAttribute::parse(text).ok_or_else(|| {
            ParameterError::UnknownAttribute {
                partition: name.to_string(),
                attribute: text.to_string(),
            }
        })?),
    };

    let size_sectors = if size_text == "-" {
        None
    } else {
        Some(parse_hex(size_text).ok_or_else(|| ParameterError::BadHex {
            partition: name.to_string(),
            field: "size".into(),
            value: size_text.to_string(),
        })?)
    };
    let offset_sectors = parse_hex(offset_text).ok_or_else(|| ParameterError::BadHex {
        partition: name.to_string(),
        field: "offset".into(),
        value: offset_text.to_string(),
    })?;

    let grammar_branch = match (size_sectors, attribute) {
        (None, Some(PartitionAttribute::Grow)) => GrammarBranch::RemainderGrow,
        (Some(_), Some(PartitionAttribute::Bootable)) => GrammarBranch::FixedBootable,
        (Some(_), None) => GrammarBranch::Fixed,
        // A remainder without `grow`, or a fixed size with `grow`, is a
        // combination the pinned grammar does not produce.
        _ => return Err(malformed()),
    };

    Ok(PartitionEntryFact {
        index,
        name: name.to_string(),
        offset_sectors,
        size_sectors,
        attribute,
        grammar_branch,
    })
}

fn parse_hex(text: &str) -> Option<u64> {
    let digits = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X"))?;
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(digits, 16).ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dayu200ParseError {
    Archive(ArchiveError),
    Parameter(ParameterError),
    Manifest(crate::manifest::ManifestError),
    /// Two members disagree about the same build fact. Not resolved by
    /// preference — a contradiction is an unknown.
    ContradictoryBuildFact {
        key: String,
        first: String,
        second: String,
    },
}

impl fmt::Display for Dayu200ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Dayu200ParseError::Archive(error) => write!(f, "{error}"),
            Dayu200ParseError::Parameter(error) => write!(f, "parameter.txt: {error}"),
            Dayu200ParseError::Manifest(error) => write!(f, "{error}"),
            Dayu200ParseError::ContradictoryBuildFact { key, first, second } => write!(
                f,
                "build fact {key} appears as both {first:?} and {second:?}"
            ),
        }
    }
}

impl std::error::Error for Dayu200ParseError {}

impl From<ArchiveError> for Dayu200ParseError {
    fn from(error: ArchiveError) -> Self {
        Dayu200ParseError::Archive(error)
    }
}

/// Classifies a member by the container format's rules.
fn classify(path: &str) -> MemberRole {
    match path {
        PARTITION_TABLE_MEMBER => MemberRole::PartitionTable,
        LOADER_MEMBER => MemberRole::Loader,
        _ if path.ends_with(".img") => MemberRole::ImageCandidate,
        _ if path.ends_with(".cfg") || path.ends_with(".xml") || path.ends_with(".log") => {
            MemberRole::Metadata
        }
        _ => MemberRole::Unclassified,
    }
}

/// Streams a `.tar.gz` and produces its manifest.
///
/// `source` is read exactly once. Nothing is written to disk, and no member is
/// held in memory beyond `parameter.txt`.
pub fn inspect<R: Read>(source: R) -> Result<ArtifactManifest, Dayu200ParseError> {
    let mut counting = CountingHasher::new(source);
    let manifest = {
        let gzip = GzipReader::new(&mut counting).map_err(|error| {
            ArchiveError::ArchiveInvalid(error.to_string())
        })?;
        inspect_decompressed(gzip)?
    };
    let (size_bytes, content_digest) = counting.finish();
    Ok(ArtifactManifest {
        content_digest,
        size_bytes,
        ..manifest
    })
}

fn inspect_decompressed<R: Read>(source: R) -> Result<ArtifactManifest, Dayu200ParseError> {
    let mut reader = TarReader::new(source);
    let mut members = Vec::new();
    let mut unclassified = Vec::new();
    let mut parameter_text: Option<String> = None;
    let mut build_facts: BTreeMap<String, (String, String)> = BTreeMap::new();

    while let Some(header) = reader.next_member()? {
        let role = classify(&header.path);
        if role == MemberRole::Unclassified {
            unclassified.push(header.path.clone());
        }

        let mut parameter_buffer: Option<Vec<u8>> = None;
        if role == MemberRole::PartitionTable {
            if header.size > 1 << 20 {
                return Err(Dayu200ParseError::Parameter(ParameterError::TooLarge(
                    header.size,
                )));
            }
            parameter_buffer = Some(Vec::with_capacity(header.size as usize));
        }

        let mut scanner = if role == MemberRole::ImageCandidate {
            Some(BuildFactScanner::new())
        } else {
            None
        };

        let observation = reader.read_member_body(&header, |chunk| {
            if let Some(buffer) = parameter_buffer.as_mut() {
                buffer.extend_from_slice(chunk);
            }
            if let Some(scanner) = scanner.as_mut() {
                scanner.push(chunk);
            }
        })?;

        if let Some(buffer) = parameter_buffer {
            let text =
                String::from_utf8(buffer).map_err(|_| ParameterError::NotUtf8).map_err(Dayu200ParseError::Parameter)?;
            parameter_text = Some(text);
        }
        if let Some(scanner) = scanner {
            for (key, value) in scanner.finish() {
                match build_facts.get(&key) {
                    Some((existing, source)) if existing != &value => {
                        return Err(Dayu200ParseError::ContradictoryBuildFact {
                            key,
                            first: format!("{existing} (from {source})"),
                            second: format!("{value} (from {})", header.path),
                        })
                    }
                    Some(_) => {}
                    None => {
                        build_facts.insert(key, (value, header.path.clone()));
                    }
                }
            }
        }

        members.push(ArchiveMemberFact {
            path: observation.path,
            size_bytes: observation.size,
            sha256: observation.sha256,
            role,
        });
    }

    let mut unknowns = Vec::new();
    let partition_table = match parameter_text {
        Some(text) => {
            let table = parse_parameter(&text, 512).map_err(Dayu200ParseError::Parameter)?;
            table.validate().map_err(Dayu200ParseError::Manifest)?;
            Some(table)
        }
        None => {
            unknowns.push(ExecutionUnknown {
                id: OpaqueId::new("RK-A01").expect("literal identifier"),
                summary: format!(
                    "archive carries no {PARTITION_TABLE_MEMBER}; the on-device layout is undeclared"
                ),
            });
            None
        }
    };

    if !unclassified.is_empty() {
        // An unclassified member is execution-relevant on purpose: something is
        // in the bundle that this parser cannot account for, and a plan built
        // over it would be a plan over partly-understood bytes
        // (architecture.md 5.5, AF-V1 "unknown member fail closed").
        unknowns.push(ExecutionUnknown {
            id: OpaqueId::new("RK-A02").expect("literal identifier"),
            summary: format!("unclassified archive members: {}", unclassified.join(", ")),
        });
    }
    if build_facts.is_empty() {
        unknowns.push(ExecutionUnknown {
            id: OpaqueId::new("RK-A03").expect("literal identifier"),
            summary: "no build facts were found inside any hashed image member".into(),
        });
    }

    let confidence = if unknowns.is_empty() {
        ParserConfidence::ProductionManifest
    } else {
        ParserConfidence::ResearchOnly
    };

    let manifest = ArtifactManifest {
        format: ArtifactFormat {
            id: OpaqueId::new(FORMAT_ID).expect("literal identifier"),
            version: FORMAT_VERSION,
        },
        // Filled in by `inspect` once the whole stream has been hashed.
        content_digest: arkforge_core::digest::sha256(b""),
        size_bytes: 0,
        members,
        partition_table,
        build_facts: build_facts
            .into_iter()
            .filter_map(|(key, (value, _))| OpaqueId::new(key).ok().map(|id| (id, value)))
            .collect(),
        unclassified_members: unclassified,
        execution_relevant_unknowns: unknowns,
        confidence,
    };
    manifest.validate().map_err(Dayu200ParseError::Manifest)?;
    Ok(manifest)
}

/// Wraps a reader to hash and count everything it yields.
#[derive(Debug)]
struct CountingHasher<R: Read> {
    inner: R,
    hasher: Sha256,
    count: u64,
}

impl<R: Read> CountingHasher<R> {
    fn new(inner: R) -> Self {
        CountingHasher {
            inner,
            hasher: Sha256::new(),
            count: 0,
        }
    }

    fn finish(self) -> (u64, Sha256Digest) {
        (self.count, self.hasher.finalize())
    }
}

impl<R: Read> Read for CountingHasher<R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let count = self.inner.read(out)?;
        self.hasher.update(&out[..count]);
        self.count += count as u64;
        Ok(count)
    }
}

/// Finds `key=value` build facts inside a streamed member.
///
/// Handles chunk boundaries by retaining a tail as long as the longest key plus
/// its value bound, so a fact split across two reads is still found.
#[derive(Debug)]
struct BuildFactScanner {
    tail: Vec<u8>,
    found: BTreeMap<String, String>,
    scanned: u64,
}

/// Longest plausible value; a properties line longer than this is not a fact
/// this parser will claim.
const MAX_VALUE_LEN: usize = 256;

impl BuildFactScanner {
    fn new() -> Self {
        BuildFactScanner {
            tail: Vec::new(),
            found: BTreeMap::new(),
            scanned: 0,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        if self.scanned >= BUILD_FACT_SCAN_BYTES {
            return;
        }
        self.scanned += chunk.len() as u64;

        let mut window = std::mem::take(&mut self.tail);
        window.extend_from_slice(chunk);
        self.scan(&window, false);

        let keep = MAX_VALUE_LEN + BUILD_FACT_KEYS.iter().map(|key| key.len()).max().unwrap_or(0);
        if window.len() > keep {
            self.tail = window[window.len() - keep..].to_vec();
        } else {
            self.tail = window;
        }
    }

    /// `at_end_of_member` distinguishes "the value runs to the end of this
    /// chunk" from "the value runs to the end of the member". Mid-stream, an
    /// unterminated value is a value split across reads, and recording its
    /// prefix would pin a truncated build string into the manifest.
    fn scan(&mut self, window: &[u8], at_end_of_member: bool) {
        for key in BUILD_FACT_KEYS {
            let needle = format!("{key}=");
            let needle = needle.as_bytes();
            let mut from = 0usize;
            while let Some(offset) = find(&window[from..], needle) {
                let start = from + offset + needle.len();
                let terminator = window[start..].iter().position(|byte| {
                    *byte == b'\n' || *byte == b'\r' || *byte == 0 || *byte == b' '
                });
                let end = match terminator {
                    Some(index) => Some(start + index),
                    None if at_end_of_member => Some(window.len()),
                    None => None,
                };
                if let Some(end) = end {
                    if end > start && end - start <= MAX_VALUE_LEN {
                        if let Ok(value) = std::str::from_utf8(&window[start..end]) {
                            let value = value.trim();
                            if !value.is_empty() && value.chars().all(|c| !c.is_control()) {
                                self.found
                                    .entry(key.to_string())
                                    .or_insert_with(|| value.to_string());
                            }
                        }
                    }
                }
                from = from + offset + needle.len();
            }
        }
    }

    fn finish(mut self) -> BTreeMap<String, String> {
        let tail = std::mem::take(&mut self.tail);
        self.scan(&tail, true);
        self.found
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact CMDLINE from the pinned DAYU200 archive, reconstructed from
    /// ArkDeck `partition-mapping.json` (`arkdeck-dayu200-partition-mapping-1.0.0`).
    const PINNED_CMDLINE: &str = concat!(
        "CMDLINE:mtdparts=rk29xxnand:",
        "0x00002000@0x00002000(uboot),",
        "0x00002000@0x00004000(misc),",
        "0x00001000@0x00006000(bootctrl),",
        "0x00003000@0x00007000(resource),",
        "0x00030000@0x0000A000(boot_linux:bootable),",
        "0x00002000@0x0003A000(ramdisk),",
        "0x00400000@0x0003C000(system),",
        "0x00200000@0x0043C000(vendor),",
        "0x00019000@0x0063C000(sys-prod),",
        "0x00019000@0x00655000(chip-prod),",
        "0x00010000@0x0066E000(updater),",
        "0x00008000@0x0067E000(eng_system),",
        "0x00008000@0x00686000(eng_chipset),",
        "0x00020000@0x0069E000(chip_ckm),",
        "-@0x01308000(userdata:grow)"
    );

    #[test]
    fn decodes_the_pinned_dayu200_partition_table() {
        let table = parse_parameter(PINNED_CMDLINE, 512).unwrap();
        table.validate().unwrap();
        assert_eq!(table.device, "rk29xxnand");
        assert_eq!(table.entries.len(), 15);

        // Spot-check every value ArkDeck's pinned decode records.
        let expected: [(&str, u64, Option<u64>, GrammarBranch); 15] = [
            ("uboot", 8192, Some(8192), GrammarBranch::Fixed),
            ("misc", 16384, Some(8192), GrammarBranch::Fixed),
            ("bootctrl", 24576, Some(4096), GrammarBranch::Fixed),
            ("resource", 28672, Some(12288), GrammarBranch::Fixed),
            ("boot_linux", 40960, Some(196_608), GrammarBranch::FixedBootable),
            ("ramdisk", 237_568, Some(8192), GrammarBranch::Fixed),
            ("system", 245_760, Some(4_194_304), GrammarBranch::Fixed),
            ("vendor", 4_440_064, Some(2_097_152), GrammarBranch::Fixed),
            ("sys-prod", 6_537_216, Some(102_400), GrammarBranch::Fixed),
            ("chip-prod", 6_639_616, Some(102_400), GrammarBranch::Fixed),
            ("updater", 6_742_016, Some(65536), GrammarBranch::Fixed),
            ("eng_system", 6_807_552, Some(32768), GrammarBranch::Fixed),
            ("eng_chipset", 6_840_320, Some(32768), GrammarBranch::Fixed),
            ("chip_ckm", 6_938_624, Some(131_072), GrammarBranch::Fixed),
            ("userdata", 19_955_712, None, GrammarBranch::RemainderGrow),
        ];
        for (index, (name, offset, size, branch)) in expected.into_iter().enumerate() {
            let entry = &table.entries[index];
            assert_eq!(entry.name, name, "entry {index}");
            assert_eq!(entry.offset_sectors, offset, "{name} offset");
            assert_eq!(entry.size_sectors, size, "{name} size");
            assert_eq!(entry.grammar_branch, branch, "{name} branch");
        }
        assert_eq!(
            table.entry("boot_linux").unwrap().attribute,
            Some(PartitionAttribute::Bootable)
        );
        assert_eq!(
            table.entry("userdata").unwrap().attribute,
            Some(PartitionAttribute::Grow)
        );
    }

    #[test]
    fn a_real_parameter_file_with_surrounding_lines_still_decodes() {
        let text = format!(
            "FIRMWARE_VER:1.0.0\nMACHINE_MODEL:RK3568\nMACHINE_ID:007\n{PINNED_CMDLINE} androidboot.selinux=permissive\n"
        );
        let table = parse_parameter(&text, 512).unwrap();
        assert_eq!(table.entries.len(), 15);
    }

    #[test]
    fn an_unknown_partition_attribute_fails_closed() {
        let text = "CMDLINE:mtdparts=rk29xxnand:0x00002000@0x00002000(uboot:secure)";
        assert!(matches!(
            parse_parameter(text, 512),
            Err(ParameterError::UnknownAttribute { .. })
        ));
    }

    #[test]
    fn malformed_descriptors_are_rejected() {
        let cases = [
            "CMDLINE:mtdparts=rk29xxnand:0x2000(uboot)",
            "CMDLINE:mtdparts=rk29xxnand:0x2000@0x1000uboot",
            "CMDLINE:mtdparts=rk29xxnand:0x2000@0x1000()",
            "CMDLINE:mtdparts=rk29xxnand:-@0x1000(userdata)",
            "CMDLINE:mtdparts=rk29xxnand:0x2000@0x1000(userdata:grow)",
            "CMDLINE:mtdparts=rk29xxnand:2000@0x1000(uboot)",
            "CMDLINE:mtdparts=rk29xxnand:0x2000@1000(uboot)",
            "CMDLINE:mtdparts=rk29xxnand:0xzz@0x1000(uboot)",
            "CMDLINE:mtdparts=rk29xxnand:",
            "CMDLINE:mtdparts=:0x2000@0x1000(uboot)",
            "CMDLINE:noparts=1",
            "MACHINE_MODEL:RK3568",
        ];
        for case in cases {
            assert!(
                parse_parameter(case, 512).is_err(),
                "{case:?} should have been rejected"
            );
        }
    }

    #[test]
    fn overlapping_partitions_in_a_parameter_file_are_caught_by_validation() {
        let text = "CMDLINE:mtdparts=rk29xxnand:0x00004000@0x00002000(uboot),0x00002000@0x00004000(misc)";
        let table = parse_parameter(text, 512).unwrap();
        assert!(table.validate().is_err());
    }

    #[test]
    fn build_fact_scanner_finds_a_fact_split_across_chunks() {
        let mut scanner = BuildFactScanner::new();
        let blob = b"junkconst.ohos.fullname=OpenHarmony-7.0.0.36\nmore junk";
        // Split in the middle of the value.
        scanner.push(&blob[..30]);
        scanner.push(&blob[30..]);
        let found = scanner.finish();
        assert_eq!(
            found.get("const.ohos.fullname").map(String::as_str),
            Some("OpenHarmony-7.0.0.36")
        );
    }

    #[test]
    fn build_fact_scanner_ignores_a_key_with_no_value() {
        let mut scanner = BuildFactScanner::new();
        scanner.push(b"const.product.model=\n");
        assert!(scanner.finish().is_empty());
    }
}
