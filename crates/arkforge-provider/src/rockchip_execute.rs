//! Native RockUSB execution for the DAYU200 Loader path.
//!
//! Stored actions carry typed semantics only. The executor validates the
//! Profile, observed partition table, staged artifact and read domain before
//! calling the native [`RockUsbPort`]. No caller-controlled subprocess or argv
//! surface exists; macOS file validation uses one fixed system SHA-256 command
//! fed only an already-open descriptor.

use arkforge_artifact::manifest::PartitionTableFact;
use arkforge_core::Sha256Digest;
use arkforge_core::digest::{CborValue, Sha256, sha256};
use arkforge_core::effect::ByteRange;
use arkforge_core::ids::{ActionId, OpaqueId, StepId};
use arkforge_core::outcome::ActionDisposition;
use arkforge_core::profile::DeviceProfile;
use arkforge_core::projection::PrivateActionRecord;
use arkforge_core::verification::{
    FailureClassification, TypedSkipReason, VerificationOutcome, VerificationStrength,
};
use core::fmt;
use std::collections::BTreeMap;
use std::fs::File;
#[cfg(not(target_os = "macos"))]
use std::io::Read;
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};
use std::thread::JoinHandle;
use std::time::Instant;

/// `a / b` rounded up. `u64::div_ceil` is unstable on this toolchain.
fn ceil_div(numerator: u64, denominator: u64) -> u64 {
    numerator / denominator + u64::from(!numerator.is_multiple_of(denominator))
}

/// Logical sector size used by the native RockUSB protocol.
pub const ROCKUSB_SECTOR_BYTES: u64 = 512;

/// One device returned by the typed RockUSB port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RockUsbDevice {
    pub vendor_id: u16,
    pub product_id: u16,
    pub usb_specification: Option<u16>,
    pub location: RockUsbLocation,
    pub mode: String,
    pub serial: Option<String>,
    pub product_name: Option<String>,
    pub vendor_name: Option<String>,
    pub device_release: Option<u16>,
}

/// Native IOKit controller topology fact. It describes attachment and is not a
/// stable device identity by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RockUsbLocation {
    IokitTopology(u32),
}

impl RockUsbDevice {
    pub fn summary(&self) -> String {
        let RockUsbLocation::IokitTopology(value) = self.location;
        let location = format!("iokit={value:08x}");
        format!(
            "vid={:04x} pid={:04x} {location} mode={}",
            self.vendor_id, self.product_id, self.mode
        )
    }
}

/// A typed observation and the bytes/facts that evidence it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RockUsbObservation<T> {
    pub value: T,
    pub evidence_digest: Sha256Digest,
}

/// A mutation answer. `semantic_success == false` is deliberately not a
/// failure return: the transport was reached, so the caller must record an
/// unknown outcome with the attached diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RockUsbMutationReceipt {
    pub semantic_success: bool,
    pub evidence_digest: Sha256Digest,
    pub duration_ms: u64,
    pub detail: String,
    pub progress: Option<RockUsbWriteProgress>,
}

/// Native write progress derived from successful WRITE_LBA CSWs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RockUsbWriteProgress {
    pub payload_bytes: u64,
    pub wire_sectors: u64,
    pub chunks: u64,
    pub chunk_sectors: u16,
    pub payload_digest: Sha256Digest,
}

/// Whether a port refusal happened before an external interaction or after
/// the selected backend may have reached the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RockUsbPortFailure {
    BeforeIo(String),
    AfterIo(String),
}

impl fmt::Display for RockUsbPortFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BeforeIo(detail) => write!(f, "before I/O: {detail}"),
            Self::AfterIo(detail) => write!(f, "after I/O began: {detail}"),
        }
    }
}

/// Closed, typed RockUSB semantics. The Provider and native implementation
/// exchange only these values.
pub trait RockUsbPort: fmt::Debug {
    fn discover(&self) -> Result<RockUsbObservation<Vec<RockUsbDevice>>, RockUsbPortFailure>;

    fn capacity_sectors(&self) -> Result<RockUsbObservation<u64>, RockUsbPortFailure> {
        Err(RockUsbPortFailure::BeforeIo(
            "this RockUSB port does not implement READ_FLASH_INFO".into(),
        ))
    }

    fn read_partition_table(
        &self,
    ) -> Result<RockUsbObservation<PartitionTableFact>, RockUsbPortFailure>;

    fn read_sectors(
        &self,
        begin_sector: u64,
        sectors: u64,
        scratch: &Path,
    ) -> Result<RockUsbObservation<Vec<u8>>, RockUsbPortFailure>;

    fn write_partition(
        &self,
        _partition: &str,
        _begin_sector: u64,
        _image: &mut ValidatedImage,
    ) -> Result<RockUsbMutationReceipt, RockUsbPortFailure> {
        Err(RockUsbPortFailure::BeforeIo(
            "this RockUSB port does not implement WRITE_LBA".into(),
        ))
    }

    fn reset_device(&self) -> Result<RockUsbMutationReceipt, RockUsbPortFailure> {
        Err(RockUsbPortFailure::BeforeIo(
            "this RockUSB port does not implement DEVICE_RESET".into(),
        ))
    }
}

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
    /// This simple entry point is used by callers that need an immediate check.
    /// Normal flashing uses [`Self::open_and_revalidate`] so independent images
    /// can hash concurrently while retaining and fingerprinting the exact file
    /// descriptors that WRITE_LBA will later consume.
    pub fn revalidate(&self) -> Result<(), ExecutionError> {
        self.open_and_revalidate().map(drop)
    }

    /// Opens and hashes the staged image, keeping that exact file description
    /// alive for the later WRITE_LBA. This closes the pathname replacement
    /// race between validation and transfer and lets independent images be
    /// validated safely on independent CPU cores.
    pub fn open_and_revalidate(&self) -> Result<ValidatedImage, ExecutionError> {
        let started = Instant::now();
        let mut file = File::open(&self.path).map_err(|error| {
            ExecutionError::StagingChanged(format!("{}: {error}", self.path.display()))
        })?;
        let before = StagedFileFingerprint::read(&file, &self.path)?;
        if before.size_bytes != self.size_bytes {
            return Err(ExecutionError::StagingChanged(format!(
                "{} is {} bytes, was staged at {}",
                self.path.display(),
                before.size_bytes,
                self.size_bytes
            )));
        }
        let (digest, total, validation_backend) = hash_open_file(&mut file, &self.path)?;
        if total != self.size_bytes {
            return Err(ExecutionError::StagingChanged(format!(
                "{} is {total} bytes, was staged at {}",
                self.path.display(),
                self.size_bytes
            )));
        }
        if digest != self.sha256 {
            return Err(ExecutionError::StagingChanged(format!(
                "{} hashes to {digest}, was staged as {}",
                self.path.display(),
                self.sha256
            )));
        }
        let after = StagedFileFingerprint::read(&file, &self.path)?;
        if after != before {
            return Err(ExecutionError::StagingChanged(format!(
                "{} changed while its SHA-256 was being checked",
                self.path.display()
            )));
        }
        Ok(ValidatedImage {
            staged: self.clone(),
            file,
            fingerprint: after,
            validation_duration_ms: started.elapsed().as_millis() as u64,
            validation_backend,
        })
    }
}

/// Hashes the exact descriptor retained for WRITE_LBA. macOS ships a
/// hardware-accelerated OpenSSL SHA-256 implementation; feeding it a clone of
/// this already-open descriptor reduced the measured 2 GiB validation from
/// ~6.9s to ~1.0s on the DAYU200 test host. The child never receives a path,
/// so it cannot follow a replacement between validation and transfer.
#[cfg(target_os = "macos")]
fn hash_open_file(
    file: &mut File,
    path: &Path,
) -> Result<(Sha256Digest, u64, &'static str), ExecutionError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| ExecutionError::StagingChanged(format!("{}: {error}", path.display())))?;
    let exact_descriptor = file
        .try_clone()
        .map_err(|error| ExecutionError::StagingChanged(format!("{}: {error}", path.display())))?;
    let output = Command::new("/usr/bin/openssl")
        .args(["dgst", "-sha256", "-binary"])
        .env_clear()
        .stdin(Stdio::from(exact_descriptor))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map_err(|error| {
            ExecutionError::StagingChanged(format!(
                "cannot run the fixed macOS SHA-256 backend for {}: {error}",
                path.display()
            ))
        })?;
    if !output.status.success() || output.stdout.len() != Sha256Digest::LEN {
        return Err(ExecutionError::StagingChanged(format!(
            "the fixed macOS SHA-256 backend did not return one binary digest for {}",
            path.display()
        )));
    }
    let total = file
        .stream_position()
        .map_err(|error| ExecutionError::StagingChanged(format!("{}: {error}", path.display())))?;
    let digest = Sha256Digest::from_bytes(
        output
            .stdout
            .as_slice()
            .try_into()
            .expect("the SHA-256 output length was checked"),
    );
    Ok((digest, total, "macos-openssl-stdin"))
}

#[cfg(not(target_os = "macos"))]
fn hash_open_file(
    file: &mut File,
    path: &Path,
) -> Result<(Sha256Digest, u64, &'static str), ExecutionError> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 8 << 20];
    let mut total = 0u64;
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            ExecutionError::StagingChanged(format!("{}: {error}", path.display()))
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total += read as u64;
    }
    Ok((hasher.finalize(), total, "portable-vendored-sha256"))
}

/// A full digest proof tied to the exact open file that will be transferred.
#[derive(Debug)]
pub struct ValidatedImage {
    staged: StagedImage,
    file: File,
    fingerprint: StagedFileFingerprint,
    validation_duration_ms: u64,
    validation_backend: &'static str,
}

impl ValidatedImage {
    pub fn staged(&self) -> &StagedImage {
        &self.staged
    }

    pub fn validation_duration_ms(&self) -> u64 {
        self.validation_duration_ms
    }

    pub fn validation_backend(&self) -> &'static str {
        self.validation_backend
    }

    /// Rechecks the non-forgeable inode change time and other file identity
    /// facts, then rewinds the already validated descriptor for WRITE_LBA.
    pub fn prepare_for_write(&mut self) -> Result<&mut File, String> {
        let current = StagedFileFingerprint::read(&self.file, &self.staged.path)
            .map_err(|error| error.to_string())?;
        if current != self.fingerprint {
            return Err(format!(
                "{} changed after its SHA-256 was checked",
                self.staged.path.display()
            ));
        }
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|error| format!("{}: {error}", self.staged.path.display()))?;
        Ok(&mut self.file)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StagedFileFingerprint {
    size_bytes: u64,
    readonly: bool,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    mode: u32,
    #[cfg(unix)]
    links: u64,
    #[cfg(unix)]
    modified_seconds: i64,
    #[cfg(unix)]
    modified_nanoseconds: i64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
    #[cfg(not(unix))]
    modified: Option<std::time::SystemTime>,
}

impl StagedFileFingerprint {
    fn read(file: &File, path: &Path) -> Result<Self, ExecutionError> {
        let metadata = file.metadata().map_err(|error| {
            ExecutionError::StagingChanged(format!("{}: {error}", path.display()))
        })?;
        if !metadata.is_file() {
            return Err(ExecutionError::StagingChanged(format!(
                "{} is not a regular file",
                path.display()
            )));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(Self {
                size_bytes: metadata.len(),
                readonly: metadata.permissions().readonly(),
                device: metadata.dev(),
                inode: metadata.ino(),
                mode: metadata.mode(),
                links: metadata.nlink(),
                modified_seconds: metadata.mtime(),
                modified_nanoseconds: metadata.mtime_nsec(),
                changed_seconds: metadata.ctime(),
                changed_nanoseconds: metadata.ctime_nsec(),
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {
                size_bytes: metadata.len(),
                readonly: metadata.permissions().readonly(),
                modified: metadata.modified().ok(),
            })
        }
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
            return Err(ExecutionError::ActionUndecodable(
                "body is not a map".into(),
            ));
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
                        ));
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
                        ));
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
    validating: BTreeMap<String, JoinHandle<Result<ValidatedImage, ExecutionError>>>,
}

impl ExecutionSession {
    pub fn new(staged: BTreeMap<String, StagedImage>) -> Self {
        ExecutionSession {
            observed_table: None,
            read_domain: None,
            demonstrated_readable: Vec::new(),
            staged,
            validating: BTreeMap::new(),
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

    /// Records an image that has been extracted and verified.
    ///
    /// Staging happens outside this module — it needs the content store, which
    /// a Provider has no business holding — so the session receives the result
    /// rather than producing it.
    pub fn stage(&mut self, member: String, image: StagedImage) {
        self.staged.insert(member, image);
    }

    /// Starts one full SHA-256 worker per independent staged image. The
    /// resulting proof retains the exact open descriptor, so waiting for a
    /// later partition write does not reopen a pathname or trust metadata in
    /// place of bytes.
    pub fn begin_parallel_staged_validation(&mut self) {
        for (member, image) in &self.staged {
            if self.validating.contains_key(member) {
                continue;
            }
            let image = image.clone();
            self.validating.insert(
                member.clone(),
                std::thread::spawn(move || image.open_and_revalidate()),
            );
        }
    }

    fn take_validated_image(
        &mut self,
        member: &str,
    ) -> Result<(ValidatedImage, u64, bool), ExecutionError> {
        let waited = Instant::now();
        let (result, parallel) = match self.validating.remove(member) {
            Some(worker) => (
                worker.join().map_err(|_| {
                    ExecutionError::StagingChanged(format!(
                        "the SHA-256 worker for {member} terminated unexpectedly"
                    ))
                })?,
                true,
            ),
            None => (
                self.staged
                    .get(member)
                    .ok_or_else(|| ExecutionError::ImageNotStaged(member.to_string()))?
                    .open_and_revalidate(),
                false,
            ),
        };
        Ok((result?, waited.elapsed().as_millis() as u64, parallel))
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
    port: &dyn RockUsbPort,
    scratch: &Path,
) -> Result<ActionOutcome, ExecutionError> {
    match action {
        StoredAction::ManagedControl { control_action, .. } => {
            Err(ExecutionError::RequiresAuthority {
                control_action: control_action.clone(),
            })
        }
        StoredAction::ProbeLoader => {
            let receipt = port
                .discover()
                .map_err(|error| port_error("discoverDevices", error))?;
            let summary = receipt
                .value
                .iter()
                .map(RockUsbDevice::summary)
                .collect::<Vec<_>>()
                .join("; ");
            Ok(outcome(
                record,
                ActionDisposition::SemanticSuccess,
                vec![fact("rockusbDevices", summary)],
                receipt.evidence_digest,
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

            let receipt = port
                .read_partition_table()
                .map_err(|error| port_error("readPartitionTable", error))?;
            let observed = receipt.value;
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
                receipt.evidence_digest,
                None,
            ))
        }
        StoredAction::CharacterizeReadDomain => {
            characterize_read_domain(record, session, port, scratch)
        }
        StoredAction::WritePartition {
            partition,
            member,
            begin_sector,
        } => write_partition(
            record,
            session,
            profile,
            port,
            partition,
            member,
            *begin_sector,
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
            let receipt = port
                .reset_device()
                .map_err(|error| port_error("resetDevice", error))?;
            let disposition = if receipt.semantic_success {
                ActionDisposition::SemanticSuccess
            } else {
                // A reset whose marker never appeared may still have reset the
                // device. Unknown, not failed.
                ActionDisposition::OutcomeUnknown
            };
            Ok(outcome(
                record,
                disposition,
                vec![fact("operation", "DEVICE_RESET")],
                receipt.evidence_digest,
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
    port: &dyn RockUsbPort,
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
    port: &dyn RockUsbPort,
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

    let device_offset_sectors = entry.offset_sectors;
    let partition_size_sectors = entry.size_sectors;

    // 3. The staged image is still the image that was staged.
    let staged = session
        .staged
        .get(member)
        .ok_or_else(|| ExecutionError::ImageNotStaged(member.to_string()))?
        .clone();

    // 4. It has to fit inside the span the device's table gives it. This is a
    //    refusal, not an addressing step: a write that would cross into the
    //    next partition is refused before external I/O begins.
    let image_sectors = ceil_div(staged.size_bytes, ROCKUSB_SECTOR_BYTES);
    if let Some(size_sectors) = partition_size_sectors
        && image_sectors > size_sectors
    {
        return Err(ExecutionError::ImageOverrunsPartition {
            partition: partition.to_string(),
            image_sectors,
            partition_sectors: size_sectors,
        });
    }

    // 5. Only now, the write. Address the native transfer from the observed
    //    entry itself — the equality checks above prove the plan and Profile
    //    agree, but neither is allowed to replace the device fact. From the
    //    first external I/O the device may have changed, so nothing after this
    //    point may report "no effect".
    let (mut validated, validation_wait_ms, parallel_validation) =
        session.take_validated_image(member)?;
    let validation_duration_ms = validated.validation_duration_ms();
    let validation_backend = validated.validation_backend();
    let image = validated.staged().clone();
    let receipt = port
        .write_partition(partition, device_offset_sectors, &mut validated)
        .map_err(|error| port_error("writePartition", error))?;
    if let Some(progress) = &receipt.progress
        && (progress.payload_bytes != image.size_bytes || progress.payload_digest != image.sha256)
    {
        return Err(ExecutionError::ExternalIo {
            operation: "writePartition".into(),
            message: format!(
                "native WRITE_LBA sent {} bytes hashing to {}; staged image is {} bytes hashing to {}",
                progress.payload_bytes, progress.payload_digest, image.size_bytes, image.sha256
            ),
        });
    }
    let disposition = if receipt.semantic_success {
        ActionDisposition::SemanticSuccess
    } else {
        // External I/O began. Whether it wrote anything is not knowable from
        // an unconfirmed receipt (architecture.md 14.1).
        ActionDisposition::OutcomeUnknown
    };

    let mut facts = vec![
        fact("partition", partition),
        fact("member", member),
        fact("imageSha256", image.sha256.to_string()),
        fact("imageBytes", image.size_bytes.to_string()),
        fact(
            "imageValidationDurationMs",
            validation_duration_ms.to_string(),
        ),
        fact("imageValidationWaitMs", validation_wait_ms.to_string()),
        fact("imageValidationBackend", validation_backend),
        fact(
            "imageValidationMode",
            if parallel_validation {
                "parallel-open-file"
            } else {
                "inline-open-file"
            },
        ),
        fact("beginSector", begin_sector.to_string()),
        fact("operationDurationMs", receipt.duration_ms.to_string()),
    ];
    if let Some(progress) = &receipt.progress {
        facts.push(fact(
            "writePayloadBytes",
            progress.payload_bytes.to_string(),
        ));
        facts.push(fact("writeWireSectors", progress.wire_sectors.to_string()));
        facts.push(fact("writeChunks", progress.chunks.to_string()));
        facts.push(fact(
            "writeChunkSectors",
            progress.chunk_sectors.to_string(),
        ));
        facts.push(fact(
            "writePayloadSha256",
            progress.payload_digest.to_string(),
        ));
    }
    if disposition != ActionDisposition::SemanticSuccess {
        // The receipt text itself is digested, not stored, so an unexplained
        // write used to leave nothing behind but "unknown" — the one fact that
        // names the cause was the one fact thrown away. The tail of the
        // executor's own detail string is where it says why it stopped
        // (AD-032's lesson, restated for the native receipt).
        let tail: String = receipt
            .detail
            .chars()
            .rev()
            .take(400)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        facts.push(fact("operationDetailTail", tail));
    }
    Ok(outcome(
        record,
        disposition,
        facts,
        receipt.evidence_digest,
        None,
    ))
}

#[allow(clippy::too_many_arguments)]
fn readback_partition(
    record: &PrivateActionRecord,
    session: &mut ExecutionSession,
    port: &dyn RockUsbPort,
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

    let sectors = ceil_div(range.length, ROCKUSB_SECTOR_BYTES);
    // On a windowed read face, probe the end of the declared range before
    // transferring the whole image. If that endpoint is the measured filler,
    // the range cannot support a full-hash claim; reading gigabytes of the same
    // uninformative byte only to reach the same TypedSkip wastes minutes and
    // can turn a range that crosses the window edge into a false mismatch.
    if matches!(domain, MeasuredReadDomain::Windowed { .. }) {
        let last_sector = begin_sector
            .checked_add(sectors.saturating_sub(1))
            .ok_or(ExecutionError::VerificationRangeMissing)?;
        let tail = read_sectors(
            port,
            scratch,
            last_sector,
            1,
            &format!("{partition}-range-end-probe"),
        )?;
        let is_declared_filler = uniform_byte(&tail)
            .is_some_and(|byte| filler.map(|declared| declared == byte).unwrap_or(true));
        if is_declared_filler {
            let verification = VerificationOutcome::TypedSkip {
                range,
                reason: TypedSkipReason::OutsideReadDomain,
                detail: format!(
                    "range endpoint sector {last_sector} returned the measured filler through a \
                     windowed read face; the full range was not transferred"
                ),
            };
            return Ok(outcome(
                record,
                ActionDisposition::SemanticSuccess,
                vec![
                    fact("partition", partition),
                    fact("addressableMedium", domain.summary()),
                    fact("outcome", verification.as_str()),
                    fact("readbackStrategy", "range-end-probe"),
                    fact("probedSector", last_sector.to_string()),
                    fact("readbackBytes", tail.len().to_string()),
                ],
                sha256(&tail),
                Some(verification),
            ));
        }
    }
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
        fact("readbackStrategy", "full-range"),
        fact("readbackBytes", read.len().to_string()),
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
    port: &dyn RockUsbPort,
    scratch: &Path,
    begin_sector: u64,
    sectors: u64,
    label: &str,
) -> Result<Vec<u8>, ExecutionError> {
    let operation_scratch = scratch.join(format!("read-{label}"));
    std::fs::create_dir_all(&operation_scratch).map_err(|error| {
        ExecutionError::ScratchUnusable(format!("{}: {error}", operation_scratch.display()))
    })?;
    port.read_sectors(begin_sector, sectors, &operation_scratch)
        .map(|receipt| receipt.value)
        .map_err(|error| port_error("readSectors", error))
}

fn port_error(operation: &str, error: RockUsbPortFailure) -> ExecutionError {
    match error {
        RockUsbPortFailure::BeforeIo(message) => ExecutionError::PortRefused {
            operation: operation.into(),
            message,
        },
        RockUsbPortFailure::AfterIo(message) => ExecutionError::ExternalIo {
            operation: operation.into(),
            message,
        },
    }
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
    (OpaqueId::new(key).expect("literal fact key"), value.into())
}

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

/// Applies the same profile/device layout gate used by normal execution.
///
/// Native rescue must not grow a second, weaker interpretation of the device's
/// GPT. It can plan a write only when every allowed target and every protected
/// target is understood by the shipped profile.
pub fn validate_partition_table_for_profile(
    observed: &PartitionTableFact,
    profile: &DeviceProfile,
) -> Result<(), ExecutionError> {
    check_conformance(observed, profile)
}

/// The Profile's own declared layout, hashed the way the Provider hashed it
/// when it built the plan. Recomputed here so a plan that drifted from the
/// Profile it names is caught before the device is touched.
fn profile_layout_digest(profile: &DeviceProfile) -> Sha256Digest {
    use arkforge_core::digest::{Domain, digest_in_domain};
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

/// Stable evidence digest for the exact name/offset table observed on-device.
pub fn observed_layout_digest(table: &PartitionTableFact) -> Sha256Digest {
    layout_digest_of(table)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionError {
    /// The action belongs to the authority's control port.
    RequiresAuthority {
        control_action: String,
    },
    ActionUndecodable(String),
    PortRefused {
        operation: String,
        message: String,
    },
    ExternalIo {
        operation: String,
        message: String,
    },
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
            ExecutionError::PortRefused { operation, message } => {
                write!(f, "{operation} was refused before I/O: {message}")
            }
            ExecutionError::ExternalIo { operation, message } => {
                write!(
                    f,
                    "{operation} failed after native USB I/O began: {message}"
                )
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
            ExecutionError::TargetNotAllowed(partition) => {
                write!(f, "the profile does not allow writing {partition}")
            }
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
        let (mut validated, _wait_ms, parallel) =
            session.take_validated_image("uboot.img").unwrap();
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
            self.writes.borrow_mut().push((
                partition.to_string(),
                begin_sector,
                image.path.clone(),
            ));
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
}
