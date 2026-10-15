//! Independent read-only verification, never an upgrade of historical receipts.
use super::*;
use crate::hdc_control::partition::{PartitionRead, TARGETS, validate_range};
use arkforge_ipc::messages::{ActionReceiptSummary, Effect, JobSummary};
use std::collections::BTreeSet;

const MAX_BUDGET_MS: u64 = 600_000;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FlashVerificationResult {
    pub document: String,
    pub record_id: String,
    pub verified: u64,
    pub skipped: u64,
    pub failed: u64,
}

impl FlashVerificationResult {
    pub fn exit_code(&self) -> i32 {
        if self.failed != 0 {
            7
        } else if self.skipped != 0 {
            9
        } else {
            0
        }
    }

    pub(super) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_string(&mut out, 1, &self.document);
        wire::write_string(&mut out, 2, &self.record_id);
        wire::write_uint64(&mut out, 3, self.verified);
        wire::write_uint64(&mut out, 4, self.skipped);
        wire::write_uint64(&mut out, 5, self.failed);
        out
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<Self, StandaloneError> {
        let mut result = Self::default();
        let mut reader = wire::Reader::new(bytes);
        while let Some((field, value)) = reader.next_field().map_err(status_decode)? {
            match field {
                1 => result.document = value.as_str(1).map_err(status_decode)?.into(),
                2 => result.record_id = value.as_str(2).map_err(status_decode)?.into(),
                3 => result.verified = value.as_u64().map_err(status_decode)?,
                4 => result.skipped = value.as_u64().map_err(status_decode)?,
                5 => result.failed = value.as_u64().map_err(status_decode)?,
                _ => {}
            }
        }
        if result.document.is_empty()
            || result.record_id.is_empty()
            || result.verified > 9
            || result.skipped > 9
            || result.failed > 9
            || ![1, 9].contains(&(result.verified + result.skipped + result.failed))
        {
            return Err(refused("The verification response is incomplete."));
        }
        Ok(result)
    }
}

pub(super) fn handle(
    runtime: &Path,
    public: &mut PublicClient,
    session: &mut SupervisorSession<'_>,
    active_jobs: usize,
    arguments: &[String],
) -> Result<FlashVerificationResult, StandaloneError> {
    let (job_id, budget, campaign, preboot_image) = match arguments {
        [job_id, budget, campaign] => (job_id, budget, campaign, ""),
        [job_id, budget, campaign, image] => (job_id, budget, campaign, image.as_str()),
        _ => {
            return Err(StandaloneError::invalid(
                "Verification takes a job reference, budget and campaign.",
            ));
        }
    };
    JobId::new(job_id).map_err(|_| StandaloneError::invalid("Invalid job reference."))?;
    let budget = budget
        .parse::<u64>()
        .ok()
        .filter(|n| (1_000..=MAX_BUDGET_MS).contains(n))
        .ok_or_else(|| {
            StandaloneError::invalid("Verification budget must be 1000..600000 milliseconds.")
        })?;
    if active_jobs != 0 {
        return Err(refused(
            "Wait for every active job to become terminal before verification.",
        ));
    }
    if session.hardware_campaign.unwrap_or_default() != campaign {
        return Err(refused(
            "Verification must explicitly name the running hardware campaign.",
        ));
    }
    let hdc_digest = session
        .hdc_digest
        .ok_or_else(|| refused("Verification requires a hash-bound HDC port."))?;
    let hdc = session
        .hdc
        .as_deref_mut()
        .ok_or_else(|| refused("Verification requires a hash-bound HDC port."))?;
    let job = public.job_show(job_id)?;
    let lineage = load_job_lineage(runtime, job_id)?;
    let record = load_authority_plan(runtime, &job.plan_id)?;
    let events = public.job_events(job_id, 0)?;
    let receipts = events
        .iter()
        .filter_map(|event| event.receipt.as_ref())
        .collect::<Vec<_>>();
    validate_source(&record, &job, &lineage, &receipts, campaign)?;
    let nonce = if preboot_image.is_empty() {
        None
    } else {
        Some(preboot_nonce(runtime, &record, preboot_image)?)
    };
    let deadline = Instant::now() + Duration::from_millis(budget);
    current_observation(runtime, &lineage, &record.profile_id, deadline)?;

    let mut random = [0_u8; 16];
    fill_random(&mut random).map_err(|e| internal("allocate verification identity", e))?;
    let record_id = format!(
        "VERIFY-{}",
        random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let root = runtime.join("authority").join("flash-verifications");
    std::fs::create_dir_all(&root).map_err(|e| internal("create verification store", e))?;
    protect_path(&root, true).map_err(|e| internal("protect verification store", e))?;
    let directory = root.join(&record_id);
    std::fs::create_dir(&directory).map_err(|e| internal("create verification record", e))?;
    protect_path(&directory, true).map_err(|e| internal("protect verification record", e))?;
    sync_directory(&root).map_err(|e| internal("persist verification directory", e))?;
    let started = now_ms()?;
    let mut context = format!(
        "\"record_id\":{},\"source_job_id\":{},\"source_plan_id\":{},\"source_plan_sha256\":{},\"source_plan_record_sha256\":{},\"profile\":{},\"device_observation_id\":{},\"stable_identity_sha256\":{},\"topology_sha256\":{},\"runtime_epoch\":{},\"hdc_sha256\":{},\"hardware_campaign\":{},\"started_at_epoch_ms\":{},\"budget_ms\":{},\"read_domain\":\"normal-block-device\",\"original_job_unchanged\":true,\"production_support_promotion\":false",
        quoted(&record_id),
        quoted(job_id),
        quoted(&record.plan.plan_id),
        quoted(&record.plan.plan_sha256),
        quoted(&sha256(&encode_authority_plan(&record)).to_hex()),
        quoted(&record.profile_id),
        quoted(&lineage.current_device_id),
        quoted(&digest_bytes(&lineage.current_stable_identity_sha256, "target lineage")?.to_hex()),
        quoted(&lineage.topology_sha256),
        session.epoch,
        quoted(&hdc_digest.to_hex()),
        quoted(campaign),
        started,
        budget,
    );
    if nonce.is_some() {
        context = context.replace(
            "\"read_domain\":\"normal-block-device\"",
            "\"read_domain\":\"normal-block-device-and-preboot-initramfs\"",
        );
        context.push_str(&format!(
            ",\"preboot_ramdisk_sha256\":{}",
            quoted(preboot_image)
        ));
    }
    let targets = record
        .plan
        .persistent_effects
        .iter()
        .map(effect_json)
        .collect::<Vec<_>>()
        .join(",");
    persist(
        &directory,
        "intent.json",
        &format!(
            "{{\"schema\":\"arkforge.flash-verification-intent/v1\",{context},\"targets\":[{targets}]}}"
        ),
    )?;

    let mut outcomes = Vec::new();
    let mut identity_failed = false;
    let mut effects = record.plan.persistent_effects.iter().collect::<Vec<_>>();
    if nonce.is_some() {
        // Capture the bounded boot report before the longer static reads can
        // allow unrelated kernel logging to evict it from the ring buffer.
        effects.sort_by_key(|effect| effect.target != "userdata");
    }
    for effect in effects {
        let outcome = if identity_failed {
            Err("target-lineage-changed")
        } else if Instant::now() >= deadline {
            Err("verification-budget-exhausted")
        } else {
            match current_observation(runtime, &lineage, &record.profile_id, deadline) {
                Ok(observation) => {
                    let result = if effect.target == "userdata" && nonce.is_some() {
                        hdc.read_preboot(&observation, nonce.as_deref().expect("checked"), deadline)
                    } else {
                        hdc.read_partition(&observation, effect, deadline)
                    };
                    if Instant::now() >= deadline {
                        Err("verification-budget-exhausted")
                    } else if current_observation(runtime, &lineage, &record.profile_id, deadline)
                        .is_err()
                    {
                        identity_failed = true;
                        Err("target-lineage-changed")
                    } else {
                        result
                    }
                }
                Err(_) => {
                    identity_failed = true;
                    Err("target-lineage-changed")
                }
            }
        };
        outcomes.push((effect, outcome));
    }
    // If target continuity is lost, the aggregate must never retain a success
    // claim. No later target is read after the first identity failure.
    if identity_failed {
        for (_, outcome) in &mut outcomes {
            *outcome = Err("target-lineage-changed");
        }
    }
    let mut result = FlashVerificationResult {
        record_id,
        ..Default::default()
    };
    let mut rows = Vec::new();
    for (effect, outcome) in outcomes {
        let preboot = match &outcome {
            Ok(PartitionRead::PrebootHash {
                boot_id_sha256,
                storage_cid_sha256,
                ..
            }) => format!(
                ",\"measurement_stage\":\"before-stock-init-and-first-mount\",\"boot_id_sha256\":{},\"storage_cid_sha256\":{},\"preboot_nonce\":{}",
                quoted(boot_id_sha256),
                quoted(storage_cid_sha256),
                quoted(nonce.as_deref().unwrap_or_default())
            ),
            _ => String::new(),
        };
        let (verdict, strength, observed, reason) = match outcome {
            Ok(PartitionRead::Hash(digest) | PartitionRead::PrebootHash { digest, .. })
                if digest == effect.content_sha256 =>
            {
                result.verified += 1;
                ("verified", Some("fullHash"), Some(digest), "")
            }
            Ok(PartitionRead::Hash(digest) | PartitionRead::PrebootHash { digest, .. }) => {
                result.failed += 1;
                ("failed", None, Some(digest), "content-mismatch")
            }
            Ok(PartitionRead::Skip(reason)) => {
                result.skipped += 1;
                ("typedSkip", None, None, reason)
            }
            Err(reason) => {
                result.failed += 1;
                ("failed", None, None, reason)
            }
        };
        let base = effect_json(effect);
        rows.push(format!(
            "{},\"outcome\":{},\"verification_strength\":{},\"observed_sha256\":{},\"reason\":{}{preboot}}}",
            base.trim_end_matches('}'),
            quoted(verdict),
            nullable(strength),
            nullable(observed.as_deref()),
            nullable((!reason.is_empty()).then_some(reason))
        ));
    }
    let aggregate = if result.failed != 0 {
        "failed"
    } else if result.skipped != 0 {
        "partial"
    } else {
        "verified"
    };
    result.document = format!(
        "{{\"schema\":\"arkforge.flash-verification/v1\",{context},\"completed_at_epoch_ms\":{},\"outcome\":{},\"verified\":{},\"skipped\":{},\"failed\":{},\"targets\":[{}]}}",
        now_ms()?,
        quoted(aggregate),
        result.verified,
        result.skipped,
        result.failed,
        rows.join(",")
    );
    persist(&directory, "result.json", &result.document)?;
    Ok(result)
}

fn validate_source(
    record: &AuthorityPlanRecord,
    job: &JobSummary,
    lineage: &ActiveTargetLineage,
    receipts: &[&ActionReceiptSummary],
    campaign: &str,
) -> Result<(), StandaloneError> {
    if record.profile_id != "org.openharmony.dayu200@1.0.0"
        || !job.terminal
        || job.state != "succeeded"
        || job.plan_id != record.plan.plan_id
        || lineage.plan_id != job.plan_id
        || digest_bytes(&job.plan_sha256, "source plan")?.to_hex() != record.plan.plan_sha256
        || job.total_steps != record.plan.public_steps.len() as u64
        || job.completed_steps != job.total_steps
        || record.plan.authority_support_campaign != campaign
        || record.plan.mechanics_maturity_campaign != campaign
    {
        return Err(refused(
            "Verification requires a complete successful DAYU200 job from this authority and campaign.",
        ));
    }
    let mut names = BTreeSet::new();
    for effect in &record.plan.persistent_effects {
        validate_range(effect).map_err(refused)?;
        if !names.insert(effect.target.as_str()) {
            return Err(refused("Duplicate sealed write target."));
        }
        let writes = record
            .plan
            .public_steps
            .iter()
            .filter(|step| {
                step.kind == "writeTarget"
                    && step.semantic_target == format!("partition:{}", effect.target)
                    && step.content_sha256 == effect.content_sha256
            })
            .count();
        if writes != 1 {
            return Err(refused("The write target has no unique sealed image step."));
        }
    }
    let original_ramdisk_only = names == BTreeSet::from(["ramdisk"])
        && record.plan.persistent_effects[0].content_sha256
            == arkforge_artifact::preboot::RESTORATION_SHA256
        && record.plan.persistent_effects[0].range_length
            == arkforge_artifact::preboot::MAX_IMAGE_BYTES as u64;
    if names != TARGETS.into_iter().collect() && !original_ramdisk_only {
        return Err(refused(
            "The source is not a complete supported partition set.",
        ));
    }
    for step in &record.plan.public_steps {
        let matching = receipts
            .iter()
            .filter(|receipt| receipt.step_id == step.step_id)
            .collect::<Vec<_>>();
        let [receipt] = matching.as_slice() else {
            return Err(refused("A source step has no unique durable receipt."));
        };
        if receipt.job_id != job.job_id
            || receipt.plan_id != record.plan.plan_id
            || receipt.disposition != "semanticSuccess"
            || !receipt.failure_classification.is_empty()
        {
            return Err(refused(
                "The source has an unsuccessful or uncorrelated receipt.",
            ));
        }
        receipt.receipt_digest().map_err(refused)?;
    }
    Ok(())
}

fn preboot_nonce(
    runtime: &Path,
    record: &AuthorityPlanRecord,
    image: &str,
) -> Result<String, StandaloneError> {
    use arkforge_artifact::preboot;
    use std::io::Read;
    let digest = Sha256Digest::parse_hex(image).map_err(refused)?;
    let ramdisk = record
        .plan
        .persistent_effects
        .iter()
        .find(|e| e.target == "ramdisk")
        .ok_or_else(|| refused("The source has no ramdisk write."))?;
    let userdata = record
        .plan
        .persistent_effects
        .iter()
        .find(|e| e.target == "userdata")
        .ok_or_else(|| refused("The source has no userdata write."))?;
    if ramdisk.content_sha256 != digest.to_hex()
        || userdata.range_start != preboot::USERDATA_START
        || userdata.range_length != preboot::USERDATA_BYTES
    {
        return Err(refused(
            "Preboot image or userdata range differs from the sealed source.",
        ));
    }
    let path = runtime
        .join("derived-artifacts")
        .join("preboot")
        .join(format!("{}.img", digest.to_hex()));
    let file = std::fs::File::open(path).map_err(refused)?;
    let mut bytes = Vec::new();
    file.take(preboot::MAX_IMAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(refused)?;
    if sha256(&bytes) != digest || bytes.len() as u64 != ramdisk.range_length {
        return Err(refused("Preboot derivative content changed."));
    }
    preboot::inspect(&bytes).map_err(refused)
}

fn current_observation(
    runtime: &Path,
    lineage: &ActiveTargetLineage,
    profile: &str,
    deadline: Instant,
) -> Result<DeviceObservationView, StandaloneError> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| refused("The verification read budget is exhausted."))?;
    let mut public =
        PublicClient::connect_with_read_timeout(runtime, remaining.min(Duration::from_secs(5)))?;
    let observation = public
        .device_list()?
        .into_iter()
        .find(|candidate| candidate.observation_id == lineage.current_device_id)
        .ok_or_else(|| refused("The original target is not currently observed."))?;
    if Instant::now() >= deadline {
        return Err(refused("The verification read budget is exhausted."));
    }
    let probe = public.device_probe(&observation.observation_id, profile)?;
    if Instant::now() >= deadline {
        return Err(refused("The verification read budget is exhausted."));
    }
    validate_observation(lineage, &observation, &probe)?;
    Ok(observation)
}

fn validate_observation(
    lineage: &ActiveTargetLineage,
    observation: &DeviceObservationView,
    probe: &DeviceProbeView,
) -> Result<(), StandaloneError> {
    for value in [observation, &probe.observation] {
        if value.mode != "hdc-normal"
            || value.identity_strength != "serialAndTopology"
            || value.malformed_descriptor
            || value.serial_sha256.is_empty()
            || value.descriptor_sha256.is_empty()
            || value.topology_sha256 != lineage.topology_sha256
        {
            return Err(refused(
                "Verification requires fresh strong Normal-mode identity.",
            ));
        }
    }
    if stable_identity_digest(observation, probe).as_bytes()
        != lineage.current_stable_identity_sha256.as_slice()
    {
        return Err(refused(
            "Fresh device facts differ from the completed job's target lineage.",
        ));
    }
    Ok(())
}

fn effect_json(effect: &Effect) -> String {
    format!(
        "{{\"target\":{},\"range_start\":{},\"range_length\":{},\"expected_sha256\":{}}}",
        quoted(&effect.target),
        effect.range_start,
        effect.range_length,
        quoted(&effect.content_sha256)
    )
}

fn persist(directory: &Path, name: &str, document: &str) -> Result<(), StandaloneError> {
    let path = directory.join(name);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| internal("create immutable verification record", e))?;
    protect_path(&path, false).map_err(|e| internal("protect verification record", e))?;
    file.write_all(document.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|e| internal("persist verification record", e))?;
    sync_directory(directory).map_err(|e| internal("persist verification directory", e))
}

fn now_ms() -> Result<u64, StandaloneError> {
    use std::time::{SystemTime, UNIX_EPOCH};
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| internal("read verification clock", e))?
        .as_millis() as u64)
}

fn refused(reason: impl std::fmt::Display) -> StandaloneError {
    StandaloneError::new("FLASH_VERIFICATION_REFUSED", reason.to_string(), 3, false)
}

fn nullable(value: Option<&str>) -> String {
    value.map_or_else(|| "null".into(), quoted)
}

fn quoted(value: &str) -> String {
    let mut text = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => text.push_str("\\\""),
            '\\' => text.push_str("\\\\"),
            c if c <= '\u{1f}' => text.push_str(&format!("\\u{:04x}", c as u32)),
            c => text.push(c),
        }
    }
    text.push('"');
    text
}

#[cfg(test)]
mod tests;
