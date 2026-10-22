//! Authority-owned maintenance orchestration. This module owns no endpoint,
//! subprocess, capability store or pairing secret. Only a trusted Runtime may
//! implement the ports; caller-provided facts must never implement these ports.
//!
//! See `docs/spec/ports/authority-maintenance.md`. The ports retain target exclusion
//! for the entire call, including verification, and durably consume every
//! restoration reservation. Neither process restart nor a returned error is
//! permission to replay a reservation.

use arkforge_core::{OpaqueId, Sha256Digest};
use arkforge_ipc::contract;
use arkforge_ipc::messages::{ActionReceiptSummary, Effect, ExecutablePlan, JobSummary};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

pub const RESTORATION_SHA256: &str =
    "bdd1f4eacbe545f5a553b529980bc487723d37554148058dd551c241abf86de4";
pub const RAMDISK_START: u64 = 121_634_816;
pub const RAMDISK_BYTES: u64 = 4 * 1024 * 1024;
const TARGETS: [&str; 9] = [
    "uboot",
    "resource",
    "boot_linux",
    "ramdisk",
    "system",
    "vendor",
    "updater",
    "chip_ckm",
    "userdata",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidInput,
    SourceRefused,
    IdentityChanged,
    BudgetExhausted,
    PersistenceFailed,
    AuthorityRefused(String),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

/// Digests come from the Runtime's durable source binding, not UI input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub serial_sha256: String,
    pub topology_sha256: String,
    pub descriptor_sha256: String,
}
impl Identity {
    fn valid(&self) -> bool {
        [
            &self.serial_sha256,
            &self.topology_sha256,
            &self.descriptor_sha256,
        ]
        .into_iter()
        .all(|s| Sha256Digest::parse_hex(s).is_ok())
    }
}

/// A correlated durable snapshot, read by the authority under target exclusion.
/// A mode transition needs an already-proven durable binding; discovery alone
/// must never create a new source identity.
#[derive(Debug, Clone)]
pub struct Source {
    pub authority_namespace: String,
    pub profile: String,
    pub campaign: String,
    pub job: JobSummary,
    pub plan: ExecutablePlan,
    pub receipts: Vec<ActionReceiptSummary>,
    pub identity: Identity,
    pub latest_job: String,
    pub active_jobs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOutcome {
    FullHash(String),
    MountedWritable,
    Failed(String),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetResult {
    pub effect: Effect,
    pub outcome: &'static str,
    pub observed_sha256: Option<String>,
    pub verification_strength: Option<&'static str>,
    pub reason: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationResult {
    pub source_job_id: String,
    pub source_plan_id: String,
    pub source_plan_sha256: String,
    pub targets: Vec<TargetResult>,
}
impl VerificationResult {
    pub fn verified(&self) -> u64 {
        self.count(contract::TARGET_VERDICT_VERIFIED)
    }
    pub fn skipped(&self) -> u64 {
        self.count(contract::TARGET_VERDICT_TYPED_SKIP)
    }
    pub fn failed(&self) -> u64 {
        self.count(contract::TARGET_VERDICT_FAILED)
    }
    fn count(&self, outcome: &str) -> u64 {
        self.targets.iter().filter(|r| r.outcome == outcome).count() as u64
    }
    pub fn exit_code(&self) -> i32 {
        contract::flash_verify_exit_code(self.failed(), self.skipped())
    }
    pub fn outcome(&self) -> &'static str {
        contract::flash_verify_outcome(self.failed(), self.skipped())
    }
}

/// All methods run under a Runtime-owned target lease. `read_partition` must
/// resolve a closed partition, check the sealed offset/length against current
/// block metadata, refuse holders, check mounts before and after reading, and
/// hash exactly that range. It must not accept shell, device paths or hashes
/// from a frontend. Returning FullHash for a short read is a port violation.
pub trait VerificationPort {
    fn load_source(&mut self, job: &str) -> Result<Source, Error>;
    fn fresh_identity(&mut self, source: &Source, deadline: Instant) -> Result<Identity, Error>;
    /// Persist and fsync a new immutable intent, including source, ranges,
    /// tool/identity/budget and unique record ID, before the first read.
    fn verification_intent(&mut self, source: &Source, budget_ms: u64) -> Result<(), Error>;
    fn read_partition(
        &mut self,
        source: &Source,
        effect: &Effect,
        deadline: Instant,
    ) -> Result<ReadOutcome, Error>;
    /// Publish a separate immutable flash-verification/v1 record. Old job
    /// outcomes, receipts and verification strengths must remain untouched.
    fn verification_result(&mut self, result: &VerificationResult) -> Result<(), Error>;
}

fn validate_request(job: &str, budget_ms: u64) -> Result<(), Error> {
    if OpaqueId::new(job).is_err() || !(1_000..=600_000).contains(&budget_ms) {
        return Err(Error::InvalidInput);
    }
    Ok(())
}
fn original_ramdisk(effect: &Effect) -> bool {
    effect.kind == "writePartition"
        && effect.target == "ramdisk"
        && effect.range_start == RAMDISK_START
        && effect.range_length == RAMDISK_BYTES
        && effect.content_sha256 == RESTORATION_SHA256
}
fn validate_source(source: &Source, requested: &str, restoring: bool) -> Result<(), Error> {
    let plan = &source.plan;
    let job = &source.job;
    let digest = Sha256Digest::parse_hex(&plan.plan_sha256).map_err(|_| Error::SourceRefused)?;
    let at_reboot = restoring
        && matches!(job.state.as_str(), "rebindWait" | "cancelledSafe")
        && job.completed_steps.checked_add(1) == Some(job.total_steps)
        && job.terminal == (job.state == "cancelledSafe");
    let complete =
        job.state == "succeeded" && job.terminal && job.completed_steps == job.total_steps;
    if source.authority_namespace != "arkdeck"
        || source.profile != "org.openharmony.dayu200@1.0.0"
        || !source.identity.valid()
        || source.latest_job != requested
        || job.job_id != requested
        || job.plan_id != plan.plan_id
        || job.plan_sha256 != digest.as_bytes()
        || plan.execution_purpose != "primaryFlash"
        || !(complete || at_reboot)
        || source.active_jobs != u64::from(!job.terminal)
        || job.total_steps != plan.public_steps.len() as u64
        || plan.public_steps.len() < 2
        || source.campaign.is_empty()
        || plan.authority_support_campaign != source.campaign
        || plan.mechanics_maturity_campaign != source.campaign
        || plan.public_steps[plan.public_steps.len() - 2].kind != "reboot"
        || plan
            .public_steps
            .last()
            .is_none_or(|s| s.kind != "postflightProbe")
        || source.receipts.len() != job.completed_steps as usize
    {
        return Err(Error::SourceRefused);
    }
    let mut steps = BTreeSet::new();
    for step in &plan.public_steps {
        if !steps.insert(&step.step_id) || OpaqueId::new(&step.step_id).is_err() {
            return Err(Error::SourceRefused);
        }
    }
    for step in plan.public_steps.iter().take(job.completed_steps as usize) {
        let receipts = source
            .receipts
            .iter()
            .filter(|r| r.step_id == step.step_id)
            .collect::<Vec<_>>();
        let [receipt] = receipts.as_slice() else {
            return Err(Error::SourceRefused);
        };
        if receipt.job_id != job.job_id
            || receipt.plan_id != job.plan_id
            || receipt.disposition != "semanticSuccess"
            || !receipt.failure_classification.is_empty()
            || receipt.receipt_digest().is_err()
        {
            return Err(Error::SourceRefused);
        }
    }
    let mut targets = BTreeSet::new();
    for effect in &plan.persistent_effects {
        if effect.kind != "writePartition"
            || !TARGETS.contains(&effect.target.as_str())
            || !targets.insert(effect.target.as_str())
            || effect.range_length == 0
            || effect.range_length > 16 * 1024 * 1024 * 1024
            || !effect.range_start.is_multiple_of(512)
            || effect
                .range_start
                .checked_add(effect.range_length)
                .is_none()
            || Sha256Digest::parse_hex(&effect.content_sha256).is_err()
        {
            return Err(Error::SourceRefused);
        }
        for kind in ["writeTarget", "verifyTarget"] {
            if plan
                .public_steps
                .iter()
                .filter(|s| {
                    s.kind == kind
                        && s.semantic_target == format!("partition:{}", effect.target)
                        && s.content_sha256 == effect.content_sha256
                })
                .count()
                != 1
            {
                return Err(Error::SourceRefused);
            }
        }
    }
    let full = targets == TARGETS.into_iter().collect();
    let ramdisk =
        plan.persistent_effects.len() == 1 && original_ramdisk(&plan.persistent_effects[0]);
    if !full && (restoring || !ramdisk) {
        return Err(Error::SourceRefused);
    }
    Ok(())
}
fn identity(
    port: &mut impl VerificationPort,
    source: &Source,
    deadline: Instant,
) -> Result<(), Error> {
    if Instant::now() >= deadline {
        return Err(Error::BudgetExhausted);
    }
    if port.fresh_identity(source, deadline)? != source.identity {
        return Err(Error::IdentityChanged);
    }
    if Instant::now() >= deadline {
        return Err(Error::BudgetExhausted);
    }
    Ok(())
}

/// Read-only verification. No restoration or replay is reachable from this API.
pub fn verify(
    port: &mut impl VerificationPort,
    job: &str,
    budget_ms: u64,
) -> Result<VerificationResult, Error> {
    validate_request(job, budget_ms)?;
    let deadline = Instant::now() + Duration::from_millis(budget_ms);
    let source = port.load_source(job)?;
    validate_source(&source, job, false)?;
    identity(port, &source, deadline)?;
    port.verification_intent(&source, budget_ms)?;
    let mut targets = Vec::new();
    let mut continuity_lost = false;
    for effect in &source.plan.persistent_effects {
        let mut result = TargetResult {
            effect: effect.clone(),
            outcome: contract::TARGET_VERDICT_FAILED,
            observed_sha256: None,
            verification_strength: None,
            reason: None,
        };
        let reading = if continuity_lost {
            Err(Error::IdentityChanged)
        } else {
            identity(port, &source, deadline).and_then(|()| {
                let read = port.read_partition(&source, effect, deadline);
                identity(port, &source, deadline)?;
                read
            })
        };
        match reading {
            Ok(ReadOutcome::FullHash(hash)) if hash == effect.content_sha256 => {
                result.outcome = contract::TARGET_VERDICT_VERIFIED;
                result.observed_sha256 = Some(hash);
                result.verification_strength = Some(contract::STRENGTH_FULL_HASH);
            }
            Ok(ReadOutcome::FullHash(hash)) => {
                result.observed_sha256 = Some(hash);
                result.reason = Some("content-mismatch".into());
            }
            Ok(ReadOutcome::MountedWritable) => {
                result.outcome = contract::TARGET_VERDICT_TYPED_SKIP;
                result.reason = Some(contract::SKIP_MOUNTED_WRITABLE.into());
            }
            Ok(ReadOutcome::Failed(reason)) => result.reason = Some(reason),
            Err(error) => {
                continuity_lost = true;
                result.reason = Some(error.to_string());
            }
        }
        targets.push(result);
    }
    if continuity_lost {
        for target in &mut targets {
            target.outcome = contract::TARGET_VERDICT_FAILED;
            target.verification_strength = None;
            target.reason = Some("target-continuity-or-budget-lost".into());
        }
    }
    let result = VerificationResult {
        source_job_id: source.job.job_id,
        source_plan_id: source.plan.plan_id,
        source_plan_sha256: source.plan.plan_sha256,
        targets,
    };
    port.verification_result(&result)?;
    Ok(result)
}

/// Returned token is opaque and consumed by value. The Runtime must durably
/// enforce single-use across restarts; Rust ownership alone is insufficient.
pub trait RestorationPort: VerificationPort {
    type Reservation;
    /// Materialize restoreRamdisk through the paired controller. Resolve the
    /// immutable artifact lease in the Runtime. Never mint capability here.
    fn restoration_plan(
        &mut self,
        source: &Source,
        artifact: &str,
        deadline: Instant,
    ) -> Result<ExecutablePlan, Error>;
    /// Under exclusion, re-read current source/binding/plan/artifact/tool facts,
    /// reserve exact RuntimeCapability, fsync intent and consume use ordinal.
    /// A previous intent (including stopped/unknown) must refuse a second use.
    fn reserve_restoration(
        &mut self,
        source: &Source,
        plan: &ExecutablePlan,
        deadline: Instant,
    ) -> Result<Self::Reservation, Error>;
    /// Dispatch exactly once through the existing StepPermit path and wait for
    /// a durable terminal job within deadline. An error never triggers retry.
    fn execute_restoration(
        &mut self,
        reservation: Self::Reservation,
        deadline: Instant,
    ) -> Result<String, Error>;
    /// fsync a stopped or success record; failure propagates, never success.
    fn restoration_result(
        &mut self,
        source_job: &str,
        result: &RestorationResult,
    ) -> Result<(), Error>;
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestorationResult {
    Verified {
        restoration_job: String,
        verification: VerificationResult,
    },
    Stopped {
        reason: String,
    },
}
impl RestorationResult {
    pub fn exit_code(&self) -> i32 {
        if matches!(self, Self::Verified { .. }) {
            0
        } else {
            1
        }
    }
}

/// Invalid inputs return InvalidInput (frontend exit 2) before calling any
/// port. Every later failure is stopped (exit 1), unless recording itself fails.
/// A successful result (exit 0) requires a second, full original-ramdisk hash.
pub fn restore(
    port: &mut impl RestorationPort,
    job: &str,
    artifact: &str,
    budget_ms: u64,
) -> Result<RestorationResult, Error> {
    validate_request(job, budget_ms)?;
    Sha256Digest::parse_hex(artifact).map_err(|_| Error::InvalidInput)?;
    let deadline = Instant::now() + Duration::from_millis(budget_ms);
    let mut attempt = || -> Result<RestorationResult, Error> {
        let source = port.load_source(job)?;
        validate_source(&source, job, true)?;
        identity(port, &source, deadline)?;
        let plan = port.restoration_plan(&source, artifact, deadline)?;
        if plan.plan_id == source.plan.plan_id
            || plan.execution_purpose != "primaryFlash"
            || Sha256Digest::parse_hex(&plan.plan_sha256).is_err()
            || plan.persistent_effects.len() != 1
            || !original_ramdisk(&plan.persistent_effects[0])
            || plan.authority_support_campaign != source.campaign
            || plan.mechanics_maturity_campaign != source.campaign
        {
            return Err(Error::SourceRefused);
        }
        identity(port, &source, deadline)?;
        let kinds = plan
            .public_steps
            .iter()
            .map(|step| step.kind.as_str())
            .collect::<Vec<_>>();
        let loader = [
            "probeDevice",
            "validateLayout",
            "writeTarget",
            "verifyTarget",
            "reboot",
            "postflightProbe",
        ];
        let normal = [
            "ensureMode",
            "probeDevice",
            "validateLayout",
            "writeTarget",
            "verifyTarget",
            "reboot",
            "postflightProbe",
        ];
        if kinds != loader && kinds != normal {
            return Err(Error::SourceRefused);
        }
        for step in plan
            .public_steps
            .iter()
            .filter(|step| matches!(step.kind.as_str(), "writeTarget" | "verifyTarget"))
        {
            if step.semantic_target != "partition:ramdisk"
                || step.content_sha256 != RESTORATION_SHA256
            {
                return Err(Error::SourceRefused);
            }
        }
        let reservation = port.reserve_restoration(&source, &plan, deadline)?;
        let restored = port.execute_restoration(reservation, deadline)?;
        if restored == job {
            return Err(Error::SourceRefused);
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(Error::BudgetExhausted)?
            .as_millis();
        let remaining = u64::try_from(remaining).map_err(|_| Error::BudgetExhausted)?;
        if remaining < 1000 {
            return Err(Error::BudgetExhausted);
        }
        let snapshot = port.load_source(&restored)?;
        if snapshot.identity != source.identity || snapshot.plan != plan {
            return Err(Error::IdentityChanged);
        }
        let verification = verify(port, &restored, remaining)?;
        if verification.targets.len() != 1
            || verification.exit_code() != 0
            || !original_ramdisk(&verification.targets[0].effect)
            || verification.source_plan_sha256 != plan.plan_sha256
        {
            return Err(Error::SourceRefused);
        }
        Ok(RestorationResult::Verified {
            restoration_job: restored,
            verification,
        })
    };
    let result = match attempt() {
        Ok(result) => result,
        Err(error) => RestorationResult::Stopped {
            reason: error.to_string(),
        },
    };
    port.restoration_result(job, &result)?;
    Ok(result)
}
