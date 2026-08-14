//! DAYU600 `unisoc-pac` research parser.
//!
//! architecture.md 10.5, 17.1. The governing fact is UNI-U01: **the PAC format
//! is not known to this project**. There is no specification, no sample, and no
//! authorized capture in either repository. The only confirmed evidence is
//! static and indirect (AD-004: BlueTool ships `CmdDloader.exe`, UNISOC DLLs
//! and PAC resources, and `ohos.boot.hardware` answers `uis7885` on DAYU600).
//!
//! So this module does not parse PAC. It **observes** a container and records
//! what a reader can see without a specification: sizes, hashes, string runs,
//! repeating aligned structure, entropy boundaries. Each observation carries
//! the rule that produced it, so a researcher can tell an observation from an
//! interpretation.
//!
//! What this module must never do, and structurally cannot:
//!
//! - claim a byte range *is* a PAC header, table or FDL image;
//! - emit a partition, an address, a load address or an erase policy;
//! - return anything but [`ParserConfidence::ResearchOnly`];
//! - clear an entry from the unknown list.
//!
//! A future production PAC manifest is not an upgrade of this code. It is a
//! different output, gated on architecture.md 17.5, and it will be written when
//! the evidence exists.

use crate::manifest::{ArchiveMemberFact, ArtifactManifest, MemberRole, ParserConfidence};
use arkforge_core::digest::{sha256, Sha256, Sha256Digest};
use arkforge_core::identity::{ArtifactFormat, Version};
use arkforge_core::ids::OpaqueId;
use arkforge_core::plan::ExecutionUnknown;
use core::fmt;
use std::io::Read;

pub const FORMAT_ID: &str = "unisoc-pac";
pub const FORMAT_VERSION: Version = Version::new(0, 1, 0);

/// Bound on what a research inspection will hold in memory.
///
/// A research parser reads the whole container to observe it; a container
/// larger than this is refused rather than streamed blind, because every
/// observation below needs random access.
pub const MAX_RESEARCH_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Block size for the entropy map.
const ENTROPY_BLOCK: usize = 4096;
/// Shortest string run worth recording.
const MIN_STRING_RUN: usize = 4;
/// Cap on how many candidates of each kind are recorded, so a hostile or merely
/// unusual container cannot produce an unbounded report.
const MAX_CANDIDATES_PER_KIND: usize = 256;

/// What kind of observation produced a candidate.
///
/// Every name here describes **what was seen**, not what it means. There is
/// deliberately no `PacHeader` or `FdlImage` variant: naming one would be an
/// interpretation this project has no evidence for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CandidateKind {
    /// A run of printable ASCII.
    AsciiStringRun,
    /// A run of printable UTF-16LE. Recorded separately because Windows-authored
    /// containers commonly carry UTF-16LE text, and mixing the two would hide
    /// which encoding was actually seen.
    Utf16LeStringRun,
    /// A block whose byte distribution is close to uniform — consistent with
    /// compressed or encrypted payload, and consistent with several other
    /// things.
    HighEntropyRegion,
    /// A block whose byte distribution is far from uniform — consistent with
    /// structured metadata, and consistent with several other things.
    LowEntropyRegion,
    /// A repeating aligned stride: many offsets `k*S` share a byte pattern.
    /// Consistent with a record table; also consistent with padding.
    RepeatingStride,
    /// A long run of one byte value.
    UniformFill,
}

impl CandidateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CandidateKind::AsciiStringRun => "asciiStringRun",
            CandidateKind::Utf16LeStringRun => "utf16leStringRun",
            CandidateKind::HighEntropyRegion => "highEntropyRegion",
            CandidateKind::LowEntropyRegion => "lowEntropyRegion",
            CandidateKind::RepeatingStride => "repeatingStride",
            CandidateKind::UniformFill => "uniformFill",
        }
    }
}

/// One structural observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructureCandidate {
    pub kind: CandidateKind,
    pub offset: u64,
    pub length: u64,
    /// Hash of the observed bytes, so a later capture can be compared to this
    /// one without redistributing the container.
    pub sha256: Sha256Digest,
    /// The rule that produced this candidate, in words. A reader must be able
    /// to tell an observation from an interpretation without reading the code.
    pub basis: String,
    /// A short rendering, for string runs only. Never raw payload.
    pub preview: Option<String>,
}

/// The complete research inspection of one container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacResearchReport {
    pub size_bytes: u64,
    pub content_digest: Sha256Digest,
    /// Hash of the first 4 KiB and the last 4 KiB, so two containers can be
    /// compared at their edges cheaply.
    pub head_digest: Sha256Digest,
    pub tail_digest: Sha256Digest,
    pub candidates: Vec<StructureCandidate>,
    /// Set when a limit truncated the report, so an empty tail is never read as
    /// "nothing more was there".
    pub truncated_kinds: Vec<CandidateKind>,
}

/// The exact unknown list for DAYU600 execution (architecture.md 17.1).
///
/// This is the AF-V3 acceptance item "exact unknown list". Each entry names one
/// fact, so evidence can close it individually rather than by a wave of the
/// hand — and so a partially answered question stays visibly open.
pub const DAYU600_EXECUTION_UNKNOWNS: [(&str, &str); 12] = [
    (
        "UNI-U01",
        "PAC container format and version: no specification, sample or authorized capture is held \
         by this project. Every structural observation this parser emits is an observation, not a \
         field.",
    ),
    (
        "UNI-U02",
        "PAC signature and checksum scheme: unknown, so container integrity cannot be verified \
         beyond a whole-file hash of the bytes as received.",
    ),
    (
        "UNI-U03",
        "FDL1/FDL2 identity, load addresses, entry points and stage order: unknown.",
    ),
    (
        "UNI-U04",
        "FDL security handshake: unknown whether one exists, and if so what it requires.",
    ),
    (
        "UNI-U05",
        "Download-mode USB identity: VID/PID, interface and endpoint layout in BootROM, FDL1 and \
         FDL2 modes are unmeasured.",
    ),
    (
        "UNI-U06",
        "Stable chip/device unique identifier readable in download mode: unknown, so exact target \
         identity across a mode change cannot be proven.",
    ),
    (
        "UNI-U07",
        "Download protocol request/ACK/error/timeout semantics: unknown, so no dispatch can be \
         classified as confirmed-no-effect rather than outcome-unknown.",
    ),
    (
        "UNI-U08",
        "Storage geometry, partition table representation, erase policy, write order and verify \
         algorithm: unknown.",
    ),
    (
        "UNI-U09",
        "Data impact of a full restore on userdata, calibration, NV and secure storage: unknown.",
    ),
    (
        "UNI-U10",
        "Cancellation and recovery semantics, including whether any write is atomically \
         cancellable: unknown.",
    ),
    (
        "UNI-U11",
        "Host driver requirements on macOS, Linux and Windows: unmeasured; support may not be \
         declared where it was not tested.",
    ),
    (
        "UNI-U12",
        "Vendor tool licence and redistribution terms for CmdDloader and the UNISOC libraries: \
         unknown, and an unknown licence defaults to not redistributable (architecture.md 24.1).",
    ),
];

/// The unknown list as plan-level unknowns.
pub fn dayu600_execution_unknowns() -> Vec<ExecutionUnknown> {
    DAYU600_EXECUTION_UNKNOWNS
        .iter()
        .map(|(id, summary)| ExecutionUnknown {
            id: OpaqueId::new(*id).expect("literal identifier"),
            summary: (*summary).to_string(),
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PacParseError {
    Empty,
    TooLarge(u64),
    Io(String),
}

impl fmt::Display for PacParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PacParseError::Empty => f.write_str("container is empty"),
            PacParseError::TooLarge(size) => write!(
                f,
                "container of {size} bytes exceeds the {MAX_RESEARCH_BYTES}-byte research bound"
            ),
            PacParseError::Io(detail) => write!(f, "container read failed: {detail}"),
        }
    }
}

impl std::error::Error for PacParseError {}

/// Observes a container and produces a research report.
pub fn inspect_research<R: Read>(mut source: R) -> Result<PacResearchReport, PacParseError> {
    let mut bytes = Vec::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let count = source
            .read(&mut buffer)
            .map_err(|error| PacParseError::Io(error.to_string()))?;
        if count == 0 {
            break;
        }
        if bytes.len() as u64 + count as u64 > MAX_RESEARCH_BYTES {
            return Err(PacParseError::TooLarge(bytes.len() as u64 + count as u64));
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    if bytes.is_empty() {
        return Err(PacParseError::Empty);
    }
    Ok(observe(&bytes))
}

fn observe(bytes: &[u8]) -> PacResearchReport {
    let edge = 4096.min(bytes.len());
    let mut candidates = Vec::new();
    let mut truncated = Vec::new();

    collect_ascii_runs(bytes, &mut candidates, &mut truncated);
    collect_utf16le_runs(bytes, &mut candidates, &mut truncated);
    collect_entropy_regions(bytes, &mut candidates, &mut truncated);
    collect_uniform_fills(bytes, &mut candidates, &mut truncated);
    collect_repeating_strides(bytes, &mut candidates, &mut truncated);

    candidates.sort_by(|left, right| {
        left.offset
            .cmp(&right.offset)
            .then(left.kind.cmp(&right.kind))
            .then(left.length.cmp(&right.length))
    });

    PacResearchReport {
        size_bytes: bytes.len() as u64,
        content_digest: sha256(bytes),
        head_digest: sha256(&bytes[..edge]),
        tail_digest: sha256(&bytes[bytes.len() - edge..]),
        candidates,
        truncated_kinds: truncated,
    }
}

fn push(
    candidates: &mut Vec<StructureCandidate>,
    truncated: &mut Vec<CandidateKind>,
    candidate: StructureCandidate,
) {
    let already = candidates
        .iter()
        .filter(|existing| existing.kind == candidate.kind)
        .count();
    if already >= MAX_CANDIDATES_PER_KIND {
        if !truncated.contains(&candidate.kind) {
            truncated.push(candidate.kind);
        }
        return;
    }
    candidates.push(candidate);
}

fn preview(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .filter(|c| !c.is_control())
        .take(48)
        .collect();
    cleaned
}

fn collect_ascii_runs(
    bytes: &[u8],
    candidates: &mut Vec<StructureCandidate>,
    truncated: &mut Vec<CandidateKind>,
) {
    let mut start = None;
    for index in 0..=bytes.len() {
        let printable = index < bytes.len() && is_printable_ascii(bytes[index]);
        match (printable, start) {
            (true, None) => start = Some(index),
            (false, Some(begin)) => {
                let length = index - begin;
                if length >= MIN_STRING_RUN {
                    let slice = &bytes[begin..index];
                    push(
                        candidates,
                        truncated,
                        StructureCandidate {
                            kind: CandidateKind::AsciiStringRun,
                            offset: begin as u64,
                            length: length as u64,
                            sha256: sha256(slice),
                            basis: format!(
                                "{length} consecutive printable ASCII bytes (>= {MIN_STRING_RUN})"
                            ),
                            preview: Some(preview(&String::from_utf8_lossy(slice))),
                        },
                    );
                }
                start = None;
            }
            _ => {}
        }
    }
}

fn is_printable_ascii(byte: u8) -> bool {
    (0x20..=0x7e).contains(&byte)
}

fn collect_utf16le_runs(
    bytes: &[u8],
    candidates: &mut Vec<StructureCandidate>,
    truncated: &mut Vec<CandidateKind>,
) {
    // A UTF-16LE run of ASCII text is `xx 00 yy 00 …`. Scanning on even and odd
    // alignments both, because a table's text field need not be 2-aligned to
    // the file start.
    for alignment in [0usize, 1] {
        let mut index = alignment;
        while index + 1 < bytes.len() {
            if !(is_printable_ascii(bytes[index]) && bytes[index + 1] == 0) {
                index += 2;
                continue;
            }
            let begin = index;
            while index + 1 < bytes.len()
                && is_printable_ascii(bytes[index])
                && bytes[index + 1] == 0
            {
                index += 2;
            }
            let units = (index - begin) / 2;
            if units >= MIN_STRING_RUN {
                let slice = &bytes[begin..index];
                let text: String = slice
                    .chunks(2)
                    .map(|pair| pair[0] as char)
                    .collect();
                push(
                    candidates,
                    truncated,
                    StructureCandidate {
                        kind: CandidateKind::Utf16LeStringRun,
                        offset: begin as u64,
                        length: (index - begin) as u64,
                        sha256: sha256(slice),
                        basis: format!(
                            "{units} consecutive UTF-16LE code units in the printable ASCII range, \
                             scanned at byte alignment {alignment}"
                        ),
                        preview: Some(preview(&text)),
                    },
                );
            }
        }
    }
}

/// Shannon entropy of a block, in bits per byte.
fn entropy(block: &[u8]) -> f64 {
    let mut counts = [0u32; 256];
    for byte in block {
        counts[*byte as usize] += 1;
    }
    let total = block.len() as f64;
    let mut sum = 0.0f64;
    for count in counts {
        if count == 0 {
            continue;
        }
        let probability = count as f64 / total;
        sum -= probability * probability.log2();
    }
    sum
}

fn collect_entropy_regions(
    bytes: &[u8],
    candidates: &mut Vec<StructureCandidate>,
    truncated: &mut Vec<CandidateKind>,
) {
    // Classify blocks, then merge runs of the same class so the report shows
    // regions rather than a histogram.
    let mut current: Option<(CandidateKind, usize, usize)> = None;
    let mut offset = 0usize;
    while offset < bytes.len() {
        let end = (offset + ENTROPY_BLOCK).min(bytes.len());
        let block = &bytes[offset..end];
        let bits = entropy(block);
        // 7.5 bits/byte is the usual working line between "compressed or
        // encrypted" and "structured"; it is a heuristic and the basis string
        // says so.
        let kind = if bits >= 7.5 {
            CandidateKind::HighEntropyRegion
        } else {
            CandidateKind::LowEntropyRegion
        };
        match current {
            Some((existing, begin, _)) if existing == kind => {
                current = Some((existing, begin, end));
            }
            Some((existing, begin, finish)) => {
                emit_region(bytes, existing, begin, finish, candidates, truncated);
                current = Some((kind, offset, end));
            }
            None => current = Some((kind, offset, end)),
        }
        offset = end;
    }
    if let Some((kind, begin, finish)) = current {
        emit_region(bytes, kind, begin, finish, candidates, truncated);
    }
}

fn emit_region(
    bytes: &[u8],
    kind: CandidateKind,
    begin: usize,
    end: usize,
    candidates: &mut Vec<StructureCandidate>,
    truncated: &mut Vec<CandidateKind>,
) {
    let slice = &bytes[begin..end];
    let bits = entropy(slice);
    push(
        candidates,
        truncated,
        StructureCandidate {
            kind,
            offset: begin as u64,
            length: slice.len() as u64,
            sha256: sha256(slice),
            basis: format!(
                "Shannon entropy {bits:.2} bits/byte over {} bytes, classified against a 7.5 \
                 bits/byte heuristic; this says nothing about what the region contains",
                slice.len()
            ),
            preview: None,
        },
    );
}

fn collect_uniform_fills(
    bytes: &[u8],
    candidates: &mut Vec<StructureCandidate>,
    truncated: &mut Vec<CandidateKind>,
) {
    const MIN_FILL: usize = 64;
    let mut index = 0usize;
    while index < bytes.len() {
        let value = bytes[index];
        let begin = index;
        while index < bytes.len() && bytes[index] == value {
            index += 1;
        }
        let length = index - begin;
        if length >= MIN_FILL {
            push(
                candidates,
                truncated,
                StructureCandidate {
                    kind: CandidateKind::UniformFill,
                    offset: begin as u64,
                    length: length as u64,
                    sha256: sha256(&bytes[begin..index]),
                    basis: format!("{length} consecutive bytes of value {value:#04x}"),
                    preview: None,
                },
            );
        }
    }
}

fn collect_repeating_strides(
    bytes: &[u8],
    candidates: &mut Vec<StructureCandidate>,
    truncated: &mut Vec<CandidateKind>,
) {
    // Look for a stride S where the byte at every k*S matches, over a long run.
    // A record table often has a constant type or magic in each entry. This is
    // a hypothesis generator, and the basis string says exactly that.
    const STRIDES: [usize; 8] = [8, 16, 24, 32, 48, 64, 128, 256];
    const MIN_REPEATS: usize = 8;

    for stride in STRIDES {
        if bytes.len() < stride * MIN_REPEATS {
            continue;
        }
        let mut start = 0usize;
        while start + stride * MIN_REPEATS <= bytes.len() {
            let anchor = bytes[start];
            // Ignore an all-zero anchor: padding would otherwise dominate.
            if anchor == 0 {
                start += stride;
                continue;
            }
            let mut repeats = 1usize;
            while start + repeats * stride < bytes.len()
                && bytes[start + repeats * stride] == anchor
            {
                repeats += 1;
            }
            if repeats >= MIN_REPEATS {
                let length = repeats * stride;
                let end = (start + length).min(bytes.len());
                push(
                    candidates,
                    truncated,
                    StructureCandidate {
                        kind: CandidateKind::RepeatingStride,
                        offset: start as u64,
                        length: (end - start) as u64,
                        sha256: sha256(&bytes[start..end]),
                        basis: format!(
                            "byte {anchor:#04x} recurs at every {stride}-byte offset for {repeats} \
                             repeats; consistent with a record table and equally consistent with \
                             padding — this is a hypothesis, not a table"
                        ),
                        preview: None,
                    },
                );
                start = end;
            } else {
                start += stride;
            }
        }
    }
}

/// Produces an `ArtifactManifest` for a container.
///
/// The manifest is `ResearchOnly` and carries the complete unknown list. It has
/// no partition table, because this parser has no basis for one: emitting an
/// empty table would read as "the container declares no partitions", which is a
/// claim, whereas `None` reads as "this parser cannot say".
pub fn inspect<R: Read>(source: R) -> Result<(ArtifactManifest, PacResearchReport), PacParseError> {
    let report = inspect_research(source)?;

    // Candidates are reported as members so the same manifest shape serves both
    // formats — with roles that say `Unclassified`, because that is what they
    // are.
    let members = report
        .candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| ArchiveMemberFact {
            path: format!(
                "candidate/{index:04}/{}@{}",
                candidate.kind.as_str(),
                candidate.offset
            ),
            size_bytes: candidate.length,
            sha256: candidate.sha256,
            role: MemberRole::Unclassified,
        })
        .collect();

    let manifest = ArtifactManifest {
        format: ArtifactFormat {
            id: OpaqueId::new(FORMAT_ID).expect("literal identifier"),
            version: FORMAT_VERSION,
        },
        content_digest: report.content_digest,
        size_bytes: report.size_bytes,
        members,
        // Not `Some(empty)`: this parser cannot say what the layout is.
        partition_table: None,
        build_facts: Vec::new(),
        unclassified_members: report
            .candidates
            .iter()
            .map(|candidate| candidate.kind.as_str().to_string())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect(),
        execution_relevant_unknowns: dayu600_execution_unknowns(),
        // Never anything else, from any input.
        confidence: ParserConfidence::ResearchOnly,
    };
    Ok((manifest, report))
}

/// A digest over the whole research report, so two inspections of the same
/// container can be compared without redistributing it.
pub fn report_digest(report: &PacResearchReport) -> Sha256Digest {
    let mut hasher = Sha256::new();
    hasher.update(b"arkforge/v1/pac-research-report\0");
    hasher.update(&report.size_bytes.to_be_bytes());
    hasher.update(report.content_digest.as_bytes());
    for candidate in &report.candidates {
        hasher.update(candidate.kind.as_str().as_bytes());
        hasher.update(&candidate.offset.to_be_bytes());
        hasher.update(&candidate.length.to_be_bytes());
        hasher.update(candidate.sha256.as_bytes());
    }
    hasher.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A container shaped like a firmware package — a text header, a
    /// record-like table, a compressed-looking payload and padding — built so
    /// the observation rules have something to find. It is **not** a PAC file
    /// and this test does not claim it resembles one.
    fn synthetic_container() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"BP_R1.0.0");
        bytes.extend_from_slice(&[0u8; 7]);
        // UTF-16LE names, in a 32-byte stride, each entry starting with 0x02.
        for index in 0..12 {
            let entry_start = bytes.len();
            bytes.push(0x02);
            bytes.push(index as u8);
            for character in format!("IMG_{index}").chars() {
                bytes.push(character as u8);
                bytes.push(0);
            }
            while bytes.len() - entry_start < 32 {
                bytes.push(0);
            }
        }
        // Payload that looks compressed.
        let mut state = 0x2468_1357u32;
        for _ in 0..20_000 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            bytes.push((state >> 24) as u8);
        }
        // Padding.
        bytes.extend_from_slice(&[0xffu8; 4096]);
        bytes
    }

    #[test]
    fn a_container_yields_observations_not_interpretations() {
        let bytes = synthetic_container();
        let (manifest, report) = inspect(bytes.as_slice()).unwrap();

        assert_eq!(report.size_bytes, bytes.len() as u64);
        assert_eq!(report.content_digest, sha256(&bytes));
        assert!(!report.candidates.is_empty());

        // Every candidate names the rule that produced it, and no candidate
        // claims a PAC field.
        for candidate in &report.candidates {
            assert!(
                !candidate.basis.is_empty(),
                "{candidate:?} has no stated basis"
            );
            let rendered = format!("{candidate:?}").to_lowercase();
            for forbidden in ["fdl", "partition", "loadaddress", "pacheader"] {
                assert!(
                    !rendered.contains(forbidden),
                    "candidate claims {forbidden:?}: {candidate:?}"
                );
            }
        }

        assert_eq!(manifest.confidence, ParserConfidence::ResearchOnly);
        assert!(manifest.partition_table.is_none());
        assert!(manifest.build_facts.is_empty());
        manifest.validate().unwrap();
    }

    #[test]
    fn the_unknown_list_is_complete_and_carried_into_every_manifest() {
        let (manifest, _) = inspect(synthetic_container().as_slice()).unwrap();
        let ids: Vec<&str> = manifest
            .execution_relevant_unknowns
            .iter()
            .map(|unknown| unknown.id.as_str())
            .collect();
        assert_eq!(ids.len(), DAYU600_EXECUTION_UNKNOWNS.len());
        for (expected, _) in DAYU600_EXECUTION_UNKNOWNS {
            assert!(ids.contains(&expected), "{expected} is missing");
        }
        // UNI-U01 is the one architecture.md 24 names as `missing`; it must be
        // present and it must be about the format itself.
        let uni_u01 = manifest
            .execution_relevant_unknowns
            .iter()
            .find(|unknown| unknown.id.as_str() == "UNI-U01")
            .unwrap();
        assert!(uni_u01.summary.contains("no specification"));
    }

    #[test]
    fn no_input_can_produce_a_production_manifest() {
        // The property that matters: whatever a container contains, the parser
        // cannot be talked into claiming it understands it.
        let inputs: Vec<Vec<u8>> = vec![
            synthetic_container(),
            b"BP_R1.0.0\0\0\0\0\0\0\0".to_vec(),
            vec![0u8; 100_000],
            (0..=255u8).cycle().take(50_000).collect(),
            b"this is not a firmware container at all".to_vec(),
        ];
        for input in inputs {
            let (manifest, _) = inspect(input.as_slice()).unwrap();
            assert_eq!(manifest.confidence, ParserConfidence::ResearchOnly);
            assert!(!manifest.execution_relevant_unknowns.is_empty());
            // And the manifest is self-consistent: a ResearchOnly manifest with
            // open unknowns is exactly what `validate` permits.
            manifest.validate().unwrap();
        }
    }

    #[test]
    fn string_runs_are_found_in_both_encodings() {
        let bytes = synthetic_container();
        let report = observe(&bytes);
        assert!(report
            .candidates
            .iter()
            .any(|candidate| candidate.kind == CandidateKind::AsciiStringRun
                && candidate.preview.as_deref() == Some("BP_R1.0.0")));
        assert!(report.candidates.iter().any(|candidate| {
            candidate.kind == CandidateKind::Utf16LeStringRun
                && candidate
                    .preview
                    .as_deref()
                    .map(|text| text.starts_with("IMG_"))
                    .unwrap_or(false)
        }));
    }

    #[test]
    fn a_record_like_stride_is_reported_as_a_hypothesis() {
        let report = observe(&synthetic_container());
        let stride = report
            .candidates
            .iter()
            .find(|candidate| candidate.kind == CandidateKind::RepeatingStride)
            .expect("the 32-byte entry stride should be visible");
        assert!(
            stride.basis.contains("hypothesis, not a table"),
            "{}",
            stride.basis
        );
    }

    #[test]
    fn entropy_separates_the_payload_from_the_metadata() {
        let report = observe(&synthetic_container());
        assert!(report
            .candidates
            .iter()
            .any(|candidate| candidate.kind == CandidateKind::HighEntropyRegion));
        assert!(report
            .candidates
            .iter()
            .any(|candidate| candidate.kind == CandidateKind::LowEntropyRegion));
    }

    #[test]
    fn padding_is_reported_as_uniform_fill_rather_than_as_content() {
        let report = observe(&synthetic_container());
        let fill = report
            .candidates
            .iter()
            .find(|candidate| candidate.kind == CandidateKind::UniformFill)
            .expect("4096 bytes of 0xff should be visible");
        assert!(fill.length >= 4096);
        assert!(fill.basis.contains("0xff"));
    }

    #[test]
    fn the_report_is_deterministic() {
        let bytes = synthetic_container();
        assert_eq!(report_digest(&observe(&bytes)), report_digest(&observe(&bytes)));
        // And it is content-sensitive.
        let mut edited = bytes.clone();
        edited[0] ^= 0xff;
        assert_ne!(report_digest(&observe(&bytes)), report_digest(&observe(&edited)));
    }

    #[test]
    fn candidate_counts_are_bounded_and_truncation_is_reported() {
        // Many short ASCII runs separated by non-printables.
        let mut bytes = Vec::new();
        for index in 0..(MAX_CANDIDATES_PER_KIND + 50) {
            bytes.extend_from_slice(format!("RUN{index:05}").as_bytes());
            bytes.push(0x01);
        }
        let report = observe(&bytes);
        let ascii = report
            .candidates
            .iter()
            .filter(|candidate| candidate.kind == CandidateKind::AsciiStringRun)
            .count();
        assert_eq!(ascii, MAX_CANDIDATES_PER_KIND);
        assert!(
            report.truncated_kinds.contains(&CandidateKind::AsciiStringRun),
            "truncation must be reported, or an empty tail reads as nothing being there"
        );
    }

    #[test]
    fn an_empty_container_is_refused() {
        assert_eq!(inspect_research(&[][..]), Err(PacParseError::Empty));
    }
}
