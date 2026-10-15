//! Resolve restoration targets from durable authority facts, not live-only events.
use super::*;
use crate::hdc_control::partition::{TARGETS, validate_range};
use arkforge_ipc::messages::{ActionReceiptSummary, JobSummary};
use std::collections::BTreeSet;

pub(super) fn require_source(
    runtime: &Path,
    public: &mut PublicClient,
    source: &str,
    observation: &DeviceObservationView,
    probe: &DeviceProbeView,
    campaign: &str,
) -> Result<(), StandaloneError> {
    JobId::new(source).map_err(|_| StandaloneError::invalid("Invalid source job reference."))?;
    let job = public.job_show(source)?;
    let record = load_authority_plan(runtime, &job.plan_id)?;
    let lineage = load_job_lineage(runtime, source)?;
    let events = public.job_events(source, 0)?;
    let receipts = events
        .iter()
        .filter_map(|e| e.receipt.as_ref())
        .collect::<Vec<_>>();
    validate_source(&record, &job, &lineage, &receipts, campaign)?;
    validate_target(&record, &lineage, observation, probe)
}

fn refused(message: impl ToString) -> StandaloneError {
    StandaloneError::new("TARGET_LINEAGE_CONFLICT", message.to_string(), 3, false)
}

fn validate_source(
    record: &AuthorityPlanRecord,
    job: &JobSummary,
    lineage: &ActiveTargetLineage,
    receipts: &[&ActionReceiptSummary],
    campaign: &str,
) -> Result<(), StandaloneError> {
    let plan = &record.plan;
    let steps = &plan.public_steps;
    if record.profile_id != "org.openharmony.dayu200@1.0.0"
        || plan.execution_purpose != "primaryFlash"
        || !record.supersedes_job_id.is_empty()
        || campaign.is_empty()
        || plan.authority_support_campaign != campaign
        || plan.mechanics_maturity_campaign != campaign
        || job.plan_id != plan.plan_id
        || lineage.plan_id != job.plan_id
        || lineage.topology_sha256 != record.topology_sha256
        || digest_bytes(&job.plan_sha256, "source plan")?.to_hex() != plan.plan_sha256
        || !matches!(
            job.state.as_str(),
            "rebindWait" | "cancelledSafe" | "succeeded"
        )
        || job.terminal != (job.state != "rebindWait")
        || ![22, 23].contains(&job.total_steps)
        || job.total_steps != steps.len() as u64
        || job.completed_steps != job.total_steps - u64::from(job.state != "succeeded")
        || steps[steps.len() - 2].kind != "reboot"
        || steps[steps.len() - 1].kind != "postflightProbe"
    {
        return Err(refused(
            "Restoration requires a confirmed full-flash reboot boundary in the same campaign.",
        ));
    }
    let mut targets = BTreeSet::new();
    for effect in &plan.persistent_effects {
        validate_range(effect).map_err(refused)?;
        if !targets.insert(effect.target.as_str()) {
            return Err(refused("Duplicate source partition effect."));
        }
        for kind in ["writeTarget", "verifyTarget"] {
            if steps
                .iter()
                .filter(|s| {
                    s.kind == kind
                        && s.semantic_target == format!("partition:{}", effect.target)
                        && s.content_sha256 == effect.content_sha256
                })
                .count()
                != 1
            {
                return Err(refused(
                    "Source partition has no unique sealed write and verification.",
                ));
            }
        }
    }
    if targets != TARGETS.into_iter().collect() || receipts.len() != job.completed_steps as usize {
        return Err(refused("Source partition or receipt set is incomplete."));
    }
    for step in steps.iter().take(job.completed_steps as usize) {
        let matching = receipts
            .iter()
            .filter(|r| r.step_id == step.step_id)
            .collect::<Vec<_>>();
        let [receipt] = matching.as_slice() else {
            return Err(refused(
                "Source step has no unique durable success receipt.",
            ));
        };
        if receipt.job_id != job.job_id
            || receipt.plan_id != job.plan_id
            || receipt.disposition != "semanticSuccess"
            || !receipt.failure_classification.is_empty()
        {
            return Err(refused(
                "Source action outcome is failed, uncertain or uncorrelated.",
            ));
        }
        receipt.receipt_digest().map_err(refused)?;
    }
    Ok(())
}

fn validate_target(
    record: &AuthorityPlanRecord,
    lineage: &ActiveTargetLineage,
    observation: &DeviceObservationView,
    probe: &DeviceProbeView,
) -> Result<(), StandaloneError> {
    for value in [observation, &probe.observation] {
        if !matches!(value.mode.as_str(), "hdc-normal" | "rockusb-loader")
            || value.identity_strength != "serialAndTopology"
            || value.malformed_descriptor
            || value.topology_sha256 != record.topology_sha256
            || [
                &value.serial_sha256,
                &value.descriptor_sha256,
                &value.topology_sha256,
            ]
            .iter()
            .any(|s| Sha256Digest::parse_hex(s).is_err())
        {
            return Err(refused(
                "Restoration requires fresh strong identity on the source topology.",
            ));
        }
    }
    // ProbeDevice returns the profile ID; the sealed authority record stores
    // its versioned reference. The probe's full facts remain in the digest.
    if record
        .profile_id
        .split_once('@')
        .is_none_or(|(id, _)| probe.profile_id != id)
        || lineage.plan_id != record.plan.plan_id
        || lineage.topology_sha256 != record.topology_sha256
    {
        return Err(refused("Source target lineage or profile differs."));
    }
    let stable = stable_identity_digest(observation, probe);
    let original = observation.observation_id == record.device_id
        && stable.as_bytes() == record.stable_identity_sha256.as_slice();
    let current = observation.observation_id == lineage.current_device_id
        && stable.as_bytes() == lineage.current_stable_identity_sha256.as_slice();
    if !original && !current {
        return Err(refused(
            "Fresh device facts match neither the original nor current durable source binding.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
