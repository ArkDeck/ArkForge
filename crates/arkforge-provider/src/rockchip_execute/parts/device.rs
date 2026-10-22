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
        #[cfg(windows)]
        #[allow(clippy::permissions_set_readonly_false)]
        {
            Ok(Self {
                size_bytes: metadata.len(),
                readonly: metadata.permissions().readonly(),
                modified: metadata.modified().ok(),
            })
        }
    }
}
