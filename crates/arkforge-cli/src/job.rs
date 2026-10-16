//! `job`, `watch`, and `cancel` commands.

use super::*;

/// Follows one job's durable events, defaulting to the one that is running.
pub(crate) fn run_watch(arguments: &[String], globals: Globals) -> Result<i32, CliError> {
    let options = Options::parse(arguments)?;
    let after_sequence = options
        .optional_one("after-sequence")?
        .map(|value| parse_u64("--after-sequence", value))
        .transpose()?
        .unwrap_or(0);
    let timeout_ms = options
        .optional_one("timeout-ms")?
        .map(|value| parse_u64("--timeout-ms", value))
        .transpose()?
        .unwrap_or(30_000);
    ensure_runtime(&globals, options.optional_one("hardware-campaign")?)?;
    let job_id = match options.optional_one("job")? {
        Some(job) => job.to_string(),
        None => default_watch_job(&globals)?,
    };
    let (events, summary, timed_out) = watch_job(&globals, &job_id, after_sequence, timeout_ms)?;
    print_job_watch(
        &globals,
        &["watch"],
        after_sequence,
        timeout_ms,
        &events,
        &summary,
        timed_out,
    );
    Ok(0)
}

/// The job a bare `watch` means.
///
/// Exactly one running job is the only unambiguous answer, so it is the only
/// one taken. With none running the most recently active job is reported —
/// terminal jobs return immediately, which is the report. Several running jobs
/// is a real ambiguity and is refused with all of them named.
fn default_watch_job(globals: &Globals) -> Result<String, CliError> {
    let mut client = public_client(globals)?;
    let jobs = client.job_list()?;
    if jobs.is_empty() {
        return Err(CliError::new(
            "NO_JOBS_RECORDED",
            "This runtime has no durable jobs to watch.",
            5,
            false,
        ));
    }
    let mut active = jobs.iter().filter(|job| !job.terminal).collect::<Vec<_>>();
    match active.len() {
        1 => return Ok(active.remove(0).job_id.clone()),
        0 => {}
        count => {
            return Err(CliError::new(
                "JOB_AMBIGUOUS",
                format!(
                    "{count} jobs are still running; name one with --job <job-id>. Candidates: {}.",
                    active
                        .iter()
                        .map(|job| format!("{} ({})", job.job_id, job.state))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
                6,
                false,
            ));
        }
    }
    // No job is running, so "most recent" is decided by durable evidence: the
    // timestamp of each job's last journalled event, with the identifier
    // breaking a tie so the answer is the same on every run.
    let mut ranked = Vec::with_capacity(jobs.len());
    for job in &jobs {
        ranked.push((job_last_activity(&mut client, job), job.job_id.clone()));
    }
    ranked.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    Ok(ranked
        .pop()
        .map(|(_, job_id)| job_id)
        .expect("the job list was checked to be non-empty"))
}

/// When a job last wrote to its journal, or zero when it never did.
fn job_last_activity(client: &mut PublicClient, job: &JobSummary) -> u64 {
    if job.last_sequence == 0 {
        return 0;
    }
    client
        .job_events(&job.job_id, job.last_sequence - 1)
        .ok()
        .and_then(|events| events.last().map(|event| event.at_epoch_ms))
        .unwrap_or(0)
}

/// Asks the authority to stop a job at a safe boundary.
pub(crate) fn run_cancel(arguments: &[String], globals: Globals) -> Result<i32, CliError> {
    let options = Options::parse(arguments)?;
    let job_id = options.one("job")?;
    let expected_sequence = parse_u64("--expect-sequence", options.one("expect-sequence")?)?;
    ensure_runtime(&globals, options.optional_one("hardware-campaign")?)?;
    let runtime_dir = command_runtime_dir(&globals)?;
    let state = supervisor::cancel_job(&runtime_dir, job_id, expected_sequence)?;
    match globals.output {
        Output::Human => {
            println!("Cancellation result for {job_id}: {state}");
            println!("The original journal remains durable; no action was replayed.");
        }
        Output::Json => emit_json!(
            "{{\"schema\":\"arkforge.job-cancellation/v1\",\"job_id\":{},\"expect_sequence\":{},\"disposition\":{},\"automatic_replay\":false,\"next_commands\":[{}]}}",
            json(job_id),
            expected_sequence,
            json(&state),
            json(&format!("arkforge job show --job {job_id}")),
        ),
    }
    Ok(match state.as_str() {
        "cancelled-safe" | "already-terminal" => 0,
        "outcome-unknown" => 8,
        _ => 9,
    })
}
pub(crate) fn run_job(arguments: &[String], globals: Globals) -> Result<i32, CliError> {
    let Some(subcommand) = arguments.first() else {
        help::print_help(help::help_spec(&["job".into()])?, globals.output);
        return Ok(0);
    };
    let options = Options::parse(&arguments[1..])?;
    ensure_runtime(&globals, options.optional_one("hardware-campaign")?)?;
    match subcommand.as_str() {
        "list" => {
            let mut client = public_client(&globals)?;
            let jobs = client.job_list()?;
            print_jobs(globals.output, &jobs);
            Ok(0)
        }
        "show" => {
            let options = Options::parse(&arguments[1..])?;
            let job_id = options.one("job")?;
            let mut client = public_client(&globals)?;
            let job = client.job_show(job_id)?;
            // The three questions an operator has about a job — what happened,
            // what was proved, and what may be done next — are answered here
            // together. Splitting them across commands made the last one easy
            // to skip, and it is the one that keeps a replay from happening.
            let events = client.job_events(job_id, 0);
            let recovery = client.recovery_guide(job_id);
            print_job(globals.output, &job, events.as_deref(), recovery.as_ref());
            Ok(0)
        }
        "reconcile" => {
            let options = Options::parse(&arguments[1..])?;
            let job_id = options.one("job")?;
            let runtime_dir = command_runtime_dir(&globals)?;
            let status = supervisor::reconcile_job(&runtime_dir, job_id)?;
            print_reconciliation(globals.output, &status);
            Ok(if status.verdict == "stillUnknown" {
                8
            } else {
                0
            })
        }
        "recover" => run_job_recover(&arguments[1..], globals),
        other => Err(CliError::invalid(format!(
            "Unknown job command {other:?}. Run 'arkforge help job'."
        ))),
    }
}

fn print_reconciliation(output: Output, status: &supervisor::ReconcileStatus) {
    match output {
        Output::Human => {
            println!("Reconciliation for {}: {}", status.job_id, status.verdict);
            println!("  original state: {}", status.original_state);
            println!("  possible-effect completeness: {}", status.completeness);
            println!("  {}", status.detail);
            for effect in &status.possible_effects {
                println!("  possible effect: {effect}");
            }
            if status.verdict == "stillUnknown" {
                println!("Next: arkforge job show --job {}", status.job_id);
            }
        }
        Output::Json => emit_json!(
            "{{\"schema\":\"arkforge.job-reconciliation/v1\",\"job_id\":{},\"verdict\":{},\"detail\":{},\"possible_effect_completeness\":{},\"possible_effects\":{},\"original_state\":{},\"original_outcome_immutable\":true,\"automatic_replay_forbidden\":true,\"next_commands\":{}}}",
            json(&status.job_id),
            json(&status.verdict),
            json(&status.detail),
            json(&status.completeness),
            json_strings(&status.possible_effects),
            json(&status.original_state),
            if status.verdict == "stillUnknown" {
                format!(
                    "[{}]",
                    json(&format!("arkforge job show --job {}", status.job_id))
                )
            } else {
                "[]".into()
            }
        ),
    }
}

pub(crate) fn watch_job(
    globals: &Globals,
    job_id: &str,
    after_sequence: u64,
    timeout_ms: u64,
) -> Result<(Vec<JobEvent>, JobSummary, bool), CliError> {
    let started = std::time::Instant::now();
    let mut client = public_client(globals)?;
    let mut summary = client.job_show(job_id)?;
    if after_sequence > summary.last_sequence {
        return Err(CliError::new(
            "STALE_JOB_SEQUENCE",
            format!(
                "--after-sequence {after_sequence} is ahead of job {job_id} sequence {}.",
                summary.last_sequence
            ),
            6,
            true,
        ));
    }
    let mut cursor = after_sequence;
    let mut events = Vec::new();
    loop {
        let next = client.job_events(job_id, cursor)?;
        if let Some(last) = next.last() {
            cursor = last.sequence;
        }
        events.extend(next);
        summary = client.job_show(job_id)?;
        if summary.terminal && cursor >= summary.last_sequence {
            return Ok((events, summary, false));
        }
        if started.elapsed().as_millis() as u64 >= timeout_ms {
            return Ok((events, summary, true));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Materializes a distinct plan that supersedes an unresolved job.
///
/// It reuses the same inference engine the normal path uses, because "which
/// device, which profile, which firmware" are the same questions here. What it
/// never does is resume: the original job keeps its outcome, its journal, and
/// its permits, and the plan produced is a new one with a new epoch that the
/// top-level apply executes.
fn run_job_recover(arguments: &[String], globals: Globals) -> Result<i32, CliError> {
    let options = Options::parse(arguments)?;
    let job_id = options.one("job")?.to_string();
    let registry = profile_registry()?;
    let runtime_dir = command_runtime_dir(&globals)?;
    ensure_runtime(&globals, options.optional_one("hardware-campaign")?)?;

    let resolved = flash::resolve_plan_inputs(&globals, &registry, &options, true, None)?;
    let mut partial = flash::PartialResolution::from_resolved(&resolved);
    // Accepting a superseding plan means naming the job it supersedes, so that
    // token travels with every command this document hands back.
    partial.extra_acknowledgements = vec![format!("recovery:supersedes-job={job_id}")];

    let assessment = supervisor::assess_plan(
        &runtime_dir,
        &resolved.artifact.artifact_id,
        &resolved.profile.reference,
        &resolved.device.observation.observation_id,
    )
    .map_err(|error| {
        partial.refuse(&error.code, error.message, error.exit_code, error.retryable)
    })?;

    let materialized = supervisor::materialize_recovery_plan(
        &runtime_dir,
        &job_id,
        &resolved.artifact.artifact_id,
        &resolved.profile.reference,
        &resolved.device.observation.observation_id,
    )
    .map_err(|error| {
        partial.refuse(&error.code, error.message, error.exit_code, error.retryable)
    })?;
    match materialized {
        MaterializePlanResponse::Plan(plan) => {
            partial.sealed_campaign = flash::sealed_campaign(&plan);
            flash::print_flash_plan_v2(
                globals.output,
                &resolved,
                &partial,
                Some(&assessment),
                Some(&plan),
            );
            Ok(0)
        }
        MaterializePlanResponse::Assessment(assessment) => Err(partial.refuse_with(
            "RECOVERY_PLAN_UNAVAILABLE",
            format!(
                "No superseding plan was created for {job_id}: {}",
                if assessment.unavailable_reason.is_empty() {
                    &assessment.availability
                } else {
                    &assessment.unavailable_reason
                }
            ),
            3,
            false,
            Some(&assessment),
            None,
        )),
    }
}
fn print_jobs(output: Output, jobs: &[JobSummary]) {
    let next = if jobs.is_empty() {
        vec!["arkforge device list".to_string()]
    } else {
        vec!["arkforge job show --job <job-id>".to_string()]
    };
    match output {
        Output::Human => {
            if jobs.is_empty() {
                println!("No durable jobs are recorded in this runtime.");
                println!("Next: {}", next[0]);
                return;
            }
            println!("Durable jobs ({})", jobs.len());
            for job in jobs {
                print_job_human(job);
            }
            println!("Next: {}", next[0]);
        }
        Output::Json => {
            let values = jobs.iter().map(job_json).collect::<Vec<_>>().join(",");
            emit_json!(
                "{{\"schema\":\"arkforge.job-list/v1\",\"jobs\":[{values}],\"next_commands\":{}}}",
                json_array(&next)
            );
        }
    }
}

/// The most recent events a job document carries.
///
/// A tail rather than the whole journal: the journal is the durable record and
/// stays queryable, while a composite document that grew with job length would
/// stop being one an Agent can budget for.
const JOB_EVENT_TAIL: usize = 20;

fn print_job(
    output: Output,
    job: &JobSummary,
    events: Result<&[JobEvent], &PublicClientError>,
    recovery: Result<&RecoveryGuideView, &PublicClientError>,
) {
    // Recovery guidance is not advice about a broken job only: an unknown
    // outcome is exactly the state where the wrong next move is a replay.
    let recovery_eligible =
        job.state == "outcomeUnknown" || (job.terminal && job.state != "succeeded");
    let next = if job.state == "outcomeUnknown" {
        vec![format!("arkforge job reconcile --job {}", job.job_id)]
    } else if job.terminal {
        Vec::new()
    } else {
        vec![format!("arkforge watch --job {}", job.job_id)]
    };
    let receipts = events
        .map(|events| {
            events
                .iter()
                .filter_map(|event| event.receipt.as_ref())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let tail = events
        .map(|events| &events[events.len().saturating_sub(JOB_EVENT_TAIL)..])
        .unwrap_or(&[]);

    match output {
        Output::Human => {
            print_job_human(job);
            match events {
                Ok(all) => {
                    println!("events ({} total, showing last {})", all.len(), tail.len());
                    for event in tail {
                        println!(
                            "  {}  {}  state={}",
                            event.sequence,
                            event.kind.as_str(),
                            event.job_state
                        );
                    }
                    println!("receipts ({})", receipts.len());
                    for receipt in &receipts {
                        println!(
                            "  {}  disposition={}  verification={}",
                            receipt.step_id, receipt.disposition, receipt.verification_outcome
                        );
                    }
                }
                Err(error) => println!("events  not observable ({})", error.code),
            }
            println!("recovery");
            println!("  eligible                     {recovery_eligible}");
            match recovery {
                Ok(guide) => {
                    println!(
                        "  original_outcome_immutable   {}",
                        guide.original_outcome_immutable
                    );
                    println!(
                        "  automatic_replay_forbidden   {}",
                        guide.automatic_replay_forbidden
                    );
                    println!(
                        "  complete_overwrite_supported {}",
                        guide.complete_overwrite_supported
                    );
                    if !guide.contract_id.is_empty() {
                        println!("  contract                     {}", guide.contract_id);
                        println!("  contract_version             {}", guide.contract_version);
                        println!("  contract_sha256              {}", guide.contract_sha256);
                    }
                    for action in &guide.actions {
                        println!("  action: {action}");
                    }
                }
                Err(error) => println!("  not observable ({})", error.code),
            }
            if let Some(command) = next.first() {
                println!("Next: {command}");
            }
        }
        Output::Json => emit_json!(
            "{{\"schema\":\"arkforge.job/v1\",\"job\":{},\"events\":{},\"receipts\":{},\"recovery\":{},\"next_commands\":{}}}",
            job_json(job),
            match events {
                Ok(all) => format!(
                    "{{\"available\":true,\"complete\":true,\"reason\":null,\"total\":{},\"tail_limit\":{JOB_EVENT_TAIL},\"items\":[{}]}}",
                    all.len(),
                    tail.iter()
                        .map(job_event_json)
                        .collect::<Vec<_>>()
                        .join(",")
                ),
                Err(error) => format!(
                    "{{\"available\":false,\"complete\":false,\"reason\":{},\"total\":null,\"tail_limit\":{JOB_EVENT_TAIL},\"items\":null}}",
                    json(&error.code)
                ),
            },
            match events {
                Ok(_) => format!(
                    "[{}]",
                    receipts
                        .iter()
                        .map(|receipt| receipt_json(receipt))
                        .collect::<Vec<_>>()
                        .join(",")
                ),
                Err(_) => "null".to_string(),
            },
            recovery_json(recovery_eligible, recovery),
            json_array(&next)
        ),
    }
}

fn recovery_json(
    eligible: bool,
    recovery: Result<&RecoveryGuideView, &PublicClientError>,
) -> String {
    match recovery {
        Ok(guide) => format!(
            "{{\"eligible\":{eligible},\"available\":true,\"reason\":null,\"original_outcome_immutable\":{},\"automatic_replay_forbidden\":{},\"complete_overwrite_supported\":{},\"contract\":{},\"actions\":{}}}",
            guide.original_outcome_immutable,
            guide.automatic_replay_forbidden,
            guide.complete_overwrite_supported,
            if guide.contract_id.is_empty() {
                "null".to_string()
            } else {
                format!(
                    "{{\"id\":{},\"version\":{},\"sha256\":{}}}",
                    json(&guide.contract_id),
                    json(&guide.contract_version),
                    json(&guide.contract_sha256)
                )
            },
            json_array(&guide.actions)
        ),
        Err(error) => format!(
            "{{\"eligible\":{eligible},\"available\":false,\"reason\":{},\"original_outcome_immutable\":null,\"automatic_replay_forbidden\":null,\"complete_overwrite_supported\":null,\"contract\":null,\"actions\":null}}",
            json(&error.code)
        ),
    }
}

pub(crate) fn print_job_watch(
    globals: &Globals,
    command: &[&str],
    after_sequence: u64,
    timeout_ms: u64,
    events: &[JobEvent],
    summary: &JobSummary,
    timed_out: bool,
) {
    let cursor = events
        .last()
        .map(|event| event.sequence)
        .unwrap_or(after_sequence);
    let next = if summary.terminal {
        Vec::new()
    } else {
        vec![format!(
            "arkforge watch --job {} --after-sequence {cursor}",
            summary.job_id
        )]
    };
    if globals.jsonl {
        // The disclosure belongs on the metadata record: it describes this
        // invocation, not each event in the stream.
        for (index, record) in render_job_jsonl(
            command,
            after_sequence,
            timeout_ms,
            events,
            summary,
            timed_out,
        )
        .into_iter()
        .enumerate()
        {
            if index == 0 {
                emit(record);
            } else {
                println!("{record}");
            }
        }
        return;
    }
    match globals.output {
        Output::Human => {
            println!(
                "Job events after sequence {after_sequence} ({} returned)",
                events.len()
            );
            for event in events {
                println!(
                    "{}  kind={}  state={}  at_epoch_ms={}  journal_sha256={}",
                    event.sequence,
                    event.kind.as_str(),
                    event.job_state,
                    event.at_epoch_ms,
                    hex_bytes(&event.journal_record_sha256)
                );
                if let Some(request) = &event.control_request {
                    println!(
                        "  control={} step={} request={}",
                        request.action.as_str(),
                        request.step_id,
                        request.request_id
                    );
                }
                if let Some(receipt) = &event.receipt {
                    println!(
                        "  receipt action={} disposition={} verification={}",
                        receipt.action_id, receipt.disposition, receipt.verification_outcome
                    );
                }
            }
            println!("terminal    {}", summary.terminal);
            println!("timed_out  {timed_out}");
            println!("last_sequence {}", summary.last_sequence);
            if let Some(command) = next.first() {
                println!("Next: {command}");
            }
        }
        Output::Json => {
            let events = events
                .iter()
                .map(job_event_json)
                .collect::<Vec<_>>()
                .join(",");
            emit_json!(
                "{{\"schema\":\"arkforge.job-watch/v1\",\"after_sequence\":{},\"timeout_ms\":{},\"timed_out\":{},\"events\":[{}],\"job\":{},\"next_commands\":{}}}",
                after_sequence,
                timeout_ms,
                timed_out,
                events,
                job_json(summary),
                json_array(&next)
            );
        }
    }
}

pub(crate) fn render_job_jsonl(
    command: &[&str],
    after_sequence: u64,
    timeout_ms: u64,
    events: &[JobEvent],
    summary: &JobSummary,
    timed_out: bool,
) -> Vec<String> {
    let cursor = events
        .last()
        .map(|event| event.sequence)
        .unwrap_or(after_sequence);
    let next = if summary.terminal {
        Vec::new()
    } else {
        vec![format!(
            "arkforge watch --job {} --after-sequence {cursor}",
            summary.job_id
        )]
    };
    let mut records = vec![format!(
        "{{\"schema\":\"arkforge.job-stream/v1\",\"record\":\"metadata\",\"stream_sequence\":0,\"command\":{},\"job_id\":{},\"after_sequence\":{},\"timeout_ms\":{}}}",
        json_array(command),
        json(&summary.job_id),
        after_sequence,
        timeout_ms,
    )];
    for (index, event) in events.iter().enumerate() {
        records.push(format!(
            "{{\"schema\":\"arkforge.job-event/v1\",\"record\":\"event\",\"stream_sequence\":{},\"event\":{}}}",
            index + 1,
            job_event_json(event),
        ));
    }
    records.push(format!(
        "{{\"schema\":\"arkforge.command-result/v1\",\"record\":\"terminal\",\"stream_sequence\":{},\"ok\":true,\"command\":{},\"result\":{{\"timed_out\":{},\"job_terminal\":{},\"job\":{}}},\"next_commands\":{}}}",
        events.len() + 1,
        json_array(command),
        timed_out,
        summary.terminal,
        job_json(summary),
        json_array(&next),
    ));
    records
}

fn print_job_human(job: &JobSummary) {
    println!(
        "{}  state={}  terminal={}  steps={}/{}  sequence={}",
        job.job_id,
        job.state,
        job.terminal,
        job.completed_steps,
        job.total_steps,
        job.last_sequence
    );
    println!(
        "  plan={}  plan_sha256={}",
        job.plan_id,
        hex_bytes(&job.plan_sha256)
    );
    if !job.current_step_id.is_empty() {
        println!("  current={}", job.current_step_id);
    }
    if !job.stopped_reason.is_empty() {
        println!("  stopped_reason={}", job.stopped_reason);
    }
}

pub(crate) fn print_key_values_human(title: &str, values: &[KeyValue]) {
    if values.is_empty() {
        return;
    }
    println!("{title}");
    for value in values {
        println!("  {} = {}", value.key, value.value);
    }
}

pub(crate) fn print_effects_human(title: &str, effects: &[Effect]) {
    if effects.is_empty() {
        return;
    }
    println!("{title}");
    for effect in effects {
        println!(
            "  {}  kind={}  start={}  length={}  content_sha256={}",
            effect.target,
            effect.kind,
            effect.range_start,
            effect.range_length,
            effect.content_sha256
        );
    }
}

pub(crate) fn observation_json(observation: &DeviceObservationView) -> String {
    format!("{{{}}}", observation_fields_json(observation))
}

/// The observation members without their enclosing braces, so a composite
/// document can inline them beside its own members instead of restating the
/// field list and letting the two drift.
pub(crate) fn observation_fields_json(observation: &DeviceObservationView) -> String {
    format!(
        "\"observation_id\":{},\"observed_at_epoch_ms\":{},\"mode\":{},\"topology_sha256\":{},\"descriptor_sha256\":{},\"serial_sha256\":{},\"serial_evidence_kind\":{},\"identity_strength\":{},\"malformed_descriptor\":{},\"protocol_identity\":{}",
        json(&observation.observation_id),
        observation.observed_at_epoch_ms,
        json(&observation.mode),
        json(&observation.topology_sha256),
        json(&observation.descriptor_sha256),
        optional_json(
            (!observation.serial_sha256.is_empty()).then_some(observation.serial_sha256.as_str())
        ),
        json(&observation.serial_evidence_kind),
        json(&observation.identity_strength),
        observation.malformed_descriptor,
        key_values_json(&observation.protocol_identity)
    )
}

pub(crate) fn key_values_json(values: &[KeyValue]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| format!(
                "{{\"key\":{},\"value\":{}}}",
                json(&value.key),
                json(&value.value)
            ))
            .collect::<Vec<_>>()
            .join(",")
    )
}

pub(crate) fn steps_json(steps: &[arkforge_ipc::messages::PublicStep]) -> String {
    format!(
        "[{}]",
        steps
            .iter()
            .map(|step| format!(
                "{{\"step_id\":{},\"kind\":{},\"effect\":{},\"cancellation\":{},\"binding\":{},\"semantic_target\":{},\"content_sha256\":{},\"expected_mode_before\":{},\"expected_mode_after\":{},\"private_action_sha256\":{}}}",
                json(&step.step_id),
                json(&step.kind),
                json(&step.effect),
                json(&step.cancellation),
                json(&step.binding),
                json(&step.semantic_target),
                json(&step.content_sha256),
                json(&step.expected_mode_before),
                json(&step.expected_mode_after),
                json(&step.private_action_sha256)
            ))
            .collect::<Vec<_>>()
            .join(",")
    )
}

pub(crate) fn effects_json(effects: &[Effect]) -> String {
    format!(
        "[{}]",
        effects
            .iter()
            .map(|effect| format!(
                "{{\"kind\":{},\"target\":{},\"range_start\":{},\"range_length\":{},\"content_sha256\":{}}}",
                json(&effect.kind),
                json(&effect.target),
                effect.range_start,
                effect.range_length,
                json(&effect.content_sha256)
            ))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn job_json(job: &JobSummary) -> String {
    format!(
        "{{\"job_id\":{},\"plan_id\":{},\"plan_sha256\":{},\"state\":{},\"terminal\":{},\"current_step_id\":{},\"completed_steps\":{},\"total_steps\":{},\"last_sequence\":{},\"stopped_reason\":{}}}",
        json(&job.job_id),
        json(&job.plan_id),
        json(&hex_bytes(&job.plan_sha256)),
        json(&job.state),
        job.terminal,
        optional_json((!job.current_step_id.is_empty()).then_some(job.current_step_id.as_str())),
        job.completed_steps,
        job.total_steps,
        job.last_sequence,
        optional_json((!job.stopped_reason.is_empty()).then_some(job.stopped_reason.as_str()))
    )
}

fn receipt_json(receipt: &ActionReceiptSummary) -> String {
    format!(
        "{{\"job_id\":{},\"plan_id\":{},\"step_id\":{},\"action_id\":{},\"attempt_id\":{},\"permit_id\":{},\"disposition\":{},\"evidence_sha256\":{},\"verification_outcome\":{},\"verification_strength\":{},\"verified_range_start\":{},\"verified_range_length\":{},\"typed_skip_reason\":{},\"failure_classification\":{},\"facts\":{}}}",
        json(&receipt.job_id),
        json(&receipt.plan_id),
        json(&receipt.step_id),
        json(&receipt.action_id),
        json(&receipt.attempt_id),
        json(&receipt.permit_id),
        json(&receipt.disposition),
        json(&hex_bytes(&receipt.evidence_sha256)),
        json(&receipt.verification_outcome),
        json(&receipt.verification_strength),
        receipt.verified_range_start,
        receipt.verified_range_length,
        optional_json(
            (!receipt.typed_skip_reason.is_empty()).then_some(receipt.typed_skip_reason.as_str())
        ),
        optional_json(
            (!receipt.failure_classification.is_empty())
                .then_some(receipt.failure_classification.as_str())
        ),
        key_values_json(&receipt.facts)
    )
}

pub(crate) fn job_event_json(event: &JobEvent) -> String {
    let admission = event
        .admission
        .as_ref()
        .map(|admission| {
            format!(
                "{{\"job_id\":{},\"plan_id\":{},\"plan_sha256\":{},\"step_id\":{},\"attempt_id\":{},\"public_step_sha256\":{},\"private_action_sha256\":{},\"effect_set_sha256\":{},\"admitted_device_facts_sha256\":{},\"observed_mode\":{},\"observed_at_epoch_ms\":{},\"snapshot_lifetime_ms\":{},\"request_id\":{},\"topology_sha256\":{},\"descriptor_sha256\":{},\"serial_sha256\":{},\"serial_evidence_kind\":{},\"protocol_identity\":{},\"identity_strength\":{},\"malformed_descriptor\":{},\"transport_session_sha256\":{}}}",
                json(&admission.job_id),
                json(&admission.plan_id),
                json(&hex_bytes(&admission.plan_sha256)),
                json(&admission.step_id),
                json(&admission.attempt_id),
                json(&hex_bytes(&admission.public_step_sha256)),
                json(&hex_bytes(&admission.private_action_sha256)),
                json(&hex_bytes(&admission.effect_set_sha256)),
                json(&hex_bytes(&admission.admitted_device_facts_sha256)),
                json(&admission.observed_mode),
                admission.observed_at_epoch_ms,
                admission.snapshot_lifetime_ms,
                json(&admission.request_id),
                json(&hex_bytes(&admission.topology_sha256)),
                json(&hex_bytes(&admission.descriptor_sha256)),
                json(&hex_bytes(&admission.serial_sha256)),
                json(&admission.serial_evidence_kind),
                key_values_json(&admission.protocol_identity),
                json(&admission.identity_strength),
                admission.malformed_descriptor,
                json(&hex_bytes(&admission.transport_session_sha256))
            )
        })
        .unwrap_or_else(|| "null".into());
    let control = event
        .control_request
        .as_ref()
        .map(|request| {
            format!(
                "{{\"job_id\":{},\"step_id\":{},\"request_id\":{},\"action\":{},\"permit_id\":{},\"expected_facts\":{},\"deadline_epoch_ms\":{}}}",
                json(&request.job_id),
                json(&request.step_id),
                json(&request.request_id),
                json(request.action.as_str()),
                json(&request.permit_id),
                key_values_json(&request.expected_facts),
                request.deadline_epoch_ms
            )
        })
        .unwrap_or_else(|| "null".into());
    let receipt = event
        .receipt
        .as_ref()
        .map(receipt_json)
        .unwrap_or_else(|| "null".into());
    format!(
        "{{\"job_id\":{},\"sequence\":{},\"kind\":{},\"at_epoch_ms\":{},\"journal_record_sha256\":{},\"job_state\":{},\"admission\":{},\"control_request\":{},\"receipt\":{},\"facts\":{}}}",
        json(&event.job_id),
        event.sequence,
        json(event.kind.as_str()),
        event.at_epoch_ms,
        json(&hex_bytes(&event.journal_record_sha256)),
        json(&event.job_state),
        admission,
        control,
        receipt,
        key_values_json(&event.facts)
    )
}
