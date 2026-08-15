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
    execute_action, ExecutionError, ExecutionSession, FixedToolPort, StagedImage, StoredAction,
};
use core::fmt;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Runs private actions for one daemon's jobs.
#[derive(Debug)]
pub struct Dispatcher<'a> {
    store_root: PathBuf,
    work_root: PathBuf,
    port: &'a dyn FixedToolPort,
    sessions: BTreeMap<String, ExecutionSession>,
    /// Jobs whose images are already on disk, so staging happens once.
    staged: BTreeSet<String>,
}

impl<'a> Dispatcher<'a> {
    pub fn new(store_root: impl Into<PathBuf>, work_root: impl Into<PathBuf>, port: &'a dyn FixedToolPort) -> Self {
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
                (text.chars().take(invocation.stdout_budget).collect(), true)
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
    #[test]
    fn a_relative_tool_path_is_refused_rather_than_resolved() {
        let error = HostFixedToolPort::open(Path::new("rkdeveloptool")).unwrap_err();
        assert!(error.contains("resolves no PATH"), "{error}");
    }

    #[test]
    fn a_refusal_before_the_spawn_confirms_no_effect() {
        for error in [
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
