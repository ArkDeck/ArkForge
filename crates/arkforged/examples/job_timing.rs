//! Read-only timing analysis for one durable ArkForge job journal.

use arkforge_engine::durable::DurableJournal;
use arkforge_engine::journal::{JournalRecord, JournalRecordKind};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Default)]
struct StepTiming {
    step_id: String,
    label: String,
    partition: Option<String>,
    image_bytes: Option<u64>,
    readback_bytes: Option<u64>,
    staging_ms: u64,
    validation_work_ms: u64,
    validation_wait_ms: u64,
    preparation_lead_ms: Option<u64>,
    preparation_mode: Option<String>,
    validation_backend: Option<String>,
    operation_ms: Option<u64>,
    requested_at: Option<u64>,
    execution_at: Option<u64>,
    evidence_at: Option<u64>,
    checkpoint_at: Option<u64>,
}

impl StepTiming {
    fn admission_ms(&self) -> u64 {
        between(self.requested_at, self.execution_at)
    }

    fn execution_ms(&self) -> u64 {
        between(self.execution_at, self.evidence_at)
    }

    fn device_ms(&self) -> u64 {
        self.operation_ms
            .unwrap_or_else(|| self.execution_ms().saturating_sub(self.staging_ms))
    }

    fn execution_overhead_ms(&self) -> u64 {
        self.execution_ms()
            .saturating_sub(self.staging_ms)
            .saturating_sub(self.device_ms())
    }

    fn settlement_ms(&self) -> u64 {
        between(self.evidence_at, self.checkpoint_at)
    }

    fn total_ms(&self) -> u64 {
        between(self.requested_at, self.checkpoint_at)
    }

    fn category(&self) -> &'static str {
        if self.label.starts_with("enter-updater") {
            "mode-change"
        } else if self.label.starts_with("probe-loader")
            || self.label.starts_with("read-partition-table")
        {
            "preflight"
        } else if self.label.starts_with("write:") {
            "writes"
        } else if self.label.starts_with("verify:") {
            "verification"
        } else if self.label.starts_with("reset") {
            "reset"
        } else if self.label.starts_with("postflight") {
            "postflight"
        } else {
            "other"
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!(
            "usage: cargo run -p arkforged --example job_timing -- <job.journal> [--records]"
        );
        std::process::exit(2);
    };
    let records = args.any(|argument| argument == "--records");
    let path = PathBuf::from(path);
    let (journal, report) = DurableJournal::open(&path).unwrap_or_else(|error| {
        eprintln!("{}: {error}", path.display());
        std::process::exit(1);
    });
    if report.was_torn() {
        eprintln!(
            "refusing timing analysis after truncating {} torn tail byte(s)",
            report.torn_tail_bytes
        );
        std::process::exit(1);
    }
    if records {
        print_records(journal.journal().records());
    } else {
        print_summary(&path, journal.journal().records());
    }
}

fn print_records(records: &[JournalRecord]) {
    let mut prior = None;
    for record in records {
        let delta = prior
            .map(|at: u64| record.at_epoch_ms.saturating_sub(at))
            .unwrap_or(0);
        let facts = record
            .facts
            .iter()
            .map(|(key, value)| format!("{}={value}", key.as_str()))
            .collect::<Vec<_>>()
            .join(",");
        println!(
            "{}\t{}\t+{}\t{}\t{}\t{}",
            record.sequence,
            record.at_epoch_ms,
            delta,
            record.kind.as_str(),
            record.subject,
            facts
        );
        prior = Some(record.at_epoch_ms);
    }
}

fn print_summary(path: &std::path::Path, records: &[JournalRecord]) {
    let mut steps: BTreeMap<String, StepTiming> = BTreeMap::new();
    for record in records {
        let step_id = record.subject.as_str();
        if !step_id.starts_with("STEP-") {
            continue;
        }
        let step = steps
            .entry(step_id.to_string())
            .or_insert_with(|| StepTiming {
                step_id: step_id.to_string(),
                ..StepTiming::default()
            });
        match record.kind {
            JournalRecordKind::StepAdmissionRequested => {
                step.requested_at.get_or_insert(record.at_epoch_ms);
            }
            JournalRecordKind::PermitConsuming | JournalRecordKind::ExternalDispatchStarted => {
                step.execution_at.get_or_insert(record.at_epoch_ms);
                classify_step(step, record);
            }
            JournalRecordKind::TransportEvidenceRecorded => {
                step.evidence_at.get_or_insert(record.at_epoch_ms);
                classify_step(step, record);
            }
            JournalRecordKind::StepCheckpointed => {
                step.checkpoint_at.get_or_insert(record.at_epoch_ms);
            }
            _ => classify_step(step, record),
        }
    }

    let start = records
        .first()
        .map(|record| record.at_epoch_ms)
        .unwrap_or(0);
    let end = records
        .iter()
        .find(|record| record.kind == JournalRecordKind::OutcomeClassified)
        .or_else(|| records.last())
        .map(|record| record.at_epoch_ms)
        .unwrap_or(start);
    let job_ms = end.saturating_sub(start);
    let write_sizes: BTreeMap<String, u64> = steps
        .values()
        .filter_map(|step| Some((step.partition.clone()?, step.image_bytes?)))
        .collect();

    println!("journal: {}", path.display());
    println!("records: {}", records.len());
    println!("total: {}", duration(job_ms));
    println!();
    println!(
        "{:<9} {:<34} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>11}",
        "step",
        "action",
        "gap",
        "admit",
        "stage",
        "hashWait",
        "hashCPU",
        "device",
        "other",
        "total",
        "MiB/s"
    );

    let mut prior_checkpoint = Some(start);
    let mut gaps_ms = 0u64;
    let mut categories: BTreeMap<&str, u64> = BTreeMap::new();
    let mut admissions_ms = 0u64;
    let mut executions_ms = 0u64;
    let mut settlements_ms = 0u64;
    let mut staging_ms = 0u64;
    let mut device_ms = 0u64;
    let mut overhead_ms = 0u64;
    let mut validation_work_ms = 0u64;
    let mut validation_wait_ms = 0u64;
    for step in steps.values() {
        let gap = between(prior_checkpoint, step.requested_at);
        let admit = step.admission_ms();
        let execute = step.execution_ms();
        let device = step.device_ms();
        let overhead = step.execution_overhead_ms();
        let settle = step.settlement_ms();
        let total = step.total_ms();
        gaps_ms += gap;
        admissions_ms += admit;
        executions_ms += execute;
        settlements_ms += settle;
        staging_ms += step.staging_ms;
        device_ms += device;
        overhead_ms += overhead;
        validation_work_ms += step.validation_work_ms;
        validation_wait_ms += step.validation_wait_ms;
        *categories.entry(step.category()).or_default() += total;
        let transferred = step.readback_bytes.or_else(|| {
            step.partition
                .as_ref()
                .and_then(|partition| write_sizes.get(partition).copied())
        });
        let rate = transferred
            .filter(|_| device > 0)
            .map(|bytes| bytes as f64 / 1_048_576.0 / (device as f64 / 1000.0))
            .map(|rate| format!("{rate:.2}"))
            .unwrap_or_else(|| "-".into());
        println!(
            "{:<9} {:<34} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>11}",
            step.step_id,
            step.label,
            duration(gap),
            duration(admit),
            duration(step.staging_ms),
            duration(step.validation_wait_ms),
            duration(step.validation_work_ms),
            duration(device),
            duration(overhead),
            duration(total),
            rate
        );
        prior_checkpoint = step.checkpoint_at;
    }

    println!();
    println!("stage totals (step wall time, excluding gaps):");
    for category in [
        "mode-change",
        "preflight",
        "writes",
        "verification",
        "reset",
        "postflight",
        "other",
    ] {
        let milliseconds = categories.get(category).copied().unwrap_or(0);
        if milliseconds > 0 {
            println!(
                "  {:<14} {:>10} {:>6.2}%",
                category,
                duration(milliseconds),
                percentage(milliseconds, job_ms)
            );
        }
    }
    println!(
        "  {:<14} {:>10} {:>6.2}%",
        "inter-step",
        duration(gaps_ms),
        percentage(gaps_ms, job_ms)
    );
    println!();
    println!("pipeline totals:");
    println!("  admission     {}", duration(admissions_ms));
    println!("  execution     {}", duration(executions_ms));
    println!("    staging     {}", duration(staging_ms));
    println!("    hash wait   {}", duration(validation_wait_ms));
    println!(
        "    hash CPU    {} (parallel worker sum)",
        duration(validation_work_ms)
    );
    println!("    device/read {}", duration(device_ms));
    println!("    other       {}", duration(overhead_ms));
    println!("  settlement    {}", duration(settlements_ms));
    println!("  inter-step    {}", duration(gaps_ms));
    if let Some(prepared) = steps
        .values()
        .find(|step| step.preparation_lead_ms.is_some())
    {
        println!(
            "  preparation   {} lead={} backend={}",
            prepared.preparation_mode.as_deref().unwrap_or("unknown"),
            duration(prepared.preparation_lead_ms.unwrap_or(0)),
            prepared.validation_backend.as_deref().unwrap_or("unknown")
        );
    }
}

fn classify_step(step: &mut StepTiming, record: &JournalRecord) {
    let fact = |name: &str| {
        record
            .facts
            .iter()
            .find(|(key, _)| key.as_str() == name)
            .map(|(_, value)| value.as_str())
    };
    if let Some(action) = fact("controlAction") {
        step.label = if action == "read-build-facts" {
            "postflight:read-build-facts".into()
        } else {
            action.to_string()
        };
    }
    if fact("rockusbDevices").is_some() {
        step.label = "probe-loader".into();
    }
    if fact("observedLayoutDigest").is_some() {
        step.label = "read-partition-table".into();
    }
    if let Some(operation) = fact("operation") {
        step.label = format!("reset:{operation}");
    }
    if let Some(partition) = fact("partition") {
        step.partition = Some(partition.to_string());
        step.label = if fact("member").is_some() {
            format!("write:{partition}")
        } else {
            format!(
                "verify:{partition}/{}",
                fact("outcome").unwrap_or("unknown")
            )
        };
    }
    if fact("const.ohos.fullname").is_some() {
        step.label = "postflight:read-build-facts".into();
    }
    if let Some(bytes) = fact("imageBytes").and_then(|value| value.parse().ok()) {
        step.image_bytes = Some(bytes);
    }
    if let Some(bytes) = fact("readbackBytes").and_then(|value| value.parse().ok()) {
        step.readback_bytes = Some(bytes);
    }
    if let Some(milliseconds) = fact("stagingDurationMs").and_then(|value| value.parse().ok()) {
        step.staging_ms = milliseconds;
    }
    if let Some(milliseconds) =
        fact("imageValidationDurationMs").and_then(|value| value.parse().ok())
    {
        step.validation_work_ms = milliseconds;
    }
    if let Some(milliseconds) = fact("imageValidationWaitMs").and_then(|value| value.parse().ok()) {
        step.validation_wait_ms = milliseconds;
    }
    if let Some(milliseconds) = fact("preparationLeadMs").and_then(|value| value.parse().ok()) {
        step.preparation_lead_ms = Some(milliseconds);
    }
    if let Some(mode) = fact("preparationMode") {
        step.preparation_mode = Some(mode.to_string());
    }
    if let Some(backend) = fact("imageValidationBackend") {
        step.validation_backend = Some(backend.to_string());
    }
    if let Some(milliseconds) = fact("operationDurationMs").and_then(|value| value.parse().ok()) {
        step.operation_ms = Some(milliseconds);
    }
    if step.label.is_empty() {
        step.label = "unclassified".into();
    }
}

fn between(start: Option<u64>, end: Option<u64>) -> u64 {
    end.zip(start)
        .map(|(end, start)| end.saturating_sub(start))
        .unwrap_or(0)
}

fn duration(milliseconds: u64) -> String {
    if milliseconds >= 60_000 {
        format!(
            "{}m{:06.3}s",
            milliseconds / 60_000,
            (milliseconds % 60_000) as f64 / 1000.0
        )
    } else {
        format!("{:.3}s", milliseconds as f64 / 1000.0)
    }
}

fn percentage(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 * 100.0 / whole as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_keep_millisecond_precision() {
        assert_eq!(duration(999), "0.999s");
        assert_eq!(duration(61_234), "1m01.234s");
    }

    #[test]
    fn missing_boundaries_never_invent_time() {
        assert_eq!(between(None, Some(10)), 0);
        assert_eq!(between(Some(10), None), 0);
        assert_eq!(between(Some(20), Some(10)), 0);
    }

    #[test]
    fn execution_components_do_not_double_count_staging() {
        let timing = StepTiming {
            staging_ms: 80,
            operation_ms: Some(15),
            execution_at: Some(10),
            evidence_at: Some(110),
            ..StepTiming::default()
        };
        assert_eq!(timing.device_ms(), 15);
        assert_eq!(timing.execution_overhead_ms(), 5);
    }
}
