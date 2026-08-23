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
use arkforge_core::Sha256Digest;
use arkforge_core::digest::Sha256;
use arkforge_core::effect::PersistentEffect;
use arkforge_core::outcome::ActionDisposition;
use arkforge_core::projection::PrivateActionRecord;
use arkforge_core::step::WorkflowEffect;
use arkforge_core::verification::VerificationOutcome;
use arkforge_engine::superseding::EffectObservation;
use arkforge_platform::sync_directory;
use arkforge_provider::rockchip_execute::{
    ExecutionError, ExecutionSession, RockUsbDevice, RockUsbLocation, RockUsbMutationReceipt,
    RockUsbObservation, RockUsbPort, RockUsbPortFailure, RockUsbWriteProgress, StagedImage,
    StoredAction, ValidatedImage, execute_action,
};
use arkforge_provider::rockusb_protocol::{
    LOGICAL_BLOCK_BYTES, ROCKUSB_TRANSFER_CHUNK_SECTORS, RockUsbBulkIo, RockUsbProtocol,
};
use core::fmt;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Instant, SystemTime};

const STAGING_CACHE_DIR: &str = "cache";

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExpectedImage {
    size_bytes: u64,
    sha256: Sha256Digest,
}

#[derive(Debug)]
struct StagingUse {
    images: BTreeMap<String, StagedImage>,
    bytes: u64,
    source: &'static str,
    cache_hit: bool,
}

#[derive(Debug, Clone)]
pub struct PendingPreparation {
    pub job_id: String,
    pub plan_actions: Vec<arkforge_core::projection::PrivateActionRecord>,
    pub profile: arkforge_core::profile::DeviceProfile,
    pub artifact_digest: Sha256Digest,
}

/// A Provider-selected reconciliation run. Every action is revalidated by the
/// dispatcher as read-only before it can reach the native port.
#[derive(Debug, Clone)]
pub struct PendingReconcile {
    pub job_id: String,
    pub actions: Vec<PrivateActionRecord>,
    pub profile: arkforge_core::profile::DeviceProfile,
    pub possible_effects: Vec<PersistentEffect>,
    /// True only when these observations cover the original plan's complete
    /// terminal contract, not merely the interrupted effect.
    pub terminal_scope: bool,
}

/// Read-only evidence returned to the engine's reconciliation reducer.
#[derive(Debug, Clone)]
pub struct ReconcileDispatchOutcome {
    pub observations: Vec<(PersistentEffect, EffectObservation)>,
    pub facts: Vec<(String, String)>,
    pub evidence_digest: Sha256Digest,
}

#[derive(Debug, Clone, Copy)]
struct StagingStats {
    duration_ms: u64,
    bytes: u64,
    source: &'static str,
    cache_hit: bool,
    ready_at: Instant,
    preparation_mode: &'static str,
}

/// Runs private actions for one daemon's jobs.
#[derive(Debug)]
pub struct Dispatcher<'a> {
    store_root: PathBuf,
    work_root: PathBuf,
    port: &'a dyn RockUsbPort,
    sessions: BTreeMap<String, ExecutionSession>,
    /// Jobs whose images are already on disk, so staging happens once.
    staged: BTreeSet<String>,
    staging_stats: BTreeMap<String, StagingStats>,
    preparation_failures: BTreeMap<String, DispatchFailure>,
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
            staging_stats: BTreeMap::new(),
            preparation_failures: BTreeMap::new(),
        }
    }

    /// Starts host-only staging and full image validation as soon as the job
    /// exists. This runs concurrently with the authority's HDC Loader switch;
    /// no device I/O or permit is involved in preparation.
    pub fn prepare(&mut self, work: &PendingPreparation) {
        if self.staged.contains(&work.job_id)
            || self.preparation_failures.contains_key(&work.job_id)
        {
            return;
        }
        match self.stage_job(
            &work.job_id,
            work.artifact_digest,
            &work.plan_actions,
            &work.profile,
            "job-start-prewarm",
        ) {
            Ok(stats) => {
                self.staging_stats.insert(work.job_id.clone(), stats);
            }
            Err(failure) => {
                self.preparation_failures
                    .insert(work.job_id.clone(), failure);
            }
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

    /// Performs a fresh, read-only reconciliation outside the service lock.
    ///
    /// This has no permit and accepts no write/reset/managed-control action.
    /// A malformed Provider plan therefore becomes indeterminate evidence,
    /// never a broader native operation.
    pub fn reconcile_read_only(&mut self, work: &PendingReconcile) -> ReconcileDispatchOutcome {
        match self.try_reconcile_read_only(work) {
            Ok(outcome) => outcome,
            Err(detail) => ReconcileDispatchOutcome {
                observations: work
                    .possible_effects
                    .iter()
                    .cloned()
                    .map(|effect| (effect, EffectObservation::Indeterminate))
                    .collect(),
                facts: vec![
                    ("reconcileError".into(), detail.clone()),
                    ("readOnly".into(), "true".into()),
                ],
                evidence_digest: arkforge_core::digest::sha256(detail.as_bytes()),
            },
        }
    }

    fn try_reconcile_read_only(
        &mut self,
        work: &PendingReconcile,
    ) -> Result<ReconcileDispatchOutcome, String> {
        let scratch = self.job_root(&work.job_id).join("reconcile-scratch");
        std::fs::create_dir_all(&scratch).map_err(|error| error.to_string())?;

        // Reconciliation observations must be fresh. No table/read-domain fact
        // retained from the interrupted execution is trusted as a new read.
        self.sessions
            .insert(work.job_id.clone(), ExecutionSession::new(BTreeMap::new()));
        let session = self
            .sessions
            .get_mut(&work.job_id)
            .expect("the fresh reconciliation session was inserted");

        let mut verification = Vec::<(PrivateActionRecord, VerificationOutcome)>::new();
        let mut evidence = Sha256::new();
        evidence.update(b"arkforge-reconcile-read-only-v1\0");
        for record in &work.actions {
            if record.effect_class != WorkflowEffect::ReadOnly {
                return Err(format!(
                    "provider selected effectful reconcile action {} ({})",
                    record.action_id,
                    record.effect_class.as_str()
                ));
            }
            let action = StoredAction::decode(record).map_err(|error| error.to_string())?;
            if !matches!(
                action,
                StoredAction::ProbeLoader
                    | StoredAction::ValidatePartitionTable { .. }
                    | StoredAction::CharacterizeReadDomain
                    | StoredAction::ReadbackPartition { .. }
            ) {
                return Err(format!(
                    "provider selected non-read-only native action {}",
                    record.action_id
                ));
            }
            let outcome =
                execute_action(&action, record, session, &work.profile, self.port, &scratch)
                    .map_err(|error| error.to_string())?;
            if outcome.disposition != ActionDisposition::SemanticSuccess {
                return Err(format!(
                    "read-only action {} did not report semantic success",
                    record.action_id
                ));
            }
            evidence.update(outcome.evidence_digest.as_bytes());
            if let Some(verdict) = outcome.verification {
                verification.push((record.clone(), verdict));
            }
        }

        let observations = work
            .possible_effects
            .iter()
            .cloned()
            .map(|effect| {
                let observation = verification
                    .iter()
                    .find(|(record, _)| readback_observes_effect(record, &effect))
                    .map(|(_, outcome)| match outcome {
                        VerificationOutcome::Verified { .. } => EffectObservation::Present,
                        // A mismatch proves the desired complete content is not
                        // present, but without a pre-write baseline it cannot
                        // distinguish no write from a partial write.
                        VerificationOutcome::TypedSkip { .. }
                        | VerificationOutcome::Failed { .. } => EffectObservation::Indeterminate,
                    })
                    .unwrap_or(EffectObservation::Indeterminate);
                (effect, observation)
            })
            .collect::<Vec<_>>();
        let indeterminate = observations
            .iter()
            .filter(|(_, observation)| *observation == EffectObservation::Indeterminate)
            .count();
        Ok(ReconcileDispatchOutcome {
            observations,
            facts: vec![
                ("readOnly".into(), "true".into()),
                ("readOnlyActionCount".into(), work.actions.len().to_string()),
                (
                    "readOnlyObservationCount".into(),
                    work.possible_effects.len().to_string(),
                ),
                ("indeterminateCount".into(), indeterminate.to_string()),
            ],
            evidence_digest: evidence.finalize(),
        })
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
        let is_write = decoded
            .iter()
            .any(|action| matches!(action, StoredAction::WritePartition { .. }));
        if is_write && let Some(failure) = self.preparation_failures.remove(&work.job_id) {
            return Err(failure);
        }
        let needs_staging = is_write && !self.staged.contains(&work.job_id);
        let staging = if needs_staging {
            Some(self.stage_if_needed(work)?)
        } else if is_write {
            self.staging_stats.remove(&work.job_id)
        } else {
            None
        };

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
        let mut facts: Vec<(String, String)> = outcome
            .facts
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect();
        if let Some(staging) = staging {
            facts.push(("stagingDurationMs".into(), staging.duration_ms.to_string()));
            facts.push(("stagingBytes".into(), staging.bytes.to_string()));
            facts.push(("stagingSource".into(), staging.source.into()));
            facts.push(("stagingCacheHit".into(), staging.cache_hit.to_string()));
            facts.push((
                "preparationLeadMs".into(),
                staging.ready_at.elapsed().as_millis().to_string(),
            ));
            facts.push(("preparationMode".into(), staging.preparation_mode.into()));
        }
        Ok(DispatchOutcome {
            disposition: outcome.disposition,
            facts,
            evidence_digest: outcome.evidence_digest,
            verification: outcome.verification,
        })
    }

    /// Extracts the images this job's writes need, once.
    fn stage_if_needed(&mut self, work: &PendingDispatch) -> Result<StagingStats, DispatchFailure> {
        self.stage_job(
            &work.job_id,
            work.artifact_digest,
            &work.plan_actions,
            &work.profile,
            "first-write-fallback",
        )
    }

    fn stage_job(
        &mut self,
        job_id: &str,
        artifact_digest: Sha256Digest,
        plan_actions: &[arkforge_core::projection::PrivateActionRecord],
        profile: &arkforge_core::profile::DeviceProfile,
        preparation_mode: &'static str,
    ) -> Result<StagingStats, DispatchFailure> {
        if self.staged.contains(job_id) {
            return Err(DispatchFailure::BeforeAnyEffect(format!(
                "{} was marked staged without a staging request",
                job_id
            )));
        }
        let started = Instant::now();
        let expected = expected_images(plan_actions, profile)?;
        let staging = materialize_staging_cache(
            &self.store_root,
            &self.work_root.join(STAGING_CACHE_DIR),
            artifact_digest,
            &expected,
        )?;

        let session = self
            .sessions
            .entry(job_id.to_string())
            .or_insert_with(|| ExecutionSession::new(BTreeMap::new()));
        for (name, image) in &staging.images {
            session.stage(name.clone(), image.clone());
        }
        session.begin_parallel_staged_validation();
        self.staged.insert(job_id.to_string());
        Ok(StagingStats {
            duration_ms: started.elapsed().as_millis() as u64,
            bytes: staging.bytes,
            source: staging.source,
            cache_hit: staging.cache_hit,
            ready_at: Instant::now(),
            preparation_mode,
        })
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
        self.staging_stats.remove(job_id);
        self.preparation_failures.remove(job_id);
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

fn readback_observes_effect(record: &PrivateActionRecord, effect: &PersistentEffect) -> bool {
    match (effect, &record.declared_target) {
        (
            PersistentEffect::WritePartition {
                partition,
                range,
                content,
            },
            Some(arkforge_core::step::SemanticTarget::Partition(observed_partition)),
        ) => {
            observed_partition == partition
                && record.declared_range == Some(*range)
                && record.content_digest == Some(*content)
        }
        _ => false,
    }
}

fn expected_images(
    plan_actions: &[arkforge_core::projection::PrivateActionRecord],
    profile: &arkforge_core::profile::DeviceProfile,
) -> Result<BTreeMap<String, ExpectedImage>, DispatchFailure> {
    let mut expected = BTreeMap::new();
    for record in plan_actions {
        let action = StoredAction::decode(record)
            .map_err(|error| DispatchFailure::BeforeAnyEffect(error.to_string()))?;
        let StoredAction::WritePartition { member, .. } = action else {
            continue;
        };
        let range = record.declared_range.ok_or_else(|| {
            DispatchFailure::BeforeAnyEffect(format!(
                "write input {member} has no declared byte range"
            ))
        })?;
        let digest = record.content_digest.ok_or_else(|| {
            DispatchFailure::BeforeAnyEffect(format!(
                "write input {member} has no declared content digest"
            ))
        })?;
        let image = ExpectedImage {
            size_bytes: range.length,
            sha256: digest,
        };
        if let Some(prior) = expected.insert(member.clone(), image.clone())
            && prior != image
        {
            return Err(DispatchFailure::BeforeAnyEffect(format!(
                "write input {member} has contradictory declarations"
            )));
        }
    }
    let profile_members: BTreeSet<String> = profile
        .allowed_targets
        .iter()
        .filter_map(|target| target.source_member.clone())
        .collect();
    let plan_members: BTreeSet<String> = expected.keys().cloned().collect();
    if expected.is_empty() || plan_members != profile_members {
        return Err(DispatchFailure::BeforeAnyEffect(format!(
            "validated plan write inputs {plan_members:?} disagree with profile inputs \
             {profile_members:?}"
        )));
    }
    Ok(expected)
}

fn materialize_staging_cache(
    store_root: &Path,
    cache_root: &Path,
    artifact_digest: Sha256Digest,
    expected: &BTreeMap<String, ExpectedImage>,
) -> Result<StagingUse, DispatchFailure> {
    create_cache_directory(cache_root)?;
    let key = staging_cache_key(artifact_digest, expected);
    let directory = cache_root.join(&key);
    if directory.exists() {
        return load_staging_cache(&directory, expected, true);
    }

    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let temporary = cache_root.join(format!(".{key}-{}-{nonce}.part", std::process::id()));
    fs::create_dir(&temporary).map_err(cache_io)?;
    set_cache_permissions(&temporary, 0o700)?;

    let build = (|| -> Result<(), DispatchFailure> {
        let store = ContentAddressedStore::open(store_root, CasQuota::dayu200_default())
            .map_err(|error| DispatchFailure::BeforeAnyEffect(error.to_string()))?;
        let manifest = dayu200::inspect(
            store
                .open_object(&artifact_digest)
                .map_err(|error| DispatchFailure::BeforeAnyEffect(error.to_string()))?,
        )
        .map_err(|error| DispatchFailure::BeforeAnyEffect(error.to_string()))?;
        let wanted: BTreeSet<String> = expected.keys().cloned().collect();
        let report = stage_members(
            store
                .open_object(&artifact_digest)
                .map_err(|error| DispatchFailure::BeforeAnyEffect(error.to_string()))?,
            &manifest,
            &wanted,
            &temporary,
        )
        .map_err(|error| DispatchFailure::BeforeAnyEffect(error.to_string()))?;
        if report.members.len() != expected.len() {
            return Err(DispatchFailure::BeforeAnyEffect(format!(
                "staging cache produced {} inputs, expected {}",
                report.members.len(),
                expected.len()
            )));
        }
        for (name, required) in expected {
            let staged = report.members.get(name).ok_or_else(|| {
                DispatchFailure::BeforeAnyEffect(format!(
                    "staging cache did not produce required input {name}"
                ))
            })?;
            if staged.size_bytes != required.size_bytes || staged.sha256 != required.sha256 {
                return Err(DispatchFailure::BeforeAnyEffect(format!(
                    "staged input {name} is {} bytes hashing to {}; the validated plan requires \
                     {} bytes hashing to {}",
                    staged.size_bytes, staged.sha256, required.size_bytes, required.sha256
                )));
            }
            set_cache_permissions(&staged.path, 0o400)?;
        }
        sync_cache_directory(&temporary)?;
        set_cache_permissions(&temporary, 0o500)?;
        Ok(())
    })();
    if let Err(error) = build {
        let _ = make_cache_removable(&temporary);
        let _ = fs::remove_dir_all(&temporary);
        return Err(error);
    }

    match fs::rename(&temporary, &directory) {
        Ok(()) => sync_cache_directory(cache_root)?,
        Err(error) if directory.exists() => {
            let _ = make_cache_removable(&temporary);
            let _ = fs::remove_dir_all(&temporary);
            if !directory.is_dir() {
                return Err(cache_io(error));
            }
        }
        Err(error) => {
            let _ = make_cache_removable(&temporary);
            let _ = fs::remove_dir_all(&temporary);
            return Err(cache_io(error));
        }
    }
    load_staging_cache(&directory, expected, false)
}

fn load_staging_cache(
    directory: &Path,
    expected: &BTreeMap<String, ExpectedImage>,
    cache_hit: bool,
) -> Result<StagingUse, DispatchFailure> {
    if !directory.is_dir() {
        return Err(DispatchFailure::BeforeAnyEffect(format!(
            "staging cache {} is not a directory",
            directory.display()
        )));
    }
    let observed: BTreeSet<String> = fs::read_dir(directory)
        .map_err(cache_io)?
        .map(|entry| {
            entry.map_err(cache_io).and_then(|entry| {
                entry.file_name().into_string().map_err(|_| {
                    DispatchFailure::BeforeAnyEffect(format!(
                        "staging cache {} contains a non-UTF-8 name",
                        directory.display()
                    ))
                })
            })
        })
        .collect::<Result<_, _>>()?;
    let wanted: BTreeSet<String> = expected.keys().cloned().collect();
    if observed != wanted {
        return Err(DispatchFailure::BeforeAnyEffect(format!(
            "staging cache entries {observed:?} disagree with required inputs {wanted:?}"
        )));
    }

    let mut images = BTreeMap::new();
    let mut bytes = 0u64;
    for (name, required) in expected {
        let name_path = Path::new(name);
        if name_path.file_name().and_then(|value| value.to_str()) != Some(name) {
            return Err(DispatchFailure::BeforeAnyEffect(format!(
                "staging input name {name:?} is not one plain path component"
            )));
        }
        let path = directory.join(name);
        let metadata = fs::symlink_metadata(&path).map_err(cache_io)?;
        if !metadata.file_type().is_file() || metadata.len() != required.size_bytes {
            return Err(DispatchFailure::BeforeAnyEffect(format!(
                "cached input {name} is not a regular {}-byte file",
                required.size_bytes
            )));
        }
        ensure_cache_read_only(&path, &metadata)?;
        bytes = bytes.checked_add(required.size_bytes).ok_or_else(|| {
            DispatchFailure::BeforeAnyEffect("staging input byte count overflows u64".into())
        })?;
        images.insert(
            name.clone(),
            StagedImage {
                member: name.clone(),
                path,
                size_bytes: required.size_bytes,
                sha256: required.sha256,
            },
        );
    }
    Ok(StagingUse {
        images,
        bytes,
        source: if cache_hit {
            "content-addressed-cache"
        } else {
            "artifact-extraction"
        },
        cache_hit,
    })
}

fn staging_cache_key(
    artifact_digest: Sha256Digest,
    expected: &BTreeMap<String, ExpectedImage>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"arkforge-staging-cache-v1\0");
    hasher.update(artifact_digest.to_hex().as_bytes());
    hasher.update(b"\0");
    for (name, image) in expected {
        hasher.update(name.as_bytes());
        hasher.update(b"\0");
        hasher.update(&image.size_bytes.to_be_bytes());
        hasher.update(image.sha256.to_hex().as_bytes());
        hasher.update(b"\0");
    }
    hasher.finalize().to_hex()
}

fn create_cache_directory(path: &Path) -> Result<(), DispatchFailure> {
    fs::create_dir_all(path).map_err(cache_io)?;
    set_cache_permissions(path, 0o700)
}

fn sync_cache_directory(path: &Path) -> Result<(), DispatchFailure> {
    sync_directory(path).map_err(cache_io)
}

fn cache_io(error: std::io::Error) -> DispatchFailure {
    DispatchFailure::BeforeAnyEffect(format!("staging cache I/O failed: {error}"))
}

#[cfg(unix)]
fn set_cache_permissions(path: &Path, mode: u32) -> Result<(), DispatchFailure> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(cache_io)
}

#[cfg(not(unix))]
fn set_cache_permissions(_path: &Path, _mode: u32) -> Result<(), DispatchFailure> {
    Ok(())
}

#[cfg(unix)]
fn ensure_cache_read_only(path: &Path, metadata: &fs::Metadata) -> Result<(), DispatchFailure> {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o222 != 0 {
        return Err(DispatchFailure::BeforeAnyEffect(format!(
            "cached input {} remains writable",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_cache_read_only(_path: &Path, _metadata: &fs::Metadata) -> Result<(), DispatchFailure> {
    Ok(())
}

fn make_cache_removable(path: &Path) -> Result<(), DispatchFailure> {
    set_cache_permissions(path, 0o700)?;
    for entry in fs::read_dir(path).map_err(cache_io)? {
        let entry = entry.map_err(cache_io)?;
        set_cache_permissions(&entry.path(), 0o600)?;
    }
    Ok(())
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
        image: &mut ValidatedImage,
    ) -> Result<RockUsbMutationReceipt, RockUsbPortFailure> {
        if partition.is_empty() {
            return Err(RockUsbPortFailure::BeforeIo(
                "native WRITE_LBA target has an empty partition name".into(),
            ));
        }
        // Validation retained the exact open file description. Recheck its
        // inode fingerprint and rewind it before claiming USB; a pathname swap
        // or chmod/write now remains a confirmed-no-effect refusal.
        let staged = image.staged().clone();
        let file = image
            .prepare_for_write()
            .map_err(RockUsbPortFailure::BeforeIo)?;
        let total_bytes = staged.size_bytes;
        if total_bytes == 0 {
            return Err(RockUsbPortFailure::BeforeIo(format!(
                "{} is empty; refusing a zero-length WRITE_LBA",
                staged.path.display()
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
        let mut buffer = vec![0u8; ROCKUSB_TRANSFER_CHUNK_SECTORS as usize * LOGICAL_BLOCK_BYTES];
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
                    local_failure(format!("{}: {error}", staged.path.display()), chunks)
                })?;
                if read == 0 {
                    return Err(local_failure(
                        format!(
                            "{} became shorter while native WRITE_LBA was reading it",
                            staged.path.display()
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
            RockUsbPortFailure::AfterIo(format!("{}: {error}", staged.path.display()))
        })?;
        if extra_read != 0 {
            return Err(RockUsbPortFailure::AfterIo(format!(
                "{} grew while native WRITE_LBA was reading it",
                staged.path.display()
            )));
        }

        let payload_digest = hasher.finalize();
        if payload_digest != staged.sha256 {
            return Err(RockUsbPortFailure::AfterIo(format!(
                "native WRITE_LBA payload hashes to {payload_digest}; staged input is {}",
                staged.sha256
            )));
        }
        let progress = RockUsbWriteProgress {
            payload_bytes: total_bytes,
            wire_sectors: total_sectors,
            chunks,
            chunk_sectors: ROCKUSB_TRANSFER_CHUNK_SECTORS,
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
    use arkforge_artifact::fixture;
    use std::cell::Cell;

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "arkforge-dispatch-{label}-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            TempRoot(path)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            make_tree_removable(&self.0);
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Debug, Default)]
    struct NoMutationPort {
        reset_calls: Cell<usize>,
    }

    impl RockUsbPort for NoMutationPort {
        fn discover(&self) -> Result<RockUsbObservation<Vec<RockUsbDevice>>, RockUsbPortFailure> {
            Err(RockUsbPortFailure::BeforeIo("unused".into()))
        }

        fn read_partition_table(
            &self,
        ) -> Result<
            RockUsbObservation<arkforge_artifact::manifest::PartitionTableFact>,
            RockUsbPortFailure,
        > {
            Err(RockUsbPortFailure::BeforeIo("unused".into()))
        }

        fn read_sectors(
            &self,
            _begin_sector: u64,
            _sectors: u64,
            _scratch: &Path,
        ) -> Result<RockUsbObservation<Vec<u8>>, RockUsbPortFailure> {
            Err(RockUsbPortFailure::BeforeIo("unused".into()))
        }

        fn reset_device(&self) -> Result<RockUsbMutationReceipt, RockUsbPortFailure> {
            self.reset_calls.set(self.reset_calls.get() + 1);
            Err(RockUsbPortFailure::AfterIo(
                "a reconcile test reached reset".into(),
            ))
        }
    }

    fn make_tree_removable(path: &Path) {
        if let Ok(metadata) = fs::symlink_metadata(path) {
            if metadata.is_dir() {
                let _ = set_cache_permissions(path, 0o700);
                if let Ok(entries) = fs::read_dir(path) {
                    for entry in entries.flatten() {
                        make_tree_removable(&entry.path());
                    }
                }
            } else {
                let _ = set_cache_permissions(path, 0o600);
            }
        }
    }

    #[test]
    fn reconcile_revalidates_provider_output_before_any_mutation_port() {
        let root = TempRoot::new("reconcile-closed-actions");
        let port = NoMutationPort::default();
        let mut dispatcher = Dispatcher::new(root.0.join("store"), root.0.join("work"), &port);
        let record = PrivateActionRecord {
            action_id: arkforge_core::ActionId::new("ACT-MALICIOUS-RESET").unwrap(),
            step_id: arkforge_core::StepId::new("STEP-MALICIOUS-RESET").unwrap(),
            role: arkforge_core::projection::PrivateActionRole::PrimaryEffect,
            // Even a contradictory declaration cannot smuggle the decoded
            // reset operation through the read-only runner.
            effect_class: WorkflowEffect::ReadOnly,
            declared_target: Some(arkforge_core::SemanticTarget::Device),
            declared_range: None,
            content_digest: None,
            body: arkforge_core::CborValue::map(vec![(
                "action",
                arkforge_core::CborValue::text("reset-device"),
            )]),
        };
        let work = PendingReconcile {
            job_id: "JOB-RECONCILE-CLOSED".into(),
            actions: vec![record],
            profile: arkforge_core::profile::load(include_str!("../../../profiles/dayu200.yaml"))
                .unwrap(),
            possible_effects: Vec::new(),
            terminal_scope: false,
        };
        let outcome = dispatcher.reconcile_read_only(&work);
        assert!(
            outcome
                .facts
                .iter()
                .any(|(key, value)| key == "reconcileError" && value.contains("non-read-only"))
        );
        assert_eq!(port.reset_calls.get(), 0);
    }

    #[test]
    fn a_published_staging_cache_is_reused_and_still_revalidated_before_write() {
        let root = TempRoot::new("staging-cache");
        let store_root = root.0.join("store");
        let cache_root = root.0.join("work/cache");
        let archive = fixture::dayu200_archive();
        let store = ContentAddressedStore::open(&store_root, CasQuota::dayu200_default()).unwrap();
        let imported = store
            .import(archive.as_slice(), archive.len() as u64, None)
            .unwrap();
        let manifest = dayu200::inspect(archive.as_slice()).unwrap();
        let mut expected = BTreeMap::new();
        for name in ["uboot.img", "system.img"] {
            let member = manifest.member(name).unwrap();
            expected.insert(
                name.to_string(),
                ExpectedImage {
                    size_bytes: member.size_bytes,
                    sha256: member.sha256,
                },
            );
        }

        let first = materialize_staging_cache(&store_root, &cache_root, imported.digest, &expected)
            .unwrap();
        assert!(!first.cache_hit);
        assert_eq!(first.source, "artifact-extraction");
        for image in first.images.values() {
            image.revalidate().unwrap();
        }

        let second =
            materialize_staging_cache(&store_root, &cache_root, imported.digest, &expected)
                .unwrap();
        assert!(second.cache_hit);
        assert_eq!(second.source, "content-addressed-cache");
        assert_eq!(
            first
                .images
                .values()
                .map(|image| image.path.clone())
                .collect::<Vec<_>>(),
            second
                .images
                .values()
                .map(|image| image.path.clone())
                .collect::<Vec<_>>()
        );

        let image = second.images.get("uboot.img").unwrap();
        set_cache_permissions(&image.path, 0o600).unwrap();
        let mut corrupted = fs::read(&image.path).unwrap();
        corrupted[0] ^= 0xff;
        fs::write(&image.path, corrupted).unwrap();
        set_cache_permissions(&image.path, 0o400).unwrap();
        assert!(
            image.revalidate().is_err(),
            "a cache hit never bypasses the full pre-write hash"
        );
    }

    #[test]
    fn native_write_local_preconditions_are_checked_before_usb_io() {
        let port = NativeRockUsbPort::new();
        let missing = StagedImage {
            member: "uboot.img".into(),
            path: PathBuf::from("/never-opened.img"),
            size_bytes: 512,
            sha256: arkforge_core::digest::sha256(b"missing"),
        };
        assert!(matches!(
            missing.open_and_revalidate(),
            Err(ExecutionError::StagingChanged(_))
        ));
        let empty_path = std::env::temp_dir().join(format!(
            "arkforge-native-empty-write-{}",
            std::process::id()
        ));
        std::fs::write(&empty_path, []).unwrap();
        let mut empty = StagedImage {
            member: "uboot.img".into(),
            path: empty_path.clone(),
            size_bytes: 0,
            sha256: arkforge_core::digest::sha256(b""),
        }
        .open_and_revalidate()
        .unwrap();
        assert!(matches!(
            port.write_partition("uboot", 0x2000, &mut empty),
            Err(RockUsbPortFailure::BeforeIo(_))
        ));
        let _ = std::fs::remove_file(empty_path);
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
