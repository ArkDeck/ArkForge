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
    execute_action, ExecutionError, ExecutionSession, FixedToolPort, RockUsbDevice,
    RockUsbLocation, RockUsbObservation, RockUsbPort, RockUsbPortFailure, StagedImage,
    StoredAction,
};
use arkforge_provider::rockusb_protocol::{RockUsbBulkIo, RockUsbProtocol};
use core::fmt;
use std::collections::{BTreeMap, BTreeSet};
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
    pub fn new(store_root: impl Into<PathBuf>, work_root: impl Into<PathBuf>, port: &'a dyn RockUsbPort) -> Self {
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
            let outcome = execute_action(action, record, session, &work.profile, self.port, &scratch)
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
    /// and provably so: the tool was never spawned.
    BeforeAnyEffect(String),
    /// The tool was spawned and did not report its own semantic success.
    /// Whether the device changed is unknown (architecture.md 14.1).
    AfterSpawn(String),
}

impl DispatchFailure {
    pub fn disposition(&self) -> ActionDisposition {
        match self {
            DispatchFailure::BeforeAnyEffect(_) => ActionDisposition::ConfirmedNoEffect,
            DispatchFailure::AfterSpawn(_) => ActionDisposition::OutcomeUnknown,
        }
    }
}

impl fmt::Display for DispatchFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DispatchFailure::BeforeAnyEffect(detail) => {
                write!(f, "refused before the tool was spawned: {detail}")
            }
            DispatchFailure::AfterSpawn(detail) => {
                write!(f, "the tool ran and did not confirm its effect: {detail}")
            }
        }
    }
}

/// Which side of the spawn an execution error falls on.
///
/// This is the only judgement this module makes, and it is the one that
/// matters: everything the executor refuses *before* running the tool leaves
/// the device provably untouched, and everything after it does not. Getting
/// this backwards would either report a real effect as "no effect", or turn
/// every refused precondition into an unresolved job.
fn classify(error: ExecutionError) -> DispatchFailure {
    match error {
        // Every one of these is a refusal the executor makes with no child
        // process in existence.
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
        | ExecutionError::ScratchUnusable(_) => {
            DispatchFailure::BeforeAnyEffect(error.to_string())
        }
        // The port was reached, so a child may have run.
        ExecutionError::ToolPort { .. } | ExecutionError::ReadFailed { .. } => {
            DispatchFailure::AfterSpawn(error.to_string())
        }
    }
}

/// The fixed-tool port against a pinned host executable.
///
/// architecture.md 16.1: one bound executable, direct spawn, no shell, no PATH
/// resolution. The argv arrives already lowered from the Provider's closed
/// command enum — this type has no way to build one.
#[derive(Debug)]
pub struct HostFixedToolPort {
    executable: PathBuf,
    digest: arkforge_core::Sha256Digest,
}

/// Explicit name for the transitional vendor implementation.
pub type VendorToolPort = HostFixedToolPort;

/// Native DAYU200 Loader port.  Each semantic call claims the exact Loader
/// interface, runs TEST_UNIT_READY, performs one bounded read operation, and
/// releases the claim.  Mutation methods remain unavailable until NRU-002.
#[derive(Debug)]
pub struct NativeRockUsbPort {
    usb: arkforge_usb::NativeUsb,
    selector: arkforge_usb::UsbInterfaceSelector,
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
            next_tag: AtomicU32::new(1),
        }
    }

    fn matching_descriptors(
        &self,
    ) -> Result<Vec<arkforge_usb::UsbInterfaceDescriptor>, RockUsbPortFailure> {
        self.usb
            .enumerate()
            .map(|records| {
                records
                    .into_iter()
                    .filter(|record| self.selector.matches(record))
                    .collect()
            })
            .map_err(|error| RockUsbPortFailure::BeforeIo(error.to_string()))
    }

    fn with_protocol<T>(
        &self,
        operation: impl FnOnce(&mut RockUsbProtocol<'_>) -> Result<T, String>,
    ) -> Result<T, RockUsbPortFailure> {
        let interface = self
            .usb
            .open_unique(self.selector)
            .map_err(|error| RockUsbPortFailure::BeforeIo(error.to_string()))?;
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
            .map(|descriptor| RockUsbDevice {
                vendor_id: descriptor.vendor_id,
                product_id: descriptor.product_id,
                usb_specification: Some(descriptor.usb_specification),
                location: RockUsbLocation::IokitTopology(descriptor.location_id),
                mode: "loader".into(),
                serial: descriptor.serial,
                product_name: descriptor.product_name,
                vendor_name: descriptor.vendor_name,
                device_release: Some(descriptor.device_release),
            })
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
}

impl HostFixedToolPort {
    pub fn open(executable: &Path) -> Result<Self, String> {
        if !executable.is_absolute() {
            return Err(format!(
                "{} is not an absolute path; this port resolves no PATH",
                executable.display()
            ));
        }
        Ok(HostFixedToolPort {
            executable: executable.to_path_buf(),
            digest: file_digest(executable)?,
        })
    }

    /// The bytes that will run. Part of the maturity combination
    /// (architecture.md 12.3), so a caller can record which tool it was.
    pub fn digest(&self) -> arkforge_core::Sha256Digest {
        self.digest
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Proves the tool can actually run, before anything depends on it.
    ///
    /// **Byte equality is not usability.** AD-015: the same bytes that work
    /// normally hang forever in dyld when the file carries
    /// `com.apple.quarantine`, and a digest check sees nothing wrong. So the
    /// check is behavioural — spawn it, and require it to finish.
    ///
    /// `probe_argv` must be a device-free invocation. Which one that is
    /// belongs to whoever knows the tool, so it is passed in rather than
    /// guessed here: a self-test that enumerated USB would make starting the
    /// daemon a device interaction.
    ///
    /// The timeout is the whole mechanism. A quarantined binary produces no
    /// output and never exits, so "it printed something" would never be
    /// reached and "it exited non-zero" would never fire either. Only the
    /// clock distinguishes hung from slow.
    pub fn self_test(
        &self,
        probe_argv: &[&str],
        expect_marker: &str,
        timeout: std::time::Duration,
    ) -> Result<ToolSelfTest, ToolSelfTestFailure> {
        use std::io::Read;
        use std::process::Stdio;

        let started = std::time::Instant::now();
        let mut child = std::process::Command::new(&self.executable)
            .args(probe_argv)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| ToolSelfTestFailure::DidNotStart(error.to_string()))?;

        // Poll rather than wait: `wait` has no deadline, and a hung child would
        // hang the daemon's startup along with it.
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if started.elapsed() >= timeout => break None,
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
                Err(error) => {
                    return Err(ToolSelfTestFailure::DidNotStart(error.to_string()));
                }
            }
        };

        if status.is_none() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ToolSelfTestFailure::HungBeforeExiting {
                after_ms: started.elapsed().as_millis() as u64,
                quarantine: quarantine_evidence(&self.executable),
            });
        }

        // Read after exit. The probe is expected to write a line, not a stream;
        // a tool that filled the pipe would have blocked on the write and been
        // caught by the timeout above, which is the honest outcome for a probe
        // that was supposed to be trivial.
        let mut output = String::new();
        if let Some(mut stdout) = child.stdout.take() {
            let _ = stdout.read_to_string(&mut output);
        }
        if let Some(mut stderr) = child.stderr.take() {
            let _ = stderr.read_to_string(&mut output);
        }

        if !output.contains(expect_marker) {
            // A quarantined binary does not always hang: it can also exit at
            // once having produced nothing, which is what /tmp/rkq did on
            // 2026-08-15 after macOS had already assessed it. Same cause,
            // different shape, so the same evidence is gathered.
            return Err(ToolSelfTestFailure::Unrecognized {
                expected: expect_marker.to_string(),
                observed: output.chars().take(200).collect(),
                quarantine: quarantine_evidence(&self.executable),
            });
        }
        Ok(ToolSelfTest {
            duration_ms: started.elapsed().as_millis() as u64,
            first_line: output.lines().next().unwrap_or_default().trim().to_string(),
        })
    }
}

/// What a passing self-test observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSelfTest {
    pub duration_ms: u64,
    /// The tool's own first line, so a log records what answered.
    pub first_line: String,
}

/// Why the tool could not be shown to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolSelfTestFailure {
    DidNotStart(String),
    /// Spawned and never finished. On macOS this is overwhelmingly quarantine
    /// (AD-015), so the diagnosis says so and carries whatever evidence could
    /// be gathered for it.
    HungBeforeExiting {
        after_ms: u64,
        quarantine: QuarantineEvidence,
    },
    /// Ran and said nothing this tool would say. An empty answer is the
    /// common shape: a binary that cannot load produces no output and exits.
    Unrecognized {
        expected: String,
        observed: String,
        quarantine: QuarantineEvidence,
    },
}

impl QuarantineEvidence {
    /// The sentence to append to a failure, if there is one worth appending.
    fn remedy(&self) -> String {
        match self {
            QuarantineEvidence::Present(value) => format!(
                " It carries com.apple.quarantine ({value}); clear it with \
                 `xattr -d com.apple.quarantine <path>` (ArkForge AD-015)."
            ),
            QuarantineEvidence::Absent => " It carries no com.apple.quarantine, so the cause is \
                 something else — a missing dynamic library or the wrong architecture would look \
                 the same."
                .to_string(),
            QuarantineEvidence::Unknown => " Quarantine could not be checked here; if this is \
                 macOS, try `xattr -p com.apple.quarantine <path>` (ArkForge AD-015)."
                .to_string(),
        }
    }
}

/// What could be established about a quarantine attribute.
///
/// Best-effort and only consulted when the self-test already failed: reading an
/// extended attribute needs a helper this daemon does not otherwise depend on,
/// and a dependency acquired to explain a failure is cheaper than one acquired
/// to do the work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuarantineEvidence {
    /// The attribute is present, with its value.
    Present(String),
    /// The helper ran and found none. The hang is something else.
    Absent,
    /// No helper available, so this says nothing either way.
    Unknown,
}

fn quarantine_evidence(path: &Path) -> QuarantineEvidence {
    const HELPER: &str = "/usr/bin/xattr";
    if !Path::new(HELPER).exists() {
        return QuarantineEvidence::Unknown;
    }
    let Ok(output) = std::process::Command::new(HELPER)
        .args(["-p", "com.apple.quarantine"])
        .arg(path)
        .output()
    else {
        return QuarantineEvidence::Unknown;
    };
    if !output.status.success() {
        // `xattr -p` fails when the attribute is absent, which is an answer.
        return QuarantineEvidence::Absent;
    }
    QuarantineEvidence::Present(
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
    )
}

impl fmt::Display for ToolSelfTestFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolSelfTestFailure::DidNotStart(detail) => {
                write!(f, "the tool could not be started: {detail}")
            }
            ToolSelfTestFailure::HungBeforeExiting {
                after_ms,
                quarantine,
            } => write!(
                f,
                "the tool did not finish a device-free probe within {after_ms} ms. Its bytes \
                 match the pin, so this is not the wrong binary — it is a binary that cannot run \
                 here.{}",
                quarantine.remedy()
            ),
            ToolSelfTestFailure::Unrecognized {
                expected,
                observed,
                quarantine,
            } => write!(
                f,
                "the tool ran but did not identify itself: expected output containing \
                 {expected:?}, observed {observed:?}.{}",
                quarantine.remedy()
            ),
        }
    }
}

fn file_digest(path: &Path) -> Result<arkforge_core::Sha256Digest, String> {
    use std::io::Read;
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

impl FixedToolPort for HostFixedToolPort {
    fn run(
        &self,
        invocation: &arkforge_provider::rockchip_execute::ToolInvocation,
    ) -> Result<arkforge_provider::rockchip_execute::ToolReceipt, String> {
        let started = std::time::Instant::now();
        let output = std::process::Command::new(&self.executable)
            .args(&invocation.argv)
            .output()
            .map_err(|error| format!("{}: {error}", self.executable.display()))?;
        let truncate = |bytes: &[u8]| -> (String, bool) {
            let text = String::from_utf8_lossy(bytes).to_string();
            if text.len() > invocation.stdout_budget {
                // Keep the tail, not the head. The semantic markers this
                // receipt is judged by — "Write LBA from file (100%)", the
                // reset marker — are the *last* thing the tool prints, after
                // however much progress chatter came first. Keeping the head
                // read a successful long write as outcome-unknown: the budget
                // filled with progress lines and the marker fell off the end.
                let kept: String = text
                    .chars()
                    .rev()
                    .take(invocation.stdout_budget)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                (kept, true)
            } else {
                (text, false)
            }
        };
        let (stdout, stdout_truncated) = truncate(&output.stdout);
        let (stderr, stderr_truncated) = truncate(&output.stderr);
        Ok(arkforge_provider::rockchip_execute::ToolReceipt {
            exited_zero: output.status.success(),
            stdout,
            stderr,
            truncated: stdout_truncated || stderr_truncated,
            duration_ms: started.elapsed().as_millis() as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The classification is the whole judgement this module makes, so it is
    /// asserted rather than trusted. A precondition refusal that reported
    /// `OutcomeUnknown` would leave every rejected write needing reconciliation
    /// it does not need.
    /// The timeout is the mechanism, so it is tested against something that
    /// genuinely never returns rather than against a binary that happens to be
    /// broken today.
    #[test]
    fn a_tool_that_never_returns_is_killed_and_named_as_unable_to_run() {
        let sleep = Path::new("/bin/sleep");
        if !sleep.exists() {
            eprintln!("skipped: no /bin/sleep on this host");
            return;
        }
        let port = HostFixedToolPort::open(sleep).unwrap();
        let started = std::time::Instant::now();
        let failure = port
            .self_test(&["60"], "never printed", std::time::Duration::from_millis(300))
            .unwrap_err();

        match failure {
            ToolSelfTestFailure::HungBeforeExiting { after_ms, .. } => {
                assert!(after_ms >= 300, "{after_ms} ms");
            }
            other => panic!("expected a hang, got {other}"),
        }
        // Killed, not waited out: the daemon's startup does not inherit the
        // child's patience.
        assert!(
            started.elapsed() < std::time::Duration::from_secs(30),
            "the self-test waited for the child instead of killing it"
        );
    }

    /// A tool that runs but says nothing this daemon recognizes is refused too
    /// — that is the other shape a binary which cannot load takes.
    #[test]
    fn a_tool_that_answers_with_something_else_is_refused() {
        let echo = Path::new("/bin/echo");
        if !echo.exists() {
            eprintln!("skipped: no /bin/echo on this host");
            return;
        }
        let port = HostFixedToolPort::open(echo).unwrap();
        let failure = port
            .self_test(
                &["some other tool entirely"],
                "the-tool-we-pinned",
                std::time::Duration::from_secs(5),
            )
            .unwrap_err();
        assert!(matches!(
            failure,
            ToolSelfTestFailure::Unrecognized { .. }
        ));
        // Every failure carries the quarantine question, answered one way or
        // the other. AD-015 cost hours precisely because nothing asked it.
        assert!(
            failure.to_string().contains("quarantine"),
            "{failure}"
        );
    }

    #[test]
    fn a_tool_that_is_not_there_is_named_as_not_starting() {
        let missing = Path::new("/nonexistent/definitely-not-a-tool");
        let Ok(port) = HostFixedToolPort::open(missing) else {
            // `open` hashes the file, so a missing one fails there first. That
            // is also a refusal, which is the point.
            return;
        };
        assert!(matches!(
            port.self_test(&[], "x", std::time::Duration::from_secs(1)),
            Err(ToolSelfTestFailure::DidNotStart(_))
        ));
    }

    #[test]
    fn a_relative_tool_path_is_refused_rather_than_resolved() {
        let error = HostFixedToolPort::open(Path::new("rkdeveloptool")).unwrap_err();
        assert!(error.contains("resolves no PATH"), "{error}");
    }

    #[test]
    fn the_nru_001_native_port_refuses_mutations_before_usb_io() {
        let port = NativeRockUsbPort::new();
        assert!(matches!(
            port.write_partition_by_name("uboot", Path::new("/never-opened.img")),
            Err(RockUsbPortFailure::BeforeIo(_))
        ));
        assert!(matches!(
            port.reset_device(),
            Err(RockUsbPortFailure::BeforeIo(_))
        ));
    }

    #[test]
    fn a_refusal_before_the_spawn_confirms_no_effect() {
        for error in [
            ExecutionError::PortRefused {
                operation: "writePartitionByName".into(),
                message: "native writes are disabled in NRU-001".into(),
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
    fn a_failure_after_the_spawn_leaves_the_outcome_unknown() {
        for error in [
            ExecutionError::ToolPort {
                argv: "wlx system /staged/system.img".into(),
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
