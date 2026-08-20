//! Running a step's private action, outside the service lock.
//!
//! architecture.md 12, 16.1. The job registry hands out a [`PendingDispatch`]
//! and takes back a [`DispatchOutcome`]; everything between those two points
//! happens here, with no lock held. That matters because the daemon serves
//! every connection under one mutex and a 2 GiB partition write takes minutes:
//! a dispatcher that ran under the lock would freeze the event stream that was
//! supposed to report on it.
//!
//! # What this owns
//!
//! Per job, an [`ExecutionSession`] — the device's observed partition table,
//! the measured read domain, and the staged images. It lives here rather than
//! in the job registry because it is execution state, not admission state, and
//! because it must survive between steps: a write refuses unless the table was
//! observed, and a readback refuses unless the read face was measured.
//!
//! # What this does not own
//!
//! Any decision about whether a step may run. That was settled before the work
//! arrived — a permit was verified and an intent was made durable. This
//! dispatcher runs what it is given and reports what it saw.

use crate::jobs::{DispatchOutcome, PendingDispatch};
use arkforge_artifact::cas::{CasQuota, ContentAddressedStore};
use arkforge_artifact::dayu200;
use arkforge_artifact::staging::stage_members;
use arkforge_core::outcome::ActionDisposition;
use arkforge_provider::rockchip_execute::{
    ExecutionError, ExecutionSession, RockUsbDevice, RockUsbLocation, RockUsbMutationReceipt,
    RockUsbObservation, RockUsbPort, RockUsbPortFailure, RockUsbWriteProgress, StagedImage,
    StoredAction, execute_action,
};
use arkforge_provider::rockusb_protocol::{RockUsbBulkIo, RockUsbProtocol};
use core::fmt;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// Runs private actions for one daemon's jobs.
#[derive(Debug)]
pub struct Dispatcher<'a> {
    store_root: PathBuf,
    work_root: PathBuf,
    port: &'a dyn RockUsbPort,
    sessions: BTreeMap<String, ExecutionSession>,
    /// Jobs whose images are already on disk, so staging happens once.
    staged: BTreeSet<String>,
}

impl<'a> Dispatcher<'a> {
    pub fn new(
        store_root: impl Into<PathBuf>,
        work_root: impl Into<PathBuf>,
        port: &'a dyn RockUsbPort,
    ) -> Self {
        Dispatcher {
            store_root: store_root.into(),
            work_root: work_root.into(),
            port,
            sessions: BTreeMap::new(),
            staged: BTreeSet::new(),
        }
    }

    /// Runs one piece of work and reports what happened.
    ///
    /// Never returns an error: every failure is a disposition. A dispatcher
    /// that returned `Err` would leave the caller to invent one, and the two
    /// answers it might invent — "failed" and "unknown" — are exactly the pair
    /// that must not be confused (architecture.md 12.4).
    pub fn run(&mut self, work: &PendingDispatch) -> DispatchOutcome {
        match self.try_run(work) {
            Ok(outcome) => outcome,
            Err(failure) => DispatchOutcome {
                disposition: failure.disposition(),
                facts: vec![
                    ("dispatchFailure".into(), failure.to_string()),
                    ("step".into(), work.step_id.clone()),
                ],
                evidence_digest: arkforge_core::digest::sha256(failure.to_string().as_bytes()),
                verification: None,
            },
        }
    }

    fn try_run(&mut self, work: &PendingDispatch) -> Result<DispatchOutcome, DispatchFailure> {
        let decoded: Vec<StoredAction> = work
            .actions
            .iter()
            .map(|action| {
                StoredAction::decode(action)
                    .map_err(|error| DispatchFailure::BeforeAnyEffect(error.to_string()))
            })
            .collect::<Result<_, _>>()?;

        // Images are staged on the first write and not before: a job that never
        // reaches one should not pay 4 GB of extraction to find that out.
        if decoded
            .iter()
            .any(|action| matches!(action, StoredAction::WritePartition { .. }))
        {
            self.stage_if_needed(work)?;
        }

        let scratch = self.job_root(&work.job_id).join("scratch");
        std::fs::create_dir_all(&scratch)
            .map_err(|error| DispatchFailure::BeforeAnyEffect(error.to_string()))?;
        let session = self
            .sessions
            .entry(work.job_id.clone())
            .or_insert_with(|| ExecutionSession::new(BTreeMap::new()));

        // Sub-actions first, then the primary. The reported outcome is the
        // primary's: a sub-action exists to establish something the primary
        // needs, and a receipt describing the measurement rather than the
        // effect would say nothing about the device.
        let mut last = None;
        for (action, record) in decoded.iter().zip(&work.actions) {
            let outcome =
                execute_action(action, record, session, &work.profile, self.port, &scratch)
                    .map_err(classify)?;
            last = Some(outcome);
        }
        let outcome = last.ok_or_else(|| {
            DispatchFailure::BeforeAnyEffect("the step declares no private action".into())
        })?;
        Ok(DispatchOutcome {
            disposition: outcome.disposition,
            facts: outcome
                .facts
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
            evidence_digest: outcome.evidence_digest,
            verification: outcome.verification,
        })
    }

    /// Extracts the images this job's writes need, once.
    fn stage_if_needed(&mut self, work: &PendingDispatch) -> Result<(), DispatchFailure> {
        if self.staged.contains(&work.job_id) {
            return Ok(());
        }
        let store = ContentAddressedStore::open(&self.store_root, CasQuota::dayu200_default())
            .map_err(|error| DispatchFailure::BeforeAnyEffect(error.to_string()))?;
        let object = store
            .open_object(&work.artifact_digest)
            .map_err(|error| DispatchFailure::BeforeAnyEffect(error.to_string()))?;
        let manifest = dayu200::inspect(
            store
                .open_object(&work.artifact_digest)
                .map_err(|error| DispatchFailure::BeforeAnyEffect(error.to_string()))?,
        )
        .map_err(|error| DispatchFailure::BeforeAnyEffect(error.to_string()))?;

        let wanted: BTreeSet<String> = work
            .profile
            .allowed_targets
            .iter()
            .filter_map(|target| target.source_member.clone())
            .collect();
        let directory = self.job_root(&work.job_id).join("staging");
        std::fs::create_dir_all(&directory)
            .map_err(|error| DispatchFailure::BeforeAnyEffect(error.to_string()))?;

        let report = stage_members(object, &manifest, &wanted, &directory)
            .map_err(|error| DispatchFailure::BeforeAnyEffect(error.to_string()))?;

        let session = self
            .sessions
            .entry(work.job_id.clone())
            .or_insert_with(|| ExecutionSession::new(BTreeMap::new()));
        for (name, member) in report.members {
            session.stage(
                name,
                StagedImage {
                    member: member.member,
                    path: member.path,
                    size_bytes: member.size_bytes,
                    sha256: member.sha256,
                },
            );
        }
        self.staged.insert(work.job_id.clone());
        Ok(())
    }

    fn job_root(&self, job_id: &str) -> PathBuf {
        self.work_root.join(job_id)
    }

    /// Removes a finished job's staging directory.
    ///
    /// A failure to clean up is local debt. It does not make anything already
    /// observed about the device unknowable, so it is reported and not raised.
    pub fn release(&mut self, job_id: &str) -> Result<(), String> {
        self.sessions.remove(job_id);
        self.staged.remove(job_id);
        let root = self.job_root(job_id);
        if !root.exists() {
            return Ok(());
        }
        std::fs::remove_dir_all(&root).map_err(|error| format!("{}: {error}", root.display()))
    }

    pub fn work_root(&self) -> &Path {
        &self.work_root
    }
}

/// Why a dispatch did not produce a receipt, and what that implies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchFailure {
    /// Refused before anything could reach the device. The device is untouched
    /// and provably so: no child process or native USB request was dispatched.
    BeforeAnyEffect(String),
    /// External I/O began and did not report its own semantic success.
    /// Whether the device changed is unknown (architecture.md 14.1).
    AfterExternalIo(String),
}

impl DispatchFailure {
    pub fn disposition(&self) -> ActionDisposition {
        match self {
            DispatchFailure::BeforeAnyEffect(_) => ActionDisposition::ConfirmedNoEffect,
            DispatchFailure::AfterExternalIo(_) => ActionDisposition::OutcomeUnknown,
        }
    }
}

impl fmt::Display for DispatchFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DispatchFailure::BeforeAnyEffect(detail) => {
                write!(f, "refused before external I/O began: {detail}")
            }
            DispatchFailure::AfterExternalIo(detail) => {
                write!(
                    f,
                    "external I/O began and did not confirm its effect: {detail}"
                )
            }
        }
    }
}

/// Which side of the first external I/O an execution error falls on.
///
/// This is the only judgement this module makes, and it is the one that
/// matters: everything the executor refuses *before* running the tool leaves
/// the device provably untouched, and everything after it does not. Getting
/// this backwards would either report a real effect as "no effect", or turn
/// every refused precondition into an unresolved job.
fn classify(error: ExecutionError) -> DispatchFailure {
    match error {
        // Every one of these is a refusal before native USB I/O begins.
        ExecutionError::RequiresAuthority { .. }
        | ExecutionError::ActionUndecodable(_)
        | ExecutionError::PortRefused { .. }
        | ExecutionError::LayoutMismatch { .. }
        | ExecutionError::PartitionTableUnreadable(_)
        | ExecutionError::DeviceDeclaresUnknownPartitions(_)
        | ExecutionError::TableNotObservedYet
        | ExecutionError::ReadDomainNotCharacterized
        | ExecutionError::NoTableAtLba1
        | ExecutionError::TargetNotAllowed(_)
        | ExecutionError::PartitionNotOnDevice(_)
        | ExecutionError::TargetOffsetDisagrees { .. }
        | ExecutionError::ImageNotStaged(_)
        | ExecutionError::ImageOverrunsPartition { .. }
        | ExecutionError::StagingChanged(_)
        | ExecutionError::VerificationRangeMissing
        | ExecutionError::ScratchUnusable(_) => DispatchFailure::BeforeAnyEffect(error.to_string()),
        // The port was reached, so a native USB request may have run.
        ExecutionError::ExternalIo { .. } | ExecutionError::ReadFailed { .. } => {
            DispatchFailure::AfterExternalIo(error.to_string())
        }
    }
}

/// Native DAYU200 Loader port. Each semantic call claims the exact Loader
/// interface, confirms TEST_UNIT_READY, performs one typed protocol operation,
/// and releases the claim.
#[derive(Debug)]
pub struct NativeRockUsbPort {
    usb: arkforge_usb::NativeUsb,
    selector: arkforge_usb::UsbInterfaceSelector,
    target: Option<arkforge_usb::UsbInterfaceDescriptor>,
    next_tag: AtomicU32,
}

impl Default for NativeRockUsbPort {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeRockUsbPort {
    pub fn new() -> Self {
        Self {
            usb: arkforge_usb::NativeUsb::new(30_000),
            selector: arkforge_usb::UsbInterfaceSelector::dayu200_loader(),
            target: None,
            next_tag: AtomicU32::new(1),
        }
    }

    pub(crate) fn for_descriptor(
        descriptor: arkforge_usb::UsbInterfaceDescriptor,
    ) -> Result<Self, RockUsbPortFailure> {
        let selector = arkforge_usb::UsbInterfaceSelector::dayu200_loader();
        if !selector.matches(&descriptor) {
            return Err(RockUsbPortFailure::BeforeIo(format!(
                "the selected USB interface is not an allowed DAYU200 Loader: {:04x}:{:04x} at {:08x}",
                descriptor.vendor_id, descriptor.product_id, descriptor.location_id
            )));
        }
        Ok(Self {
            usb: arkforge_usb::NativeUsb::new(30_000),
            selector,
            target: Some(descriptor),
            next_tag: AtomicU32::new(1),
        })
    }

    pub(crate) fn matching_descriptors(
        &self,
    ) -> Result<Vec<arkforge_usb::UsbInterfaceDescriptor>, RockUsbPortFailure> {
        self.usb
            .enumerate()
            .map(|records| {
                records
                    .into_iter()
                    .filter(|record| {
                        self.selector.matches(record)
                            && self
                                .target
                                .as_ref()
                                .map(|target| target == record)
                                .unwrap_or(true)
                    })
                    .collect()
            })
            .map_err(|error| RockUsbPortFailure::BeforeIo(error.to_string()))
    }

    fn open_interface(&self) -> Result<Box<dyn arkforge_usb::BulkInterface>, RockUsbPortFailure> {
        match &self.target {
            Some(target) => self
                .usb
                .open_exact(self.selector, target)
                .map_err(|error| RockUsbPortFailure::BeforeIo(error.to_string())),
            None => self
                .usb
                .open_unique(self.selector)
                .map_err(|error| RockUsbPortFailure::BeforeIo(error.to_string())),
        }
    }

    fn with_protocol<T>(
        &self,
        operation: impl FnOnce(&mut RockUsbProtocol<'_>) -> Result<T, String>,
    ) -> Result<T, RockUsbPortFailure> {
        let interface = self.open_interface()?;
        let mut io = NativeBulkIo { interface };
        let first_tag = self.next_tag.fetch_add(0x100, Ordering::Relaxed);
        let mut protocol = RockUsbProtocol::new(&mut io, first_tag);
        protocol
            .test_unit_ready()
            .map_err(|error| RockUsbPortFailure::AfterIo(error.to_string()))?;
        operation(&mut protocol).map_err(RockUsbPortFailure::AfterIo)
    }

    pub fn read_capacity_sectors(&self) -> Result<u64, RockUsbPortFailure> {
        self.with_protocol(|protocol| {
            protocol
                .read_capacity_sectors()
                .map_err(|error| error.to_string())
        })
    }

    pub fn read_bytes(
        &self,
        begin_sector: u64,
        sectors: u64,
    ) -> Result<Vec<u8>, RockUsbPortFailure> {
        self.with_protocol(|protocol| {
            protocol
                .read_lba(begin_sector, sectors)
                .map_err(|error| error.to_string())
        })
    }
}

#[derive(Debug)]
struct NativeBulkIo {
    interface: Box<dyn arkforge_usb::BulkInterface>,
}

impl RockUsbBulkIo for NativeBulkIo {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.interface
            .write_all(bytes)
            .map_err(|error| error.to_string())
    }

    fn read_exact(&mut self, bytes: &mut [u8]) -> Result<(), String> {
        self.interface
            .read_exact(bytes)
            .map_err(|error| error.to_string())
    }

    fn read_some(&mut self, bytes: &mut [u8]) -> Result<usize, String> {
        self.interface
            .read_some(bytes)
            .map_err(|error| error.to_string())
    }
}

pub(crate) fn device_from_descriptor(
    descriptor: arkforge_usb::UsbInterfaceDescriptor,
) -> RockUsbDevice {
    RockUsbDevice {
        vendor_id: descriptor.vendor_id,
        product_id: descriptor.product_id,
        usb_specification: Some(descriptor.usb_specification),
        location: RockUsbLocation::IokitTopology(descriptor.location_id),
        mode: "loader".into(),
        serial: descriptor.serial,
        product_name: descriptor.product_name,
        vendor_name: descriptor.vendor_name,
        device_release: Some(descriptor.device_release),
    }
}

impl RockUsbPort for NativeRockUsbPort {
    fn discover(&self) -> Result<RockUsbObservation<Vec<RockUsbDevice>>, RockUsbPortFailure> {
        let descriptors = self.matching_descriptors()?;
        if descriptors.len() != 1 {
            return Err(RockUsbPortFailure::BeforeIo(format!(
                "expected one exact DAYU200 Loader interface, observed {}",
                descriptors.len()
            )));
        }
        // A descriptor is not protocol evidence. Claim the interface and ask
        // the Loader itself before publishing discovery.
        self.with_protocol(|_| Ok(()))?;
        let devices: Vec<RockUsbDevice> = descriptors
            .into_iter()
            .map(device_from_descriptor)
            .collect();
        let mut evidence = Vec::new();
        for device in &devices {
            evidence.extend_from_slice(device.summary().as_bytes());
            evidence.push(0);
            evidence.extend_from_slice(device.serial.as_deref().unwrap_or("").as_bytes());
            evidence.push(0);
        }
        Ok(RockUsbObservation {
            value: devices,
            evidence_digest: arkforge_core::digest::sha256(&evidence),
        })
    }

    fn capacity_sectors(&self) -> Result<RockUsbObservation<u64>, RockUsbPortFailure> {
        let sectors = self.read_capacity_sectors()?;
        Ok(RockUsbObservation {
            value: sectors,
            evidence_digest: arkforge_core::digest::sha256(&sectors.to_be_bytes()),
        })
    }

    fn read_partition_table(
        &self,
    ) -> Result<
        RockUsbObservation<arkforge_artifact::manifest::PartitionTableFact>,
        RockUsbPortFailure,
    > {
        let table = self.with_protocol(|protocol| {
            protocol
                .read_partition_table()
                .map_err(|error| error.to_string())
        })?;
        let mut evidence = Vec::new();
        for entry in &table.entries {
            evidence.extend_from_slice(entry.name.as_bytes());
            evidence.push(b'@');
            evidence.extend_from_slice(entry.offset_sectors.to_string().as_bytes());
            evidence.push(b'\n');
        }
        Ok(RockUsbObservation {
            value: table,
            evidence_digest: arkforge_core::digest::sha256(&evidence),
        })
    }

    fn read_sectors(
        &self,
        begin_sector: u64,
        sectors: u64,
        _scratch: &Path,
    ) -> Result<RockUsbObservation<Vec<u8>>, RockUsbPortFailure> {
        let bytes = self.read_bytes(begin_sector, sectors)?;
        Ok(RockUsbObservation {
            evidence_digest: arkforge_core::digest::sha256(&bytes),
            value: bytes,
        })
    }

    fn write_partition(
        &self,
        partition: &str,
        begin_sector: u64,
        image: &StagedImage,
    ) -> Result<RockUsbMutationReceipt, RockUsbPortFailure> {
        if partition.is_empty() {
            return Err(RockUsbPortFailure::BeforeIo(
                "native WRITE_LBA target has an empty partition name".into(),
            ));
        }
        // Open and size the already revalidated staging file before claiming
        // USB. A local path failure cannot become an unknown device effect.
        let mut file = std::fs::File::open(&image.path).map_err(|error| {
            RockUsbPortFailure::BeforeIo(format!("{}: {error}", image.path.display()))
        })?;
        let observed_bytes = file
            .metadata()
            .map_err(|error| {
                RockUsbPortFailure::BeforeIo(format!("{}: {error}", image.path.display()))
            })?
            .len();
        if observed_bytes != image.size_bytes {
            return Err(RockUsbPortFailure::BeforeIo(format!(
                "{} is {observed_bytes} bytes; the staged input is exactly {} bytes",
                image.path.display(),
                image.size_bytes
            )));
        }
        let total_bytes = image.size_bytes;
        if total_bytes == 0 {
            return Err(RockUsbPortFailure::BeforeIo(format!(
                "{} is empty; refusing a zero-length WRITE_LBA",
                image.path.display()
            )));
        }
        let total_sectors = total_bytes / 512 + u64::from(!total_bytes.is_multiple_of(512));
        let end_sector = begin_sector.checked_add(total_sectors).ok_or_else(|| {
            RockUsbPortFailure::BeforeIo("native WRITE_LBA sector range overflows".into())
        })?;
        if begin_sector > u32::MAX as u64 || end_sector > u32::MAX as u64 + 1 {
            return Err(RockUsbPortFailure::BeforeIo(format!(
                "native WRITE_LBA range {begin_sector}+{total_sectors} exceeds the protocol"
            )));
        }

        let interface = self.open_interface()?;
        let mut io = NativeBulkIo { interface };
        let first_tag = self.next_tag.fetch_add(0x100, Ordering::Relaxed);
        let mut protocol = RockUsbProtocol::new(&mut io, first_tag);
        // Read-only readiness happens before the mutation boundary. If it
        // fails, no WRITE_LBA CBW was sent and no write effect is possible.
        protocol
            .test_unit_ready()
            .map_err(|error| RockUsbPortFailure::BeforeIo(error.to_string()))?;

        let started = std::time::Instant::now();
        let mut hasher = arkforge_core::digest::Sha256::new();
        let mut buffer = vec![0u8; 128 * 512];
        let mut remaining = total_bytes;
        let mut position = begin_sector;
        let mut chunks = 0u64;
        let local_failure = |message: String, chunks: u64| {
            if chunks == 0 {
                RockUsbPortFailure::BeforeIo(message)
            } else {
                RockUsbPortFailure::AfterIo(message)
            }
        };
        while remaining > 0 {
            let wanted = usize::try_from(remaining.min(buffer.len() as u64))
                .expect("bounded by the host buffer");
            let mut filled = 0usize;
            while filled < wanted {
                let read = file.read(&mut buffer[filled..wanted]).map_err(|error| {
                    local_failure(format!("{}: {error}", image.path.display()), chunks)
                })?;
                if read == 0 {
                    return Err(local_failure(
                        format!(
                            "{} became shorter while native WRITE_LBA was reading it",
                            image.path.display()
                        ),
                        chunks,
                    ));
                }
                filled += read;
            }
            hasher.update(&buffer[..filled]);
            let progress = protocol
                .write_lba(position, &buffer[..filled])
                .map_err(|error| RockUsbPortFailure::AfterIo(error.to_string()))?;
            position += progress.wire_sectors;
            remaining -= filled as u64;
            chunks += progress.chunks;
        }
        let mut extra = [0u8; 1];
        let extra_read = file.read(&mut extra).map_err(|error| {
            RockUsbPortFailure::AfterIo(format!("{}: {error}", image.path.display()))
        })?;
        if extra_read != 0 {
            return Err(RockUsbPortFailure::AfterIo(format!(
                "{} grew while native WRITE_LBA was reading it",
                image.path.display()
            )));
        }

        let payload_digest = hasher.finalize();
        if payload_digest != image.sha256 {
            return Err(RockUsbPortFailure::AfterIo(format!(
                "native WRITE_LBA payload hashes to {payload_digest}; staged input is {}",
                image.sha256
            )));
        }
        let progress = RockUsbWriteProgress {
            payload_bytes: total_bytes,
            wire_sectors: total_sectors,
            chunks,
            payload_digest,
        };
        let detail = format!(
            "native WRITE_LBA confirmed partition={partition} begin={begin_sector} bytes={} sectors={} chunks={}",
            progress.payload_bytes, progress.wire_sectors, progress.chunks
        );
        Ok(RockUsbMutationReceipt {
            semantic_success: true,
            evidence_digest: arkforge_core::digest::sha256(
                format!("{detail} sha256={payload_digest}").as_bytes(),
            ),
            duration_ms: started.elapsed().as_millis() as u64,
            detail,
            progress: Some(progress),
        })
    }

    fn reset_device(&self) -> Result<RockUsbMutationReceipt, RockUsbPortFailure> {
        let interface = self.open_interface()?;
        let mut io = NativeBulkIo { interface };
        let first_tag = self.next_tag.fetch_add(0x100, Ordering::Relaxed);
        let mut protocol = RockUsbProtocol::new(&mut io, first_tag);
        protocol
            .test_unit_ready()
            .map_err(|error| RockUsbPortFailure::BeforeIo(error.to_string()))?;
        let started = std::time::Instant::now();
        protocol
            .reset_device()
            .map_err(|error| RockUsbPortFailure::AfterIo(error.to_string()))?;
        let detail = "native DEVICE_RESET confirmed by matching CSW".to_string();
        Ok(RockUsbMutationReceipt {
            semantic_success: true,
            evidence_digest: arkforge_core::digest::sha256(detail.as_bytes()),
            duration_ms: started.elapsed().as_millis() as u64,
            detail,
            progress: None,
        })
    }
}

pub fn executable_digest(path: &Path) -> Result<arkforge_core::Sha256Digest, String> {
    let mut file =
        std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut hasher = arkforge_core::digest::Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_write_local_preconditions_are_checked_before_usb_io() {
        let port = NativeRockUsbPort::new();
        assert!(matches!(
            port.write_partition(
                "uboot",
                0x2000,
                &StagedImage {
                    member: "uboot.img".into(),
                    path: PathBuf::from("/never-opened.img"),
                    size_bytes: 512,
                    sha256: arkforge_core::digest::sha256(b"missing"),
                }
            ),
            Err(RockUsbPortFailure::BeforeIo(_))
        ));
        let empty = std::env::temp_dir().join(format!(
            "arkforge-native-empty-write-{}",
            std::process::id()
        ));
        std::fs::write(&empty, []).unwrap();
        assert!(matches!(
            port.write_partition(
                "uboot",
                0x2000,
                &StagedImage {
                    member: "uboot.img".into(),
                    path: empty.clone(),
                    size_bytes: 0,
                    sha256: arkforge_core::digest::sha256(b""),
                }
            ),
            Err(RockUsbPortFailure::BeforeIo(_))
        ));
        let _ = std::fs::remove_file(empty);
    }

    #[test]
    fn a_refusal_before_external_io_confirms_no_effect() {
        for error in [
            ExecutionError::PortRefused {
                operation: "writePartitionByName".into(),
                message: "port refused before external I/O".into(),
            },
            ExecutionError::TargetNotAllowed("misc".into()),
            ExecutionError::TableNotObservedYet,
            ExecutionError::ReadDomainNotCharacterized,
            ExecutionError::StagingChanged("digest changed".into()),
            ExecutionError::ImageOverrunsPartition {
                partition: "uboot".into(),
                image_sectors: 9000,
                partition_sectors: 8192,
            },
        ] {
            assert_eq!(
                classify(error.clone()).disposition(),
                ActionDisposition::ConfirmedNoEffect,
                "{error}"
            );
        }
    }

    #[test]
    fn a_failure_after_external_io_leaves_the_outcome_unknown() {
        for error in [
            ExecutionError::ExternalIo {
                operation: "writePartition".into(),
                message: "killed".into(),
            },
            ExecutionError::ReadFailed {
                begin_sector: 1,
                sectors: 1,
                output: "quit".into(),
            },
        ] {
            assert_eq!(
                classify(error.clone()).disposition(),
                ActionDisposition::OutcomeUnknown,
                "{error}"
            );
        }
    }
}
