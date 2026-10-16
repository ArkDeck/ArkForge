//! `status` command.

use super::*;

/// One aggregate host and runtime snapshot.
///
/// It deliberately never starts a runtime: a caller asking "what is going on"
/// must be able to learn "nothing is running" without that question changing
/// the answer. Every section separates three states that a bare array cannot —
/// not observable, observed and empty, observed and populated.
pub(crate) fn run_status(globals: Globals) -> Result<i32, CliError> {
    let runtime_dir = command_runtime_dir(&globals)?;
    // Refuse before any IPC when the host clock itself is unusable: an
    // unstampable snapshot is a failed host assessment, not a partial one.
    let captured_at_epoch_ms = now_epoch_ms()?;
    let platform_supported = cfg!(any(target_os = "macos", target_os = "windows"));
    let inspect_ready = platform_supported;
    let runtime = supervisor::status(&runtime_dir).ok();

    let mut connection = runtime.as_ref().map(|_| public_client(&globals));
    let devices = match connection.as_mut() {
        None => StatusSection::unobservable("RUNTIME_NOT_RUNNING"),
        Some(Err(error)) => StatusSection::unobservable(error.code.clone()),
        Some(Ok(client)) => match client.device_list() {
            Ok(observations) => StatusSection::enumerated(
                observations.iter().map(status_device_json).collect(),
                observations
                    .iter()
                    .map(|observation| {
                        format!(
                            "{}  mode={}  identity={}",
                            observation.observation_id,
                            observation.mode,
                            observation.identity_strength
                        )
                    })
                    .collect(),
            ),
            Err(error) => StatusSection::unobservable(error.code),
        },
    };
    // Held beside the section because an unknown job set must not render as
    // "none are running": null and [] are different answers.
    let mut active_job_ids: Option<Vec<String>> = None;
    let jobs = match connection.as_mut() {
        None => StatusSection::unobservable("RUNTIME_NOT_RUNNING"),
        Some(Err(error)) => StatusSection::unobservable(error.code.clone()),
        Some(Ok(client)) => match client.job_list() {
            Ok(summaries) => {
                active_job_ids = Some(
                    summaries
                        .iter()
                        .filter(|job| !job.terminal)
                        .map(|job| job.job_id.clone())
                        .collect(),
                );
                StatusSection::enumerated(
                    summaries.iter().map(status_job_json).collect(),
                    summaries
                        .iter()
                        .map(|job| {
                            format!("{}  state={}  plan={}", job.job_id, job.state, job.plan_id)
                        })
                        .collect(),
                )
            }
            Err(error) => StatusSection::unobservable(error.code),
        },
    };
    // The content store is host-local, so it stays readable while no runtime is
    // paired. An absent store is a complete enumeration of zero, not unknown.
    let artifacts = match list_artifacts(&globals) {
        Ok(objects) => StatusSection::enumerated(
            objects
                .iter()
                .map(|(digest, size)| {
                    format!(
                        "{{\"artifact_id\":{},\"sha256\":{},\"size_bytes\":{size}}}",
                        json(&digest.to_hex()),
                        json(&digest.to_hex())
                    )
                })
                .collect(),
            objects
                .iter()
                .map(|(digest, size)| format!("{}  size_bytes={size}", digest.to_hex()))
                .collect(),
        ),
        Err(error) => StatusSection::unobservable(error.code),
    };

    let mut blockers: Vec<String> = Vec::new();
    if !platform_supported {
        blockers.push("PLATFORM_UNSUPPORTED".into());
    }
    match &runtime {
        None => blockers.push("RUNTIME_NOT_RUNNING".into()),
        Some(status) => blockers.extend(status.blockers.iter().cloned()),
    }
    for section in [&devices, &artifacts, &jobs] {
        if let Some(reason) = section.reason.as_deref()
            && !blockers.iter().any(|blocker| blocker == reason)
        {
            blockers.push(reason.to_string());
        }
    }
    let complete = devices.complete() && artifacts.complete() && jobs.complete();

    let execute_ready = platform_supported
        && runtime.as_ref().is_some_and(|status| {
            status.mechanics_ready
                && status.authority_support_available
                && status.hdc_bound
                && status.blockers.is_empty()
        });
    let next = runtime.as_ref().map_or_else(
        || vec!["arkforge daemon start".to_string()],
        daemon::daemon_next_commands,
    );

    match globals.output {
        Output::Human => {
            if !globals.quiet {
                println!("ArkForge status");
            }
            println!("host");
            println!("  platform supported: {platform_supported}");
            println!("  inspect ready:      {inspect_ready}");
            if globals.verbose {
                println!("  runtime dir set:    {}", globals.runtime_dir.is_some());
                println!("  color disabled:     {}", globals.no_color);
            }
            match &runtime {
                None => {
                    println!("runtime");
                    println!("  running:            false");
                }
                Some(status) => {
                    println!("runtime");
                    println!("  running:            true");
                    println!("  pairing epoch:      {}", status.epoch);
                    println!("  mechanics ready:    {}", status.mechanics_ready);
                    println!(
                        "  authority support:  {}",
                        status.authority_support_available
                    );
                    println!("  HDC bound:          {}", status.hdc_bound);
                    if !status.hardware_campaign.is_empty() {
                        println!("  hardware campaign:  {}", status.hardware_campaign);
                    }
                    println!("  execute ready:      {execute_ready}");
                    println!("  active jobs:        {}", status.active_jobs);
                }
            }
            devices.print_human("devices", "No device observations are available.");
            artifacts.print_human("artifacts", "No artifacts are stored in this runtime.");
            jobs.print_human("jobs", "No durable jobs are recorded in this runtime.");
            if !blockers.is_empty() {
                println!("blockers");
                for blocker in &blockers {
                    println!("  {blocker}  ({})", blocker_remediation(blocker));
                }
            }
            if !complete {
                println!("This snapshot is partial; at least one section could not be observed.");
            }
            println!("Next: {}", next[0]);
        }
        Output::Json => {
            let runtime_json = match &runtime {
                None => "{\"running\":false}".to_string(),
                Some(status) => format!(
                    "{{\"running\":true,\"pairing_epoch\":{},\"protocol\":{{\"major\":{},\"minor\":{}}},\"daemon_version\":{},\"mechanics_ready\":{},\"authority_support_available\":{},\"hdc_bound\":{},\"hardware_campaign\":{},\"execute_ready\":{},\"active_job_count\":{},\"active_jobs\":{}}}",
                    status.epoch,
                    status.protocol_major,
                    status.protocol_minor,
                    json(&status.daemon_version),
                    status.mechanics_ready,
                    status.authority_support_available,
                    status.hdc_bound,
                    optional_json(
                        (!status.hardware_campaign.is_empty())
                            .then_some(status.hardware_campaign.as_str())
                    ),
                    execute_ready,
                    status.active_jobs,
                    active_job_ids
                        .as_ref()
                        .map_or_else(|| "null".to_string(), |ids| json_strings(ids)),
                ),
            };
            let blocker_values = blockers
                .iter()
                .map(|code| {
                    format!(
                        "{{\"code\":{},\"remediation\":{}}}",
                        json(code),
                        json(blocker_remediation(code))
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            emit_json!(
                "{{\"schema\":{},\"captured_at_epoch_ms\":{captured_at_epoch_ms},\"complete\":{complete},\"host\":{{\"platform_supported\":{platform_supported},\"inspect_ready\":{inspect_ready}}},\"runtime\":{runtime_json},\"devices\":{},\"artifacts\":{},\"jobs\":{},\"blockers\":[{blocker_values}],\"next_commands\":{}}}",
                json(STATUS_SCHEMA),
                devices.json(),
                artifacts.json(),
                jobs.json(),
                json_strings(&next),
            );
        }
    }
    Ok(0)
}

/// One `status` section. `available`/`complete` exist so "not observable" never
/// renders as the empty list that "observed nothing" earns.
pub(crate) struct StatusSection {
    available: bool,
    reason: Option<String>,
    pub(crate) items: Option<Vec<String>>,
    lines: Vec<String>,
}

impl StatusSection {
    pub(crate) fn enumerated(items: Vec<String>, lines: Vec<String>) -> Self {
        Self {
            available: true,
            reason: None,
            items: Some(items),
            lines,
        }
    }

    pub(crate) fn unobservable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            reason: Some(reason.into()),
            items: None,
            lines: Vec::new(),
        }
    }

    fn complete(&self) -> bool {
        self.items.is_some()
    }

    pub(crate) fn json(&self) -> String {
        format!(
            "{{\"available\":{},\"complete\":{},\"reason\":{},\"items\":{}}}",
            self.available,
            self.complete(),
            optional_json(self.reason.as_deref()),
            self.items.as_ref().map_or_else(
                || "null".to_string(),
                |items| format!("[{}]", items.join(","))
            ),
        )
    }

    pub(crate) fn print_human(&self, title: &str, empty: &str) {
        println!("{title}");
        match (&self.items, &self.reason) {
            (None, reason) => println!(
                "  not observable ({})",
                reason.as_deref().unwrap_or("UNKNOWN")
            ),
            (Some(items), _) if items.is_empty() => println!("  {empty}"),
            (Some(_), _) => {
                for line in &self.lines {
                    println!("  {line}");
                }
            }
        }
    }
}

/// The bounded device summary `status` denormalizes. Raw descriptor serials
/// never leave the mechanics daemon, so only the domain-separated digest is
/// reported here.
fn status_device_json(observation: &DeviceObservationView) -> String {
    format!(
        "{{\"observation_id\":{},\"observed_at_epoch_ms\":{},\"mode\":{},\"serial_sha256\":{},\"identity_strength\":{}}}",
        json(&observation.observation_id),
        observation.observed_at_epoch_ms,
        json(&observation.mode),
        optional_json(
            (!observation.serial_sha256.is_empty()).then_some(observation.serial_sha256.as_str())
        ),
        json(&observation.identity_strength),
    )
}

fn status_job_json(job: &JobSummary) -> String {
    format!(
        "{{\"job_id\":{},\"plan_id\":{},\"state\":{},\"terminal\":{}}}",
        json(&job.job_id),
        json(&job.plan_id),
        json(&job.state),
        job.terminal,
    )
}

fn blocker_remediation(code: &str) -> &'static str {
    match code {
        "RUNTIME_NOT_RUNNING" => "arkforge daemon start",
        "PLATFORM_UNSUPPORTED" => "arkforge help --all --format json",
        "AUTHORITY_HDC_UNBOUND" | "AUTHORITY_SUPPORT_UNPUBLISHED" => {
            "arkforge help daemon start --format json"
        }
        "NO_PAIRED_AUTHORITY" | "NO_DISPATCHER" => "arkforge daemon stop",
        "TOOLCHAIN_DIGEST_MISMATCH" => "arkforge help flash plan --format json",
        _ => remediation(code).unwrap_or("arkforge help --all --format json"),
    }
}
