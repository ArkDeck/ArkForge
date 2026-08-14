//! The Rockchip fixed-tool execution side.
//!
//! architecture.md 16.1: a fixed executable, direct spawn, no shell, no PATH
//! resolution, no caller argv, and a closed enum lowering inside the Provider.
//! The last clause is the one this module is built around — [`RockUsbCommand`]
//! is the entire vocabulary, [`RockUsbCommand::argv`] is the only place an argv
//! is constructed, and a caller has no way to reach a subprocess except through
//! a command this enum can spell.
//!
//! Two commands the vendor tool offers are deliberately absent:
//!
//! - `db`/`ul`/`gpt` — Maskrom-stage commands. This Provider declares itself
//!   applicable in Loader mode only, so on an inapplicable device it blocks
//!   rather than reaching for something adjacent.
//! - `wl` — sector-addressed write. `wlx` resolves the address from the
//!   device's own table, which [`StoredAction::ValidatePartitionTable`] has
//!   just proved equal to the plan's. A sector-addressed fallback would let a
//!   write land at an address no observation confirmed, and no evidence has
//!   ever shown `wlx` needing one (AD-006).

use arkforge_artifact::manifest::PartitionTableFact;
use arkforge_core::digest::{sha256, CborValue, Sha256};
use arkforge_core::effect::ByteRange;
use arkforge_core::ids::{ActionId, OpaqueId, StepId};
use arkforge_core::outcome::ActionDisposition;
use arkforge_core::profile::DeviceProfile;
use arkforge_core::projection::PrivateActionRecord;
use arkforge_core::verification::{
    FailureClassification, TypedSkipReason, VerificationOutcome, VerificationStrength,
};
use arkforge_core::Sha256Digest;
use core::fmt;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The tool's own success markers. Semantic success is the marker, never the
/// exit status (architecture.md 12.4).
pub const WRITE_SUCCESS_MARKER: &str = "Write LBA from file (100%)";
pub const RESET_SUCCESS_MARKER: &str = "Reset Device OK.";

/// The flash device string the tool and the archive both spell.
const DEVICE_STRING: &str = "rk29xxnand";

/// `a / b` rounded up. `u64::div_ceil` is unstable on this toolchain.
fn ceil_div(numerator: u64, denominator: u64) -> u64 {
    numerator / denominator + u64::from(numerator % denominator != 0)
}

/// Sector size the tool addresses in. Not a Profile fact: it is the unit the
/// `rl`/`wl` command grammar itself is written in.
pub const TOOL_SECTOR_BYTES: u64 = 512;

/// The entire command surface this Provider can express.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RockUsbCommand {
    /// `ld` — list devices and their mode.
    ListDevices,
    /// `ppt` — print the device's own partition table.
    PrintPartitionTable,
    /// `wlx <partition> <image>` — write, addressed by partition name.
    WriteByName { partition: String, image: PathBuf },
    /// `rl <begin> <count> <out>` — read sectors to a file.
    ReadSectors {
        begin_sector: u64,
        sectors: u64,
        out: PathBuf,
    },
    /// `rd` — reset the device.
    ResetDevice,
}

impl RockUsbCommand {
    /// The only place an argv is built.
    pub fn argv(&self) -> Vec<String> {
        match self {
            RockUsbCommand::ListDevices => vec!["ld".into()],
            RockUsbCommand::PrintPartitionTable => vec!["ppt".into()],
            RockUsbCommand::WriteByName { partition, image } => vec![
                "wlx".into(),
                partition.clone(),
                image.display().to_string(),
            ],
            RockUsbCommand::ReadSectors {
                begin_sector,
                sectors,
                out,
            } => vec![
                "rl".into(),
                begin_sector.to_string(),
                sectors.to_string(),
                out.display().to_string(),
            ],
            RockUsbCommand::ResetDevice => vec!["rd".into()],
        }
    }

    /// The marker that means this command did what it claims, or `None` when
    /// the command's evidence is its output rather than a marker.
    pub fn success_marker(&self) -> Option<&'static str> {
        match self {
            RockUsbCommand::WriteByName { .. } => Some(WRITE_SUCCESS_MARKER),
            RockUsbCommand::ResetDevice => Some(RESET_SUCCESS_MARKER),
            RockUsbCommand::ListDevices
            | RockUsbCommand::PrintPartitionTable
            | RockUsbCommand::ReadSectors { .. } => None,
        }
    }

    /// Whether killing the child is safe. A partition write is not
    /// interruptible: the tool holds the device mid-transfer, and a kill leaves
    /// the outcome unknown rather than cancelled (architecture.md 13.4).
    pub fn is_interruptible(&self) -> bool {
        !matches!(self, RockUsbCommand::WriteByName { .. })
    }

    /// Whether this command can change the device.
    pub fn is_read_only(&self) -> bool {
        matches!(
            self,
            RockUsbCommand::ListDevices
                | RockUsbCommand::PrintPartitionTable
                | RockUsbCommand::ReadSectors { .. }
        )
    }
}

/// One invocation, as the port receives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInvocation {
    pub argv: Vec<String>,
    /// Output beyond this is truncated rather than buffered without bound.
    pub stdout_budget: usize,
    pub interruptible: bool,
}

/// What the port observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolReceipt {
    pub exited_zero: bool,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub duration_ms: u64,
}

impl ToolReceipt {
    /// Everything the tool said, which is what evidence digests cover.
    pub fn combined_output(&self) -> String {
        let mut text = self.stdout.clone();
        text.push_str(&self.stderr);
        text
    }

    pub fn evidence_digest(&self) -> Sha256Digest {
        sha256(self.combined_output().as_bytes())
    }
}

/// The subprocess boundary.
///
/// The port receives an argv the Provider lowered from [`RockUsbCommand`]; it
/// never receives one from outside. An implementation binds one pinned
/// executable and is responsible for proving its identity before the first
/// spawn — which executable is a host fact, not a plan fact, so it does not
/// appear in the invocation.
pub trait FixedToolPort: fmt::Debug {
    fn run(&self, invocation: &ToolInvocation) -> Result<ToolReceipt, String>;
}

/// An image extracted from a hashed archive and waiting to be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedImage {
    pub member: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub sha256: Sha256Digest,
}

impl StagedImage {
    /// Re-reads the staged file and checks it is still what was staged.
    ///
    /// Called immediately before the write, not once at staging time: the file
    /// lives on a filesystem other processes can reach, and the gap between
    /// "verified" and "written" is the whole window an attacker or a stray
    /// build script needs.
    pub fn revalidate(&self) -> Result<(), ExecutionError> {
        let mut file = std::fs::File::open(&self.path).map_err(|error| {
            ExecutionError::StagingChanged(format!("{}: {error}", self.path.display()))
        })?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 1 << 20];
        let mut total = 0u64;
        loop {
            let read = file.read(&mut buffer).map_err(|error| {
                ExecutionError::StagingChanged(format!("{}: {error}", self.path.display()))
            })?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            total += read as u64;
        }
        if total != self.size_bytes {
            return Err(ExecutionError::StagingChanged(format!(
                "{} is {total} bytes, was staged at {}",
                self.path.display(),
                self.size_bytes
            )));
        }
        let digest = hasher.finalize();
        if digest != self.sha256 {
            return Err(ExecutionError::StagingChanged(format!(
                "{} hashes to {digest}, was staged as {}",
                self.path.display(),
                self.sha256
            )));
        }
        Ok(())
    }
}

/// What the medium's read face turned out to be.
///
/// architecture.md 16.4: the read window is a runtime-measured fact, never a
/// Profile constant. This is the measurement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeasuredReadDomain {
    /// The backup table at the far end of the medium read back as real data,
    /// so the read face reaches the whole medium.
    Full,
    /// The far end read back as uniform filler while the near end read real
    /// data. Reads past the window return filler regardless of what is on the
    /// medium, so filler carries no information about content out there.
    Windowed { detail: String },
}

impl MeasuredReadDomain {
    pub fn summary(&self) -> &'static str {
        match self {
            MeasuredReadDomain::Full => "full",
            MeasuredReadDomain::Windowed { .. } => "windowed",
        }
    }
}

/// What one action turned out to be, once decoded from its stored body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredAction {
    /// Handled by the authority's control port, not by this Provider
    /// (architecture.md 9.2). Decoded so the caller knows what to ask for, and
    /// what answer would satisfy it.
    ManagedControl {
        control_action: String,
        /// Facts the booted device must answer with, for a postflight probe.
        /// Empty for a control action that only changes state.
        expect: Vec<(String, String)>,
    },
    ProbeLoader,
    ValidatePartitionTable {
        expected_layout_digest: Sha256Digest,
    },
    WritePartition {
        partition: String,
        member: String,
        begin_sector: u64,
    },
    ReadbackPartition {
        partition: String,
        begin_sector: u64,
        max_strength_when_readable: VerificationStrength,
        erased_medium_filler: Option<u8>,
    },
    CharacterizeReadDomain,
    ResetDevice,
}

impl StoredAction {
    /// Decodes the Provider's own stored body.
    ///
    /// Nothing is inferred and nothing defaults: a body this Provider did not
    /// write is an error, because the alternative is executing a half-understood
    /// instruction against a device.
    pub fn decode(record: &PrivateActionRecord) -> Result<StoredAction, ExecutionError> {
        let CborValue::Map(entries) = &record.body else {
            return Err(ExecutionError::ActionUndecodable("body is not a map".into()));
        };
        let field = |name: &str| -> Option<&CborValue> {
            entries
                .iter()
                .find(|(key, _)| matches!(key, CborValue::Text(text) if text == name))
                .map(|(_, value)| value)
        };
        let text = |name: &str| -> Result<String, ExecutionError> {
            match field(name) {
                Some(CborValue::Text(value)) => Ok(value.clone()),
                _ => Err(ExecutionError::ActionUndecodable(format!(
                    "field {name:?} is missing or not text"
                ))),
            }
        };
        let unsigned = |name: &str| -> Result<u64, ExecutionError> {
            match field(name) {
                Some(CborValue::Unsigned(value)) => Ok(*value),
                _ => Err(ExecutionError::ActionUndecodable(format!(
                    "field {name:?} is missing or not an unsigned integer"
                ))),
            }
        };
        let digest = |name: &str| -> Result<Sha256Digest, ExecutionError> {
            match field(name) {
                Some(CborValue::Bytes(bytes)) if bytes.len() == 32 => {
                    let mut array = [0u8; 32];
                    array.copy_from_slice(bytes);
                    Ok(Sha256Digest::from_bytes(array))
                }
                _ => Err(ExecutionError::ActionUndecodable(format!(
                    "field {name:?} is missing or not a 32-byte digest"
                ))),
            }
        };

        match text("action")?.as_str() {
            "enter-loader" => Ok(StoredAction::ManagedControl {
                control_action: text("controlAction")?,
                expect: Vec::new(),
            }),
            "verify-hdc-postflight" => Ok(StoredAction::ManagedControl {
                control_action: text("controlAction")?,
                expect: match field("expect") {
                    Some(CborValue::Map(pairs)) => pairs
                        .iter()
                        .map(|(key, value)| match (key, value) {
                            (CborValue::Text(key), CborValue::Text(value)) => {
                                Ok((key.clone(), value.clone()))
                            }
                            _ => Err(ExecutionError::ActionUndecodable(
                                "expect holds a non-text entry".into(),
                            )),
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                    _ => {
                        return Err(ExecutionError::ActionUndecodable(
                            "verify-hdc-postflight carries no expect map".into(),
                        ))
                    }
                },
            }),
            "probe-loader" => Ok(StoredAction::ProbeLoader),
            "validate-partition-table" => Ok(StoredAction::ValidatePartitionTable {
                expected_layout_digest: digest("expectedLayoutDigest")?,
            }),
            "write-partition" => Ok(StoredAction::WritePartition {
                partition: text("partition")?,
                member: text("member")?,
                begin_sector: unsigned("beginSector")?,
            }),
            "readback-partition" => Ok(StoredAction::ReadbackPartition {
                partition: text("partition")?,
                begin_sector: unsigned("beginSector")?,
                max_strength_when_readable: parse_strength(&text("maxStrengthWhenReadable")?)?,
                erased_medium_filler: match field("erasedMediumFiller") {
                    Some(CborValue::Unsigned(byte)) if *byte <= 0xFF => Some(*byte as u8),
                    Some(CborValue::Null) => None,
                    _ => {
                        return Err(ExecutionError::ActionUndecodable(
                            "erasedMediumFiller is neither a byte nor null".into(),
                        ))
                    }
                },
            }),
            "characterize-read-domain" => Ok(StoredAction::CharacterizeReadDomain),
            "reset-device" => Ok(StoredAction::ResetDevice),
            other => Err(ExecutionError::ActionUndecodable(format!(
                "unknown stored action {other:?}"
            ))),
        }
    }
}

fn parse_strength(text: &str) -> Result<VerificationStrength, ExecutionError> {
    match text {
        "fullHash" => Ok(VerificationStrength::FullHash),
        "sampledRanges" => Ok(VerificationStrength::SampledRanges),
        "prefixHash" => Ok(VerificationStrength::PrefixHash),
        "semanticOnly" => Ok(VerificationStrength::SemanticOnly),
        other => Err(ExecutionError::ActionUndecodable(format!(
            "unknown verification strength {other:?}"
        ))),
    }
}

/// What executing one action produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionOutcome {
    pub action_id: ActionId,
    pub step_id: StepId,
    pub disposition: ActionDisposition,
    pub facts: Vec<(OpaqueId, String)>,
    pub evidence_digest: Sha256Digest,
    /// Present for verification actions only.
    pub verification: Option<VerificationOutcome>,
}

/// Carries what earlier actions established into the ones that need it.
///
/// A write may not be dispatched until this holds an observed partition table,
/// and a readback's verdict depends on the measured read domain. Keeping both
/// here, rather than re-deriving them per action, is what stops a write from
/// running against a table nobody looked at.
#[derive(Debug, Default)]
pub struct ExecutionSession {
    observed_table: Option<PartitionTableFact>,
    read_domain: Option<MeasuredReadDomain>,
    /// Sectors that returned real (non-uniform) data before any write. A
    /// partition start in here is inside the read face, so filler read there
    /// afterwards is a real failure rather than an unreadable address.
    demonstrated_readable: Vec<u64>,
    staged: BTreeMap<String, StagedImage>,
}

impl ExecutionSession {
    pub fn new(staged: BTreeMap<String, StagedImage>) -> Self {
        ExecutionSession {
            observed_table: None,
            read_domain: None,
            demonstrated_readable: Vec::new(),
            staged,
        }
    }

    pub fn observed_table(&self) -> Option<&PartitionTableFact> {
        self.observed_table.as_ref()
    }

    pub fn read_domain(&self) -> Option<&MeasuredReadDomain> {
        self.read_domain.as_ref()
    }

    pub fn note_readable_sector(&mut self, sector: u64) {
        if !self.demonstrated_readable.contains(&sector) {
            self.demonstrated_readable.push(sector);
        }
    }

    pub fn set_read_domain(&mut self, domain: MeasuredReadDomain) {
        self.read_domain = Some(domain);
    }

    pub fn set_observed_table(&mut self, table: PartitionTableFact) {
        self.observed_table = Some(table);
    }
}

/// Executes one decoded action.
///
/// `profile` is consulted for the target allowlist; the Provider does not hold
/// its own (architecture.md 10.4).
pub fn execute_action(
    action: &StoredAction,
    record: &PrivateActionRecord,
    session: &mut ExecutionSession,
    profile: &DeviceProfile,
    port: &dyn FixedToolPort,
    scratch: &Path,
) -> Result<ActionOutcome, ExecutionError> {
    match action {
        StoredAction::ManagedControl {
            control_action, ..
        } => Err(ExecutionError::RequiresAuthority {
            control_action: control_action.clone(),
        }),
        StoredAction::ProbeLoader => {
            let receipt = run(port, &RockUsbCommand::ListDevices, 64 * 1024)?;
            Ok(outcome(
                record,
                ActionDisposition::SemanticSuccess,
                vec![fact("ld", receipt.stdout.trim())],
                receipt.evidence_digest(),
                None,
            ))
        }
        StoredAction::ValidatePartitionTable {
            expected_layout_digest,
        } => {
            // Two different checks, against two different things. The digest
            // says the plan still agrees with the Profile it was built from;
            // conformance says the device agrees with the Profile. An earlier
            // revision compared the digest against the *device's* table, which
            // could never match: the Profile names nine writable targets and
            // the device's table declares fifteen partitions with no sizes at
            // all (AD-018).
            let profile_digest = profile_layout_digest(profile);
            if profile_digest != *expected_layout_digest {
                return Err(ExecutionError::LayoutMismatch {
                    expected: *expected_layout_digest,
                    observed: profile_digest,
                });
            }

            let receipt = run(port, &RockUsbCommand::PrintPartitionTable, 256 * 1024)?;
            let observed = parse_ppt(&receipt.stdout)?;
            check_conformance(&observed, profile)?;
            let observed_digest = layout_digest_of(&observed);
            session.set_observed_table(observed);
            Ok(outcome(
                record,
                ActionDisposition::SemanticSuccess,
                vec![
                    fact("observedLayoutDigest", observed_digest.to_string()),
                    fact("planLayoutDigest", profile_digest.to_string()),
                ],
                receipt.evidence_digest(),
                None,
            ))
        }
        StoredAction::CharacterizeReadDomain => characterize_read_domain(record, session, port, scratch),
        StoredAction::WritePartition {
            partition,
            member,
            begin_sector,
        } => write_partition(
            record, session, profile, port, partition, member, *begin_sector,
        ),
        StoredAction::ReadbackPartition {
            partition,
            begin_sector,
            max_strength_when_readable,
            erased_medium_filler,
        } => readback_partition(
            record,
            session,
            port,
            scratch,
            partition,
            *begin_sector,
            *max_strength_when_readable,
            *erased_medium_filler,
        ),
        StoredAction::ResetDevice => {
            let receipt = run(port, &RockUsbCommand::ResetDevice, 64 * 1024)?;
            let disposition = if receipt.combined_output().contains(RESET_SUCCESS_MARKER) {
                ActionDisposition::SemanticSuccess
            } else {
                // A reset whose marker never appeared may still have reset the
                // device. Unknown, not failed.
                ActionDisposition::OutcomeUnknown
            };
            Ok(outcome(
                record,
                disposition,
                vec![fact("marker", RESET_SUCCESS_MARKER)],
                receipt.evidence_digest(),
                None,
            ))
        }
    }
}

/// Probes the near and far ends of the medium.
///
/// The primary table sits at LBA 1 and must read as real data — without it the
/// name-addressed write has nothing to resolve against. The backup table sits
/// at the far end; if that reads as uniform filler while the primary did not,
/// the read face is windowed and every filler read past it is uninformative.
fn characterize_read_domain(
    record: &PrivateActionRecord,
    session: &mut ExecutionSession,
    port: &dyn FixedToolPort,
    scratch: &Path,
) -> Result<ActionOutcome, ExecutionError> {
    let primary = read_sectors(port, scratch, 1, 1, "primary-table")?;
    if uniform_byte(&primary).is_some() {
        return Err(ExecutionError::NoTableAtLba1);
    }
    session.note_readable_sector(1);

    let table = session
        .observed_table()
        .ok_or(ExecutionError::TableNotObservedYet)?;
    // The far end the device's own table declares. `None` size is the grow
    // marker: the last declared partition runs to the end of the medium, so
    // its start is the furthest address the table names.
    let far_sector = table
        .entries
        .iter()
        .map(|entry| match entry.size_sectors {
            Some(size) => entry.offset_sectors + size.saturating_sub(1),
            None => entry.offset_sectors,
        })
        .max()
        .ok_or(ExecutionError::TableNotObservedYet)?;

    let far = read_sectors(port, scratch, far_sector, 1, "far-end")?;
    let domain = match uniform_byte(&far) {
        Some(byte) => MeasuredReadDomain::Windowed {
            detail: format!(
                "sector 1 read real data; sector {far_sector} read uniform 0x{byte:02X}"
            ),
        },
        None => {
            session.note_readable_sector(far_sector);
            MeasuredReadDomain::Full
        }
    };
    let summary = domain.summary();
    let detail = match &domain {
        MeasuredReadDomain::Windowed { detail } => detail.clone(),
        MeasuredReadDomain::Full => format!("sectors 1 and {far_sector} both read real data"),
    };
    session.set_read_domain(domain);

    Ok(outcome(
        record,
        ActionDisposition::SemanticSuccess,
        vec![
            fact("addressableMedium", summary),
            fact("readDomainDetail", detail.clone()),
        ],
        sha256(detail.as_bytes()),
        None,
    ))
}

#[allow(clippy::too_many_arguments)]
fn write_partition(
    record: &PrivateActionRecord,
    session: &mut ExecutionSession,
    profile: &DeviceProfile,
    port: &dyn FixedToolPort,
    partition: &str,
    member: &str,
    begin_sector: u64,
) -> Result<ActionOutcome, ExecutionError> {
    // 1. The Profile's allowlist, not the Provider's opinion.
    let target = profile
        .allowed_targets
        .iter()
        .find(|target| target.partition.as_str() == partition)
        .ok_or_else(|| ExecutionError::TargetNotAllowed(partition.to_string()))?;
    if target.offset_sectors != begin_sector {
        return Err(ExecutionError::TargetOffsetDisagrees {
            partition: partition.to_string(),
            profile: target.offset_sectors,
            plan: begin_sector,
        });
    }

    // 2. The device's own table has to have been read, and has to agree.
    let table = session
        .observed_table()
        .ok_or(ExecutionError::TableNotObservedYet)?;
    let entry = table
        .entries
        .iter()
        .find(|entry| entry.name == partition)
        .ok_or_else(|| ExecutionError::PartitionNotOnDevice(partition.to_string()))?;
    if entry.offset_sectors != begin_sector {
        return Err(ExecutionError::TargetOffsetDisagrees {
            partition: partition.to_string(),
            profile: begin_sector,
            plan: entry.offset_sectors,
        });
    }

    // 3. The staged image is still the image that was staged.
    let image = session
        .staged
        .get(member)
        .ok_or_else(|| ExecutionError::ImageNotStaged(member.to_string()))?
        .clone();
    image.revalidate()?;

    // 4. It has to fit inside the span the device's table gives it. `wlx`
    //    resolves the address itself, so this is a refusal, not an addressing
    //    step: a write that would cross into the next partition is refused
    //    before the tool is spawned.
    let image_sectors = ceil_div(image.size_bytes, TOOL_SECTOR_BYTES);
    if let Some(size_sectors) = entry.size_sectors {
        if image_sectors > size_sectors {
            return Err(ExecutionError::ImageOverrunsPartition {
                partition: partition.to_string(),
                image_sectors,
                partition_sectors: size_sectors,
            });
        }
    }

    // 5. Only now, the write. From the moment the child is spawned the device
    //    may have changed, so nothing after this point may report "no effect".
    let receipt = run(
        port,
        &RockUsbCommand::WriteByName {
            partition: partition.to_string(),
            image: image.path.clone(),
        },
        1 << 20,
    )?;
    let output = receipt.combined_output();
    let disposition = if output.contains(WRITE_SUCCESS_MARKER) {
        ActionDisposition::SemanticSuccess
    } else {
        // The tool was spawned. Whether it wrote anything is not knowable from
        // a missing marker (architecture.md 14.1).
        ActionDisposition::OutcomeUnknown
    };

    Ok(outcome(
        record,
        disposition,
        vec![
            fact("partition", partition),
            fact("member", member),
            fact("imageSha256", image.sha256.to_string()),
            fact("imageBytes", image.size_bytes.to_string()),
            fact("beginSector", begin_sector.to_string()),
        ],
        receipt.evidence_digest(),
        None,
    ))
}

#[allow(clippy::too_many_arguments)]
fn readback_partition(
    record: &PrivateActionRecord,
    session: &mut ExecutionSession,
    port: &dyn FixedToolPort,
    scratch: &Path,
    partition: &str,
    begin_sector: u64,
    max_strength: VerificationStrength,
    filler: Option<u8>,
) -> Result<ActionOutcome, ExecutionError> {
    let range = record
        .declared_range
        .ok_or(ExecutionError::VerificationRangeMissing)?;
    let expected = record
        .content_digest
        .ok_or(ExecutionError::VerificationRangeMissing)?;
    let domain = session
        .read_domain()
        .cloned()
        .ok_or(ExecutionError::ReadDomainNotCharacterized)?;

    let sectors = ceil_div(range.length, TOOL_SECTOR_BYTES);
    let bytes = read_sectors(port, scratch, begin_sector, sectors, partition)?;
    let read = &bytes[..(range.length as usize).min(bytes.len())];

    let verification = classify_readback(
        read,
        range,
        expected,
        max_strength,
        filler,
        &domain,
        session.demonstrated_readable.contains(&begin_sector),
    );

    // A verification action is read-only: it never had an effect, so its
    // disposition is about the observation, not the medium. A failed
    // verification is a real, confirmed observation.
    let outcome_facts = vec![
        fact("partition", partition),
        fact("addressableMedium", domain.summary()),
        fact("outcome", verification.as_str()),
    ];
    Ok(outcome(
        record,
        ActionDisposition::SemanticSuccess,
        outcome_facts,
        sha256(read),
        Some(verification),
    ))
}

/// The three-state verdict of architecture.md 16.4.
///
/// The ordering matters and is the whole lesson of AD-006: uniform filler is
/// classified *before* any hash comparison, and outside a windowed read face it
/// is a skip rather than a failure. Every "the write did not land" verdict of
/// 2026-08-04 came from hashing filler that the read face had invented.
fn classify_readback(
    read: &[u8],
    range: ByteRange,
    expected: Sha256Digest,
    max_strength: VerificationStrength,
    filler: Option<u8>,
    domain: &MeasuredReadDomain,
    start_demonstrated_readable: bool,
) -> VerificationOutcome {
    if let Some(byte) = uniform_byte(read) {
        let is_filler = filler.map(|declared| declared == byte).unwrap_or(true);
        if is_filler {
            return match domain {
                MeasuredReadDomain::Windowed { detail } => VerificationOutcome::TypedSkip {
                    range,
                    reason: TypedSkipReason::OutsideReadDomain,
                    detail: format!(
                        "read 0x{byte:02X} throughout; the read face is windowed ({detail}), so \
                         filler here carries no information about the medium"
                    ),
                },
                MeasuredReadDomain::Full if start_demonstrated_readable => {
                    VerificationOutcome::Failed {
                        range,
                        classification: FailureClassification::ErasedMediumFiller,
                    }
                }
                MeasuredReadDomain::Full => VerificationOutcome::Failed {
                    range,
                    classification: FailureClassification::ErasedMediumFiller,
                },
            };
        }
    }

    if read.len() as u64 != range.length {
        return VerificationOutcome::TypedSkip {
            range,
            reason: TypedSkipReason::OutsideReadDomain,
            detail: format!(
                "the read face returned {} of {} declared bytes",
                read.len(),
                range.length
            ),
        };
    }

    if sha256(read) == expected {
        VerificationOutcome::Verified {
            strength: max_strength,
            range,
        }
    } else {
        VerificationOutcome::Failed {
            range,
            classification: FailureClassification::ContentMismatch,
        }
    }
}

/// The single byte a buffer consists of, if it consists of one.
fn uniform_byte(bytes: &[u8]) -> Option<u8> {
    let first = *bytes.first()?;
    bytes.iter().all(|byte| *byte == first).then_some(first)
}

fn read_sectors(
    port: &dyn FixedToolPort,
    scratch: &Path,
    begin_sector: u64,
    sectors: u64,
    label: &str,
) -> Result<Vec<u8>, ExecutionError> {
    let out = scratch.join(format!("read-{label}-{begin_sector}-{sectors}.bin"));
    let receipt = run(
        port,
        &RockUsbCommand::ReadSectors {
            begin_sector,
            sectors,
            out: out.clone(),
        },
        64 * 1024,
    )?;
    if !receipt.exited_zero {
        return Err(ExecutionError::ReadFailed {
            begin_sector,
            sectors,
            output: receipt.combined_output(),
        });
    }
    let bytes = std::fs::read(&out)
        .map_err(|error| ExecutionError::ScratchUnusable(format!("{}: {error}", out.display())))?;
    let _ = std::fs::remove_file(&out);
    Ok(bytes)
}

fn run(
    port: &dyn FixedToolPort,
    command: &RockUsbCommand,
    budget: usize,
) -> Result<ToolReceipt, ExecutionError> {
    port.run(&ToolInvocation {
        argv: command.argv(),
        stdout_budget: budget,
        interruptible: command.is_interruptible(),
    })
    .map_err(|message| ExecutionError::ToolPort {
        argv: command.argv().join(" "),
        message,
    })
}

fn outcome(
    record: &PrivateActionRecord,
    disposition: ActionDisposition,
    facts: Vec<(OpaqueId, String)>,
    evidence_digest: Sha256Digest,
    verification: Option<VerificationOutcome>,
) -> ActionOutcome {
    ActionOutcome {
        action_id: record.action_id.clone(),
        step_id: record.step_id.clone(),
        disposition,
        facts,
        evidence_digest,
        verification,
    }
}

fn fact(key: &str, value: impl Into<String>) -> (OpaqueId, String) {
    (
        OpaqueId::new(key).expect("literal fact key"),
        value.into(),
    )
}

/// Parses the tool's own partition table listing.
///
/// Measured on a DAYU200 in Loader mode, 2026-08-15 (AD-018). The real format
/// is three columns, CRLF-terminated, and the LBA carries no `0x`:
///
/// ```text
/// **********Partition Info(GPT)**********
/// NO  LBA       Name
/// 00  00002000  uboot
/// 14  01308000  userdata
/// ```
///
/// There is **no size column**. The device's table declares where each
/// partition starts and nothing else, so `size_sectors` here is the distance to
/// the next partition's start — an upper bound on what can be written without
/// crossing into it, not a size the device declared. On this board the two
/// differ: the archive gives `chip_ckm` 131072 sectors, and the next partition
/// does not begin for 13017088, so the space between them is unallocated. The
/// bound is what a refusal needs; the declared size belongs to the artifact.
/// The last entry has no next, so its bound is `None` — the same thing the
/// archive says with `-@...(userdata:grow)`.
fn parse_ppt(stdout: &str) -> Result<PartitionTableFact, ExecutionError> {
    use arkforge_artifact::manifest::{GrammarBranch, PartitionEntryFact};

    let mut rows: Vec<(u32, u64, String)> = Vec::new();
    for line in stdout.lines() {
        let line = line.trim_end_matches(['\r', '\n']);
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() != 3 {
            continue;
        }
        // The header row is `NO LBA Name`, which has three fields too. An index
        // that is not a number is what separates it from a partition row.
        let (Ok(index), Some(offset)) = (fields[0].parse::<u32>(), parse_hex(fields[1])) else {
            continue;
        };
        rows.push((index, offset, fields[2].to_string()));
    }
    if rows.is_empty() {
        return Err(ExecutionError::PartitionTableUnreadable(
            stdout.chars().take(200).collect(),
        ));
    }

    let mut entries = Vec::with_capacity(rows.len());
    for (position, (index, offset, name)) in rows.iter().enumerate() {
        let size_sectors = rows
            .get(position + 1)
            .map(|(_, next_offset, _)| next_offset.saturating_sub(*offset));
        entries.push(PartitionEntryFact {
            index: *index,
            name: name.clone(),
            offset_sectors: *offset,
            size_sectors,
            attribute: None,
            grammar_branch: if size_sectors.is_some() {
                GrammarBranch::Fixed
            } else {
                GrammarBranch::RemainderGrow
            },
        });
    }
    Ok(PartitionTableFact {
        device: DEVICE_STRING.to_string(),
        logical_block_size: TOOL_SECTOR_BYTES as u32,
        entries,
    })
}

/// The tool prints LBAs as bare uppercase hex (`0003C000`), with no `0x`. The
/// prefix is accepted too, so the same function reads a table written either
/// way, but its absence is the normal case.
fn parse_hex(text: &str) -> Option<u64> {
    let body = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .unwrap_or(text);
    if body.is_empty() || !body.chars().all(|character| character.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(body, 16).ok()
}

/// Every Profile target must exist on the device where the Profile says, and
/// every partition the device declares must be one the Profile knows.
///
/// This is the three-way agreement architecture.md 16.3 requires: an image in
/// the artifact does not license a write, and neither does a partition on the
/// device. Both have to be named by the Profile.
fn check_conformance(
    observed: &PartitionTableFact,
    profile: &DeviceProfile,
) -> Result<(), ExecutionError> {
    for target in &profile.allowed_targets {
        let entry = observed
            .entries
            .iter()
            .find(|entry| entry.name == target.partition.as_str())
            .ok_or_else(|| ExecutionError::PartitionNotOnDevice(target.partition.to_string()))?;
        if entry.offset_sectors != target.offset_sectors {
            return Err(ExecutionError::TargetOffsetDisagrees {
                partition: target.partition.to_string(),
                profile: target.offset_sectors,
                plan: entry.offset_sectors,
            });
        }
    }

    let mut unknown: Vec<String> = Vec::new();
    for entry in &observed.entries {
        let known = profile
            .allowed_targets
            .iter()
            .any(|target| target.partition.as_str() == entry.name)
            || profile
                .protected_targets
                .iter()
                .any(|protected| protected.as_str() == entry.name);
        if !known {
            unknown.push(entry.name.clone());
        }
    }
    if !unknown.is_empty() {
        return Err(ExecutionError::DeviceDeclaresUnknownPartitions(unknown));
    }
    Ok(())
}

/// The Profile's own declared layout, hashed the way the Provider hashed it
/// when it built the plan. Recomputed here so a plan that drifted from the
/// Profile it names is caught before the device is touched.
fn profile_layout_digest(profile: &DeviceProfile) -> Sha256Digest {
    use arkforge_core::digest::{digest_in_domain, Domain};
    let mut ordered: Vec<_> = profile.allowed_targets.iter().collect();
    ordered.sort_by_key(|target| target.write_order);
    let value = CborValue::array(
        ordered
            .iter()
            .map(|target| {
                CborValue::map(vec![
                    ("partition", CborValue::text(target.partition.as_str())),
                    ("offsetSectors", CborValue::Unsigned(target.offset_sectors)),
                ])
            })
            .collect(),
    );
    let bytes = value
        .to_canonical_bytes()
        .expect("layout values are canonical");
    digest_in_domain(Domain::DeviceProfile, &bytes)
}

/// The observed layout reduced to name + first sector, in table order.
///
/// Deliberately not the archive's grammar branch or attributes: the device's
/// table does not carry them, and a digest over fields only one side has would
/// never match.
fn layout_digest_of(table: &PartitionTableFact) -> Sha256Digest {
    let mut hasher = Sha256::new();
    for entry in &table.entries {
        hasher.update(entry.name.as_bytes());
        hasher.update(b"@");
        hasher.update(entry.offset_sectors.to_string().as_bytes());
        hasher.update(b"\n");
    }
    hasher.finalize()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionError {
    /// The action belongs to the authority's control port.
    RequiresAuthority { control_action: String },
    ActionUndecodable(String),
    ToolPort { argv: String, message: String },
    LayoutMismatch {
        expected: Sha256Digest,
        observed: Sha256Digest,
    },
    PartitionTableUnreadable(String),
    DeviceDeclaresUnknownPartitions(Vec<String>),
    TableNotObservedYet,
    ReadDomainNotCharacterized,
    NoTableAtLba1,
    TargetNotAllowed(String),
    PartitionNotOnDevice(String),
    TargetOffsetDisagrees {
        partition: String,
        profile: u64,
        plan: u64,
    },
    ImageNotStaged(String),
    ImageOverrunsPartition {
        partition: String,
        image_sectors: u64,
        partition_sectors: u64,
    },
    StagingChanged(String),
    VerificationRangeMissing,
    ReadFailed {
        begin_sector: u64,
        sectors: u64,
        output: String,
    },
    ScratchUnusable(String),
}

impl fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecutionError::RequiresAuthority { control_action } => write!(
                f,
                "{control_action} is performed by the authority's control port, not by this \
                 Provider (architecture.md 9.2)"
            ),
            ExecutionError::ActionUndecodable(detail) => {
                write!(f, "stored action cannot be decoded: {detail}")
            }
            ExecutionError::ToolPort { argv, message } => {
                write!(f, "running {argv:?}: {message}")
            }
            ExecutionError::LayoutMismatch { expected, observed } => write!(
                f,
                "the device's partition table hashes to {observed}, the plan assumed {expected}"
            ),
            ExecutionError::PartitionTableUnreadable(head) => {
                write!(f, "the partition table listing could not be read: {head:?}")
            }
            ExecutionError::DeviceDeclaresUnknownPartitions(names) => write!(
                f,
                "the device declares partitions the profile does not know: {}",
                names.join(", ")
            ),
            ExecutionError::TableNotObservedYet => f.write_str(
                "the device's own partition table has not been observed; no write may be \
                 dispatched against an unread table",
            ),
            ExecutionError::ReadDomainNotCharacterized => f.write_str(
                "the medium's read face has not been characterized; a readback verdict without \
                 it cannot tell filler from an unwritten partition (architecture.md 16.4)",
            ),
            ExecutionError::NoTableAtLba1 => f.write_str(
                "LBA 1 carries no table; the name-addressed write has nothing to resolve against",
            ),
            ExecutionError::TargetNotAllowed(partition) => write!(
                f,
                "the profile does not allow writing {partition}"
            ),
            ExecutionError::PartitionNotOnDevice(partition) => write!(
                f,
                "the device's own table declares no partition named {partition}"
            ),
            ExecutionError::TargetOffsetDisagrees {
                partition,
                profile,
                plan,
            } => write!(
                f,
                "{partition} starts at sector {profile} in one source and {plan} in another; \
                 refusing the write"
            ),
            ExecutionError::ImageNotStaged(member) => {
                write!(f, "{member} was not staged for this job")
            }
            ExecutionError::ImageOverrunsPartition {
                partition,
                image_sectors,
                partition_sectors,
            } => write!(
                f,
                "the image for {partition} needs {image_sectors} sectors and the device gives it \
                 {partition_sectors}; refusing the write"
            ),
            ExecutionError::StagingChanged(detail) => {
                write!(f, "a staged image changed before it was written: {detail}")
            }
            ExecutionError::VerificationRangeMissing => {
                f.write_str("a verification action carries no declared range or content digest")
            }
            ExecutionError::ReadFailed {
                begin_sector,
                sectors,
                output,
            } => write!(
                f,
                "reading {sectors} sector(s) at {begin_sector} failed: {output}"
            ),
            ExecutionError::ScratchUnusable(detail) => write!(f, "scratch space: {detail}"),
        }
    }
}

impl std::error::Error for ExecutionError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const PROFILE_SOURCE: &str = include_str!("../../../profiles/dayu200.yaml");

    fn profile() -> DeviceProfile {
        arkforge_core::profile::load(PROFILE_SOURCE).expect("the shipped profile parses")
    }

    #[test]
    fn the_command_surface_lowers_to_the_argv_the_evidence_was_produced_with() {
        assert_eq!(RockUsbCommand::ListDevices.argv(), vec!["ld"]);
        assert_eq!(RockUsbCommand::PrintPartitionTable.argv(), vec!["ppt"]);
        assert_eq!(RockUsbCommand::ResetDevice.argv(), vec!["rd"]);
        assert_eq!(
            RockUsbCommand::WriteByName {
                partition: "uboot".into(),
                image: PathBuf::from("/staging/uboot.img"),
            }
            .argv(),
            vec!["wlx", "uboot", "/staging/uboot.img"]
        );
        assert_eq!(
            RockUsbCommand::ReadSectors {
                begin_sector: 8192,
                sectors: 4,
                out: PathBuf::from("/scratch/read.bin"),
            }
            .argv(),
            vec!["rl", "8192", "4", "/scratch/read.bin"]
        );
    }

    #[test]
    fn only_a_write_is_non_interruptible() {
        for command in [
            RockUsbCommand::ListDevices,
            RockUsbCommand::PrintPartitionTable,
            RockUsbCommand::ResetDevice,
            RockUsbCommand::ReadSectors {
                begin_sector: 0,
                sectors: 1,
                out: PathBuf::from("/tmp/x"),
            },
        ] {
            assert!(command.is_interruptible(), "{command:?}");
        }
        assert!(!RockUsbCommand::WriteByName {
            partition: "system".into(),
            image: PathBuf::from("/x"),
        }
        .is_interruptible());
    }

    /// Byte-for-byte the listing a DAYU200 in Loader mode printed on
    /// 2026-08-15, CRLF and all (AD-018). The earlier fixture here was written
    /// from documentation and had a size column the tool does not print; it
    /// passed, and the real device did not.
    const REAL_PPT: &str = concat!(
        "**********Partition Info(GPT)**********\r\n",
        "NO  LBA       Name                \r\n",
        "00  00002000  uboot\r\n",
        "01  00004000  misc\r\n",
        "02  00006000  bootctrl\r\n",
        "03  00007000  resource\r\n",
        "04  0000A000  boot_linux\r\n",
        "05  0003A000  ramdisk\r\n",
        "06  0003C000  system\r\n",
        "07  0043C000  vendor\r\n",
        "08  0063C000  sys-prod\r\n",
        "09  00655000  chip-prod\r\n",
        "10  0066E000  updater\r\n",
        "11  0067E000  eng_system\r\n",
        "12  00686000  eng_chipset\r\n",
        "13  0069E000  chip_ckm\r\n",
        "14  01308000  userdata\r\n",
    );

    #[test]
    fn the_devices_own_table_parses_into_the_same_units_the_archive_declares() {
        let table = parse_ppt(REAL_PPT).unwrap();
        assert_eq!(table.entries.len(), 15);
        assert_eq!(table.entries[0].name, "uboot");
        assert_eq!(table.entries[0].offset_sectors, 8192);
        assert_eq!(table.entries[6].name, "system");
        assert_eq!(table.entries[6].offset_sectors, 245_760);
        // The tool prints no sizes. A span is derived from the next start,
        // and the last entry has none.
        assert_eq!(table.entries[6].size_sectors, Some(4_194_304));
        assert_eq!(table.entries[14].name, "userdata");
        assert_eq!(table.entries[14].offset_sectors, 19_955_712);
        assert_eq!(table.entries[14].size_sectors, None);
    }

    /// The device's fifteen rows and the archive's fifteen rows are the same
    /// layout, which is the whole premise of a name-addressed write.
    #[test]
    fn the_device_and_the_archive_agree_on_every_partition_start() {
        let device = parse_ppt(REAL_PPT).unwrap();
        let profile = profile();
        for target in &profile.allowed_targets {
            let entry = device
                .entries
                .iter()
                .find(|entry| entry.name == target.partition.as_str())
                .unwrap_or_else(|| panic!("device has no {}", target.partition));
            assert_eq!(
                entry.offset_sectors,
                target.offset_sectors,
                "{} start",
                target.partition
            );
        }
        check_conformance(&device, &profile).unwrap();
    }

    #[test]
    fn a_device_partition_the_profile_never_named_is_refused() {
        let mut device = parse_ppt(REAL_PPT).unwrap();
        device.entries.push(arkforge_artifact::manifest::PartitionEntryFact {
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

    #[test]
    fn a_listing_with_no_rows_is_unreadable_rather_than_an_empty_table() {
        assert!(matches!(
            parse_ppt("rkdeveloptool: no device\n"),
            Err(ExecutionError::PartitionTableUnreadable(_))
        ));
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

    #[derive(Debug, Default)]
    struct RecordingPort {
        invocations: RefCell<Vec<Vec<String>>>,
    }

    impl FixedToolPort for RecordingPort {
        fn run(&self, invocation: &ToolInvocation) -> Result<ToolReceipt, String> {
            self.invocations
                .borrow_mut()
                .push(invocation.argv.clone());
            Err("this port never runs anything".into())
        }
    }

    /// A write whose partition the Profile does not allow must never reach the
    /// port. The recording port proves "never reached", not "refused after".
    #[test]
    fn a_write_to_a_partition_the_profile_protects_never_reaches_the_tool() {
        let profile = profile();
        let port = RecordingPort::default();
        let mut session = ExecutionSession::new(BTreeMap::new());
        session.set_observed_table(PartitionTableFact {
            device: DEVICE_STRING.into(),
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
            port.invocations.borrow().is_empty(),
            "the tool was spawned for a protected partition"
        );
    }

    #[test]
    fn a_write_before_the_table_was_read_never_reaches_the_tool() {
        let profile = profile();
        let port = RecordingPort::default();
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
        assert!(port.invocations.borrow().is_empty());
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
}
