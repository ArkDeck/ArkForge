//! `flash` command, including plan resolution.

use super::*;

pub(crate) fn run_flash(arguments: &[String], globals: Globals) -> Result<i32, CliError> {
    // A bare `arkforge flash` is the one-step verb, not a help page: the
    // shortest thing an operator can type should be the thing they came to do.
    let Some(subcommand) = arguments.first() else {
        return run_flash_run(&[], globals);
    };
    if subcommand == "run" {
        return run_flash_run(&arguments[1..], globals);
    }
    let options = Options::parse(&arguments[1..])?;
    match subcommand.as_str() {
        "plan" => run_flash_plan(&options, globals),
        "restore-ramdisk" => run_flash_plan_intent(&options, globals, "restoreRamdisk"),
        "verify" => run_flash_verify(&options, globals),
        other => Err(CliError::invalid(format!(
            "Unknown flash command {other:?}. Run 'arkforge help flash'."
        ))),
    }
}

fn run_flash_verify(options: &Options, globals: Globals) -> Result<i32, CliError> {
    let job = options.one("job")?;
    let campaign = options.optional_one("hardware-campaign")?;
    let budget = options
        .optional_one("budget-ms")?
        .unwrap_or("300000")
        .parse::<u64>()
        .map_err(|_| CliError::invalid("--budget-ms must be an integer."))?;
    if !(1000..=600000).contains(&budget) {
        return Err(CliError::invalid("--budget-ms must be 1000..600000."));
    }
    let runtime_dir = command_runtime_dir(&globals)?;
    ensure_runtime(&globals, campaign)?;
    let result = supervisor::verify_flash_preboot(
        &runtime_dir,
        job,
        budget,
        campaign,
        options.optional_one("preboot-ramdisk")?.unwrap_or_default(),
    )?;
    match globals.output {
        Output::Json => emit(result.document.clone()),
        Output::Human => {
            println!(
                "Verification of {job}: {} full hashes, {} typed skips, {} failures.",
                result.verified, result.skipped, result.failed
            );
            println!(
                "Record: {}",
                runtime_dir
                    .join("authority")
                    .join("flash-verifications")
                    .join(&result.record_id)
                    .join("result.json")
                    .display()
            );
        }
    }
    Ok(result.exit_code())
}
/// The whole flash, in one command.
///
/// Every stage before the consent gate reads, or writes only host storage, so
/// the frontend performs them. The gate itself is never inferred: an operator
/// at a terminal accepts named effects on a confirmation screen, and a script
/// accepts them with exact `--ack` tokens. Nothing else changes — the same
/// sealed plan, the same digest equality, the same exact token set, and the
/// same journal `arkforged` would have written either way.
fn run_flash_run(arguments: &[String], globals: Globals) -> Result<i32, CliError> {
    let interactive = interaction::open_for(globals.output == Output::Human, globals.no_input);
    let (positional, rest) = match arguments.first() {
        Some(first) if !first.starts_with("--") => (Some(first.clone()), &arguments[1..]),
        _ => (None, arguments),
    };
    let mut options = Options::parse(rest)?;
    if let Some(file) = positional {
        options.insert("file", file);
    }
    let registry = profile_registry()?;
    let runtime_dir = command_runtime_dir(&globals)?;
    let campaign = options
        .optional_one("hardware-campaign")?
        .map(str::to_string);
    let detach = options.flag("detach");
    ensure_runtime(&globals, campaign.as_deref())?;

    let mut terminal = interaction::TerminalPrompt;
    let resolved = {
        let prompt: Option<&mut dyn interaction::Prompt> =
            interactive.then_some(&mut terminal as &mut dyn interaction::Prompt);
        resolve_plan_inputs(&globals, &registry, &options, true, prompt)?
    };
    let mut partial = PartialResolution::from_resolved(&resolved);

    let assessment = supervisor::assess_plan(
        &runtime_dir,
        &resolved.artifact.artifact_id,
        &resolved.profile.reference,
        &resolved.device.observation.observation_id,
    )
    .map_err(|error| {
        partial.refuse(&error.code, error.message, error.exit_code, error.retryable)
    })?;
    if !assessment_is_executable(&assessment) {
        return Err(plan_unavailable(&partial, &assessment));
    }
    let materialized = supervisor::materialize_plan(
        &runtime_dir,
        &resolved.artifact.artifact_id,
        &resolved.profile.reference,
        &resolved.device.observation.observation_id,
    )
    .map_err(|error| {
        partial.refuse(&error.code, error.message, error.exit_code, error.retryable)
    })?;
    let plan = match materialized {
        MaterializePlanResponse::Plan(plan) => plan,
        MaterializePlanResponse::Assessment(assessment) => {
            return Err(plan_unavailable(&partial, &assessment));
        }
    };
    partial.sealed_campaign = sealed_campaign(&plan);
    let tokens = plan_acknowledgements(&plan);

    // The one decision that is never inferred.
    let first_flash_key = first_flash_key_for(&registry, &resolved);
    let consent = if interactive {
        confirm_on_terminal(
            &mut terminal,
            &runtime_dir,
            &registry,
            &resolved,
            &assessment,
            &plan,
            &tokens,
            first_flash_key.as_deref(),
        )?
    } else {
        accept_from_arguments(&options, &tokens, &partial, &assessment, &plan)?
    };

    // Durable before dispatch, never after: an execution nobody can prove was
    // approved is worse than one that did not happen.
    let approval = ApprovalRecord {
        plan_id: plan.plan_id.clone(),
        plan_sha256: plan.plan_sha256.clone(),
        tokens: tokens.clone(),
        provenance: consent.0,
        model_assertion: consent.1,
        hardware_campaign: partial.sealed_campaign,
        recorded_at_epoch_ms: now_epoch_ms()?,
    };
    let approval_id = approval::record(&runtime_dir, &approval)?;

    let job_id = supervisor::apply_plan(
        &runtime_dir,
        &plan.plan_id,
        &plan.plan_sha256,
        &tokens,
        detach,
    )?;
    if detach {
        match globals.output {
            Output::Human => {
                println!("Started durable job {job_id} (approval {approval_id}).");
                println!("Next: arkforge watch --job {job_id}");
            }
            Output::Json => emit_json!(
                "{{\"schema\":\"arkforge.flash-run/v1\",\"job_id\":{},\"approval_id\":{},\"detached\":true,\"authority_continues\":true,\"next_commands\":[{}]}}",
                json(&job_id),
                json(&approval_id),
                json(&format!("arkforge watch --job {job_id}")),
            ),
        }
        return Ok(0);
    }
    let (events, summary, _) = job::watch_job(&globals, &job_id, 0, u64::MAX)?;
    job::print_job_watch(
        &globals,
        &["flash", "run"],
        0,
        u64::MAX,
        &events,
        &summary,
        false,
    );
    if summary.state == "succeeded"
        && let Some(key) = first_flash_key
    {
        // Only a completed flash spends the first confirmation; a failure or an
        // interruption leaves the screen owed next time.
        approval::record_first_flash(&runtime_dir, &key)?;
    }
    Ok(match summary.state.as_str() {
        "succeeded" => 0,
        "outcomeUnknown" => 8,
        "cancelledSafe" => 7,
        _ if summary.terminal => 7,
        _ => 9,
    })
}

/// The remembered-pair key for this target, or `None` when nothing about the
/// board is provable enough to remember.
fn first_flash_key_for(
    registry: &inference::ProfileRegistry,
    resolved: &Resolved,
) -> Option<String> {
    let identity = resolved.device.identification.physical_identity_digest()?;
    let profile = registry.find(&resolved.profile.reference)?;
    let digest = profile.digest().ok()?;
    Some(approval::first_flash_key(&identity, &digest.to_hex()))
}

/// The confirmation screen, and what it will accept.
#[allow(clippy::too_many_arguments)]
fn confirm_on_terminal(
    prompt: &mut dyn interaction::Prompt,
    runtime_dir: &Path,
    registry: &inference::ProfileRegistry,
    resolved: &Resolved,
    assessment: &Assessment,
    plan: &ExecutablePlan,
    tokens: &[String],
    first_flash_key: Option<&str>,
) -> Result<(Provenance, Option<String>), CliError> {
    let identification = &resolved.device.identification;
    prompt.show("");
    prompt.show("About to write this device:");
    prompt.show(&format!(
        "  device      {}  mode={}",
        resolved.device.observation.observation_id, resolved.device.observation.mode
    ));
    prompt.show(&format!(
        "  model       {}  strength={}",
        identification.model.as_deref().unwrap_or("UNPROVEN"),
        identification.strength.as_str()
    ));
    prompt.show(&format!(
        "  evidence    {}",
        identification.evidence.join(", ")
    ));
    prompt.show(&format!(
        "  profile     {} ({})",
        resolved.profile.reference, resolved.profile.resolution
    ));
    prompt.show(&format!(
        "  intent      {} ({})",
        resolved.intent.value, resolved.intent.resolution
    ));
    prompt.show(&format!(
        "  firmware    {}  {}",
        resolved.artifact.manifest.format_id, resolved.artifact.artifact_id
    ));
    if let Some(campaign) = sealed_campaign(plan) {
        prompt.show(&format!("  campaign    {campaign}"));
    }
    prompt.show(&format!("  plan        {}", plan.plan_sha256));
    prompt.show(&format!(
        "  persistent effects ({})",
        plan.persistent_effects.len()
    ));
    for effect in &plan.persistent_effects {
        prompt.show(&format!("    {} {}", effect.kind, effect.target));
    }
    for unknown in &assessment.unknowns {
        prompt.show(&format!("  unknown     {}={}", unknown.key, unknown.value));
    }
    prompt.show(&format!("  accepting   {}", tokens.join(", ")));

    let strong = identification.strength == inference::Strength::Strong;
    let first = first_flash_key.is_none_or(|key| approval::is_first_flash(runtime_dir, key));
    let models = registry
        .find(&resolved.profile.reference)
        .map(|profile| profile.product_models.clone())
        .unwrap_or_default();
    let confirmation = interaction::Confirmation::required(strong, first, &models);
    let answer = prompt.ask(&confirmation.question()).unwrap_or_default();
    if !confirmation.accepts(&answer) {
        return Err(CliError::new(
            "CONSENT_DECLINED",
            "The confirmation was not accepted; nothing was dispatched.",
            4,
            true,
        ));
    }
    let model_assertion = match confirmation {
        interaction::Confirmation::TypeModel(_) => Some(answer),
        interaction::Confirmation::Acknowledge => None,
    };
    Ok((Provenance::InteractiveTty, model_assertion))
}

/// Consent from a script: exactly the sealed tokens, no more and no fewer.
fn accept_from_arguments(
    options: &Options,
    tokens: &[String],
    partial: &PartialResolution,
    assessment: &Assessment,
    plan: &ExecutablePlan,
) -> Result<(Provenance, Option<String>), CliError> {
    let supplied = options.many("ack");
    let unexpected = supplied
        .iter()
        .filter(|token| !tokens.contains(token))
        .cloned()
        .collect::<Vec<_>>();
    if !unexpected.is_empty() {
        return Err(partial.refuse_with(
            "UNEXPECTED_ACKNOWLEDGEMENT",
            format!(
                "This plan does not require [{}]; supply exactly [{}].",
                unexpected.join(", "),
                tokens.join(", ")
            ),
            4,
            false,
            Some(assessment),
            Some(plan),
        ));
    }
    let missing = tokens
        .iter()
        .filter(|token| !supplied.contains(token))
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        // The plan is already sealed. The way forward is to execute that one,
        // not to seal a second by running this command again with tokens.
        let command = apply_command(
            plan,
            partial.sealed_campaign.as_deref(),
            &partial.extra_acknowledgements,
        );
        return Err(partial
            .refuse_with(
                "ACKNOWLEDGEMENT_REQUIRED",
                format!(
                    "This plan requires [{}]. It is sealed; accept it with the returned apply command.",
                    tokens.join(", ")
                ),
                4,
                true,
                Some(assessment),
                Some(plan),
            )
            .with_retry(command, tokens.to_vec()));
    }
    Ok((Provenance::Argv, None))
}

/// One staging call: import, identify, assess, and seal.
///
/// The stages are joined here rather than left to the caller because every one
/// of them only reads, or only writes host storage. The single step that cannot
/// be inferred — consent to a named destructive effect — stays where it was.
fn run_flash_plan(options: &Options, globals: Globals) -> Result<i32, CliError> {
    run_flash_plan_intent(options, globals, "fullRestore")
}

fn run_flash_plan_intent(
    options: &Options,
    globals: Globals,
    intent: &str,
) -> Result<i32, CliError> {
    let assess_only = options.flag("assess-only");
    let registry = profile_registry()?;
    let runtime_dir = command_runtime_dir(&globals)?;
    ensure_runtime(&globals, options.optional_one("hardware-campaign")?)?;
    let mut resolved = resolve_plan_inputs(&globals, &registry, options, !assess_only, None)?;
    if intent == "restoreRamdisk" {
        resolved.intent.value = "restore-ramdisk".into();
    }
    let mut partial = PartialResolution::from_resolved(&resolved);

    let source_job = options.optional_one("source-job")?;
    let assessment = supervisor::assess_plan_from_source(
        &runtime_dir,
        &resolved.artifact.artifact_id,
        &resolved.profile.reference,
        &resolved.device.observation.observation_id,
        intent,
        source_job,
    )
    .map_err(|error| {
        partial.refuse(&error.code, error.message, error.exit_code, error.retryable)
    })?;
    let executable = assessment_is_executable(&assessment);

    if assess_only {
        // The assessment is the answer, so producing one is success even when
        // it says the device cannot be flashed by this build.
        print_flash_plan_v2(globals.output, &resolved, &partial, Some(&assessment), None);
        return Ok(0);
    }

    if !executable {
        return Err(plan_unavailable(&partial, &assessment));
    }
    let materialized = supervisor::materialize_plan_from_source(
        &runtime_dir,
        &resolved.artifact.artifact_id,
        &resolved.profile.reference,
        &resolved.device.observation.observation_id,
        intent,
        source_job,
    )
    .map_err(|error| {
        partial.refuse(&error.code, error.message, error.exit_code, error.retryable)
    })?;
    match materialized {
        MaterializePlanResponse::Plan(plan) => {
            partial.sealed_campaign = sealed_campaign(&plan);
            print_flash_plan_v2(
                globals.output,
                &resolved,
                &partial,
                Some(&assessment),
                Some(&plan),
            );
            Ok(0)
        }
        // The gate closed between assessing and sealing. The second assessment
        // is the current truth, so it is the one reported.
        MaterializePlanResponse::Assessment(assessment) => {
            Err(plan_unavailable(&partial, &assessment))
        }
    }
}

fn assessment_is_executable(assessment: &Assessment) -> bool {
    assessment.availability == "available"
        && execution_support_state_permits(&assessment.mechanics_maturity_state)
        && execution_support_state_permits(&assessment.authority_support_state)
}

/// The campaign a sealed plan carries, if any.
pub(crate) fn sealed_campaign(plan: &ExecutablePlan) -> Option<String> {
    [
        plan.mechanics_maturity_campaign.as_str(),
        plan.authority_support_campaign.as_str(),
    ]
    .into_iter()
    .find(|campaign| !campaign.is_empty())
    .map(str::to_string)
}

/// Refuses a plan while keeping every fact the failed call already established.
fn plan_unavailable(partial: &PartialResolution, assessment: &Assessment) -> CliError {
    CliError::new(
        "PLAN_UNAVAILABLE",
        format!(
            "No executable plan was created: {}",
            if assessment.unavailable_reason.is_empty() {
                assessment.availability.clone()
            } else {
                assessment.unavailable_reason.clone()
            }
        ),
        3,
        false,
    )
    .with_facts(format!(
        "{{\"flash_plan\":{}}}",
        flash_plan_v2_json(partial, Some(assessment), None)
    ))
}

/// A campaign is named for this call, or it is not in play.
///
/// A running campaign runtime is never inherited silently and never restarted
/// to match an argument: a named acceptance run means nothing if a command can
/// wander into or out of one without saying so.
pub(crate) fn require_campaign_acknowledgement(
    runtime_dir: &Path,
    given: Option<&str>,
) -> Result<(), CliError> {
    // A failed read is not evidence that the runtime has no campaign.
    // Keep the gate closed if the status socket disappears or fails decoding.
    let status = supervisor::status(runtime_dir)?;
    let running = (!status.hardware_campaign.is_empty()).then_some(status.hardware_campaign);
    match (running.as_deref(), given) {
        (None, None) => Ok(()),
        (Some(running), Some(given)) if running == given => Ok(()),
        (Some(running), None) => Err(CliError::new(
            "CAMPAIGN_ACKNOWLEDGEMENT_REQUIRED",
            format!(
                "This runtime serves hardware campaign {running}. Name it for this call with --hardware-campaign {running}; it is never inherited."
            ),
            3,
            false,
        )),
        (Some(running), Some(given)) => Err(CliError::new(
            "RUNTIME_CAMPAIGN_MISMATCH",
            format!("The running runtime serves hardware campaign {running}, not {given}."),
            6,
            false,
        )),
        (None, Some(given)) => Err(CliError::new(
            "RUNTIME_CAMPAIGN_MISMATCH",
            format!(
                "The running runtime has no hardware campaign, so it cannot serve campaign {given}."
            ),
            6,
            false,
        )),
    }
}

/// The declared upper bound on the `device_candidates` refusal projection,
/// matching what `flash plan` publishes in its help.
const MAX_DEVICE_CANDIDATE_FACTS: usize = 32;

pub(crate) struct ResolvedArtifact {
    pub(crate) artifact_id: String,
    manifest: InspectArtifactResponse,
    imported: bool,
    compatible_profiles: Vec<String>,
}

pub(crate) struct ResolvedProfile {
    pub(crate) reference: String,
    resolution: &'static str,
}

struct ResolvedIntent {
    value: String,
    resolution: &'static str,
}

pub(crate) struct Resolved {
    pub(crate) artifact: ResolvedArtifact,
    pub(crate) device: inference::Candidate,
    pub(crate) profile: ResolvedProfile,
    intent: ResolvedIntent,
}

/// What resolution had established when it finished — or when it gave up.
///
/// A refusal carries this so the failure path returns the same facts the
/// success path would have, and the caller does not have to re-query to learn
/// how far the command got.
#[derive(Default)]
pub(crate) struct PartialResolution {
    artifact: Option<String>,
    device: Option<String>,
    profile: Option<String>,
    intent: Option<String>,
    candidates: Vec<String>,
    pub(crate) sealed_campaign: Option<String>,
    /// Tokens the plan's origin requires beyond its own effects.
    pub(crate) extra_acknowledgements: Vec<String>,
}

impl PartialResolution {
    pub(crate) fn from_resolved(resolved: &Resolved) -> Self {
        Self {
            artifact: Some(resolved_artifact_json(&resolved.artifact)),
            device: Some(resolved_device_json(&resolved.device)),
            profile: Some(format!(
                "{{\"reference\":{},\"resolution\":{}}}",
                json(&resolved.profile.reference),
                json(resolved.profile.resolution)
            )),
            intent: Some(format!(
                "{{\"value\":{},\"resolution\":{}}}",
                json(&resolved.intent.value),
                json(resolved.intent.resolution)
            )),
            candidates: Vec::new(),
            sealed_campaign: None,
            extra_acknowledgements: Vec::new(),
        }
    }

    pub(crate) fn refuse(
        &self,
        code: &str,
        message: impl Into<String>,
        exit_code: i32,
        retryable: bool,
    ) -> CliError {
        self.refuse_with(code, message, exit_code, retryable, None, None)
    }

    pub(crate) fn refuse_with(
        &self,
        code: &str,
        message: impl Into<String>,
        exit_code: i32,
        retryable: bool,
        assessment: Option<&Assessment>,
        plan: Option<&ExecutablePlan>,
    ) -> CliError {
        // The candidate list is bounded to what this command's help declares.
        // The full count travels with it, so a truncated list is never mistaken
        // for the whole field of candidates.
        let listed = self
            .candidates
            .iter()
            .take(MAX_DEVICE_CANDIDATE_FACTS)
            .cloned()
            .collect::<Vec<_>>();
        CliError::new(code, message, exit_code, retryable).with_facts(format!(
            "{{\"flash_plan\":{},\"device_candidates\":[{}],\"device_candidates_total\":{}}}",
            flash_plan_v2_json(self, assessment, plan),
            listed.join(","),
            self.candidates.len()
        ))
    }
}

fn resolved_artifact_json(artifact: &ResolvedArtifact) -> String {
    format!(
        "{{\"artifact_id\":{},\"sha256\":{},\"format\":{},\"imported\":{},\"manifest_summary\":{},\"compatible_profiles\":{}}}",
        json(&artifact.artifact_id),
        json(&artifact.manifest.content_sha256),
        json(&artifact.manifest.format_id),
        artifact.imported,
        artifact::manifest_summary_json(&artifact.manifest),
        json_strings(&artifact.compatible_profiles),
    )
}

fn resolved_device_json(candidate: &inference::Candidate) -> String {
    format!(
        "{{\"observation_id\":{},\"mode\":{},\"identity_strength\":{},\"identification\":{}}}",
        json(&candidate.observation.observation_id),
        json(&candidate.observation.mode),
        json(&candidate.observation.identity_strength),
        candidate.identification.to_json(json),
    )
}

/// Resolves everything a plan needs, refusing rather than guessing.
///
/// `materializing` gates the identity rule: sealing a plan against a board this
/// build cannot name requires the caller to name it, while a read-only
/// assessment may proceed and say plainly that the model is unproven.
/// Hands the same operator to a nested step without giving up this one's.
fn reborrow<'a>(
    prompt: &'a mut Option<&mut dyn interaction::Prompt>,
) -> Option<&'a mut (dyn interaction::Prompt + 'a)> {
    match prompt {
        Some(prompt) => Some(*prompt),
        None => None,
    }
}

pub(crate) fn resolve_plan_inputs(
    globals: &Globals,
    registry: &inference::ProfileRegistry,
    options: &Options,
    materializing: bool,
    mut prompt: Option<&mut dyn interaction::Prompt>,
) -> Result<Resolved, CliError> {
    let interactive = prompt.is_some();
    let mut partial = PartialResolution::default();

    // 1. Content. Bytes always enter the content store before anything binds
    //    to them, whether the caller passed a path or an artifact id.
    let artifact = resolve_artifact(globals, registry, options, reborrow(&mut prompt))?;
    partial.artifact = Some(resolved_artifact_json(&artifact));
    if artifact.compatible_profiles.is_empty() {
        return Err(partial.refuse(
            "PROFILE_AMBIGUOUS",
            format!(
                "No loaded profile declares artifact format {}.",
                artifact.manifest.format_id
            ),
            6,
            false,
        ));
    }

    // 2. Device.
    let explicit_device = options.optional_one("device")?;
    let target = options.optional_one("target")?;
    if explicit_device.is_some() && target.is_some() {
        return Err(CliError::invalid(
            "--device names one exact observation and --target searches for one; supply at most one.",
        ));
    }
    let wait_device_ms = options
        .optional_one("wait-device")?
        .map(|value| parse_u64("--wait-device", value))
        .transpose()?
        .unwrap_or(0);
    let device = resolve_device(
        globals,
        registry,
        &artifact,
        explicit_device,
        target,
        wait_device_ms,
        &mut partial,
        reborrow(&mut prompt),
    )?;
    partial.device = Some(resolved_device_json(&device));

    // 3. Profile: the intersection of what the firmware fits and what the
    //    device could be.
    let compatible = device
        .identification
        .compatible_profiles
        .iter()
        .filter(|profile| artifact.compatible_profiles.contains(profile))
        .cloned()
        .collect::<Vec<_>>();
    let profile = match options.optional_one("profile")? {
        Some(explicit) => {
            if !compatible.iter().any(|profile| profile == explicit) {
                return Err(partial.refuse(
                    "PROFILE_INCOMPATIBLE",
                    format!(
                        "Profile {explicit} is not in the compatible set for this firmware and device: [{}].",
                        compatible.join(", ")
                    ),
                    3,
                    false,
                ));
            }
            ResolvedProfile {
                reference: explicit.to_string(),
                resolution: "explicit",
            }
        }
        None => match compatible.len() {
            1 => ResolvedProfile {
                reference: compatible[0].clone(),
                resolution: "inferred",
            },
            _ => {
                return Err(partial.refuse(
                    "PROFILE_AMBIGUOUS",
                    format!(
                        "The firmware declares [{}] and the device matches [{}]; their intersection is [{}], which is not exactly one profile.",
                        artifact.compatible_profiles.join(", "),
                        device.identification.compatible_profiles.join(", "),
                        compatible.join(", ")
                    ),
                    6,
                    false,
                ));
            }
        },
    };
    partial.profile = Some(format!(
        "{{\"reference\":{},\"resolution\":{}}}",
        json(&profile.reference),
        json(profile.resolution)
    ));

    // 4. Identity gate. A compatible profile is not a proven board, and a
    //    caller asserting one does not make the evidence stronger — it only
    //    makes the assertion theirs. An operator at a terminal answers this at
    //    the confirmation screen instead, by typing the model out.
    if materializing
        && !interactive
        && device.identification.strength != inference::Strength::Strong
        && !(explicit_device.is_some() && options.optional_one("profile")?.is_some())
    {
        return Err(partial.refuse(
            "IDENTITY_CONFIRMATION_REQUIRED",
            format!(
                "This build cannot prove which board {} is (strength {}, model unproven). Sealing a destructive plan against it requires an explicit --profile and an exact --device.",
                device.observation.observation_id,
                device.identification.strength.as_str()
            ),
            3,
            false,
        ));
    }

    // 5. Intent.
    let profile_document = registry.find(&profile.reference).ok_or_else(|| {
        CliError::new(
            "PROFILE_NOT_FOUND",
            format!("This build has no profile {}.", profile.reference),
            5,
            false,
        )
    })?;
    let legal = inference::legal_intents(profile_document, &artifact.manifest.format_id);
    let intent = match options.optional_one("intent")? {
        Some(explicit) => {
            if !legal.contains(&explicit) {
                return Err(partial.refuse(
                    "INTENT_UNAVAILABLE",
                    format!(
                        "Profile {} and format {} admit [{}], not {explicit:?}.",
                        profile.reference,
                        artifact.manifest.format_id,
                        legal.join(", ")
                    ),
                    3,
                    false,
                ));
            }
            ResolvedIntent {
                value: explicit.to_string(),
                resolution: "explicit",
            }
        }
        None => match legal.as_slice() {
            [only] => ResolvedIntent {
                value: (*only).to_string(),
                resolution: "defaulted",
            },
            _ => {
                return Err(partial.refuse(
                    "INTENT_REQUIRED",
                    format!(
                        "Profile {} and format {} admit [{}]; supply --intent.",
                        profile.reference,
                        artifact.manifest.format_id,
                        legal.join(", ")
                    ),
                    3,
                    false,
                ));
            }
        },
    };

    Ok(Resolved {
        artifact,
        device,
        profile,
        intent,
    })
}

/// Firmware an operator can pick from a line list.
enum Content {
    /// Bytes already in the content store.
    Stored(String),
    /// A file in the current directory, still to be imported.
    File(String),
}

/// Offers the firmware this host already has, plus what is in front of the
/// operator right now.
///
/// One directory level and known container suffixes only: this is a line list,
/// not a file browser, and a firmware image somewhere else is named with
/// `--file` rather than navigated to.
fn select_content(
    globals: &Globals,
    prompt: &mut dyn interaction::Prompt,
) -> Result<Content, CliError> {
    let mut choices = Vec::new();
    let mut values = Vec::new();
    for (digest, size) in list_artifacts(globals)? {
        values.push(Content::Stored(digest.to_hex()));
        choices.push(interaction::Choice {
            value: digest.to_hex(),
            label: format!("stored  {}  {size} bytes", digest.to_hex()),
        });
    }
    if let Ok(entries) = std::fs::read_dir(".") {
        let mut local = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_file())
            .map(|entry| entry.path())
            .filter(|path| {
                let name = path.to_string_lossy().to_ascii_lowercase();
                name.ends_with(".tar.gz") || name.ends_with(".pac")
            })
            .collect::<Vec<_>>();
        local.sort();
        for path in local {
            let display = path.display().to_string();
            values.push(Content::File(display.clone()));
            choices.push(interaction::Choice {
                value: display.clone(),
                label: format!("file    {display}"),
            });
        }
    }
    if choices.is_empty() {
        return Err(CliError::new(
            "CONTENT_REQUIRED",
            "No firmware is stored in this runtime and none is in the current directory; supply --file <path>.",
            2,
            false,
        ));
    }
    let selected = interaction::select(prompt, "Select the firmware to write:", &choices)
        .ok_or_else(|| CliError::new("CONTENT_REQUIRED", "No firmware was selected.", 2, true))?;
    let index = choices
        .iter()
        .position(|choice| choice.value == selected)
        .expect("the selection came from the choice list");
    Ok(match &values[index] {
        Content::Stored(id) => Content::Stored(id.clone()),
        Content::File(path) => Content::File(path.clone()),
    })
}

/// Brings the firmware into the content store and reports what it is.
fn resolve_artifact(
    globals: &Globals,
    registry: &inference::ProfileRegistry,
    options: &Options,
    prompt: Option<&mut dyn interaction::Prompt>,
) -> Result<ResolvedArtifact, CliError> {
    let mut file = options.optional_one("file")?.map(str::to_string);
    let mut artifact = options.optional_one("artifact")?.map(str::to_string);
    if file.is_none()
        && artifact.is_none()
        && let Some(prompt) = prompt
    {
        match select_content(globals, prompt)? {
            Content::Stored(id) => artifact = Some(id),
            Content::File(path) => file = Some(path),
        }
    }
    let file = file.as_deref();
    let artifact = artifact.as_deref();
    if file.is_some() && artifact.is_some() {
        return Err(CliError::invalid(
            "--file imports bytes and --artifact names bytes already imported; supply at most one.",
        ));
    }
    let (digest, imported, store) = match (file, artifact) {
        (Some(path), _) => {
            let path = Path::new(path);
            let metadata = std::fs::metadata(path).map_err(|error| {
                CliError::new(
                    "ARTIFACT_FILE_NOT_FOUND",
                    format!("Cannot read firmware input {}: {error}", path.display()),
                    5,
                    false,
                )
            })?;
            if !metadata.is_file() {
                return Err(CliError::invalid(format!(
                    "--file must name one regular file, not {}.",
                    path.display()
                )));
            }
            let store = open_artifact_store(globals)?;
            let input = File::open(path).map_err(|error| {
                CliError::new(
                    "ARTIFACT_FILE_NOT_FOUND",
                    format!("Cannot open firmware input {}: {error}", path.display()),
                    5,
                    false,
                )
            })?;
            // The bytes are addressed before the plan can name them: --file is
            // an implicit import, never a path the plan carries.
            let object = store
                .import(input, metadata.len(), None)
                .map_err(artifact_store_error)?;
            (object.digest, !object.deduplicated, store)
        }
        (None, Some(artifact)) => {
            let digest = parse_digest("--artifact", artifact)?;
            (digest, false, open_existing_artifact_store(globals)?)
        }
        (None, None) => {
            return Err(CliError::new(
                "CONTENT_REQUIRED",
                "Supply the firmware as --file <path> or --artifact <artifact-id>.",
                2,
                false,
            ));
        }
    };
    let manifest = manifest_response(&artifact::stored_manifest(&store, &digest)?);
    let compatible_profiles = registry.compatible_with_format(&manifest.format_id);
    Ok(ResolvedArtifact {
        artifact_id: digest.to_hex(),
        manifest,
        imported,
        compatible_profiles,
    })
}

/// Binds exactly one observation, or refuses with every candidate it saw.
#[allow(clippy::too_many_arguments)]
fn resolve_device(
    globals: &Globals,
    registry: &inference::ProfileRegistry,
    artifact: &ResolvedArtifact,
    explicit_device: Option<&str>,
    target: Option<&str>,
    wait_device_ms: u64,
    partial: &mut PartialResolution,
    mut prompt: Option<&mut dyn interaction::Prompt>,
) -> Result<inference::Candidate, CliError> {
    // Every refusal from here on carries what resolution already established:
    // the caller should never have to re-import or re-query to learn how far
    // the command got.
    let mut client = public_client(globals).map_err(|error| {
        partial.refuse(&error.code, error.message, error.exit_code, error.retryable)
    })?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(wait_device_ms);
    let interactive = prompt.is_some();
    let mut waiting_announced = false;
    loop {
        let observations = client.device_list().map_err(|error| {
            partial.refuse(&error.code, error.message, error.exit_code, error.retryable)
        })?;
        let mut candidates = observations
            .into_iter()
            .map(|observation| {
                let identification = inference::identify(registry, &observation, None);
                inference::Candidate {
                    observation,
                    identification,
                }
            })
            .collect::<Vec<_>>();
        if let Some(device) = explicit_device {
            candidates.retain(|candidate| candidate.observation.observation_id == device);
        } else {
            // Only devices this firmware could actually be written to are
            // candidates; a board of a different family is not an ambiguity.
            candidates.retain(|candidate| {
                candidate
                    .identification
                    .compatible_profiles
                    .iter()
                    .any(|profile| artifact.compatible_profiles.contains(profile))
            });
            if let Some(selector) = target {
                let selected = inference::select_by_target(&candidates, selector)
                    .into_iter()
                    .map(|candidate| candidate.observation.observation_id.clone())
                    .collect::<Vec<_>>();
                candidates
                    .retain(|candidate| selected.contains(&candidate.observation.observation_id));
            }
        }
        partial.candidates = candidates.iter().map(resolved_device_json).collect();

        match candidates.len() {
            1 => return Ok(candidates.remove(0)),
            // An operator at a terminal is told what is being waited for rather
            // than refused for not having plugged it in yet.
            0 if interactive => {
                if let Some(prompt) = reborrow(&mut prompt)
                    && !waiting_announced
                {
                    waiting_announced = true;
                    prompt.show(&format!(
                        "Waiting for a device that can take {}. Press Ctrl-C to stop.",
                        artifact.manifest.format_id
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            0 if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            0 => {
                return Err(partial.refuse(
                    "DEVICE_NOT_FOUND",
                    match (explicit_device, target) {
                        (Some(device), _) => {
                            format!("No current observation has id {device}.")
                        }
                        (None, Some(selector)) => format!(
                            "No connected device matching {selector:?} declares a profile compatible with format {}.",
                            artifact.manifest.format_id
                        ),
                        (None, None) => format!(
                            "No connected device declares a profile compatible with format {}.",
                            artifact.manifest.format_id
                        ),
                    },
                    5,
                    true,
                ));
            }
            _ if interactive => {
                let choices = candidates
                    .iter()
                    .map(|candidate| interaction::Choice {
                        value: candidate.observation.observation_id.clone(),
                        label: candidate.summary(),
                    })
                    .collect::<Vec<_>>();
                let selected = reborrow(&mut prompt)
                    .and_then(|prompt| {
                        interaction::select(
                            prompt,
                            "Several devices can take this firmware:",
                            &choices,
                        )
                    })
                    .ok_or_else(|| {
                        partial.refuse("DEVICE_AMBIGUOUS", "No device was selected.", 6, true)
                    })?;
                return Ok(candidates
                    .into_iter()
                    .find(|candidate| candidate.observation.observation_id == selected)
                    .expect("the selection came from the candidate list"));
            }
            count => {
                return Err(partial.refuse(
                    "DEVICE_AMBIGUOUS",
                    format!(
                        "{count} connected devices could take this firmware; name one with --device <observation-id> or --target <selector>. Candidates: {}.",
                        candidates
                            .iter()
                            .map(inference::Candidate::summary)
                            .collect::<Vec<_>>()
                            .join("; ")
                    ),
                    6,
                    false,
                ));
            }
        }
    }
}

/// The one composite staging document, for every outcome.
///
/// The same shape carries a sealed plan, an assessment-only answer, and the
/// refusal facts, so a caller parses one document rather than three.
fn flash_plan_v2_json(
    partial: &PartialResolution,
    assessment: Option<&Assessment>,
    plan: Option<&ExecutablePlan>,
) -> String {
    let assessment_json = match assessment {
        None => "null".to_string(),
        Some(assessment) => format!(
            "{{\"executable\":{},\"availability\":{},\"unavailable_reason\":{},\"mechanics_maturity\":{{\"key_sha256\":{},\"state\":{}}},\"authority_support\":{{\"key_sha256\":{},\"state\":{}}},\"would_be_steps\":{},\"known_persistent_effects\":{},\"data_impact\":{},\"unknowns\":{},\"evidence_requirements\":{},\"blockers\":[{}]}}",
            assessment_is_executable(assessment),
            json(&assessment.availability),
            optional_json(
                (!assessment.unavailable_reason.is_empty())
                    .then_some(assessment.unavailable_reason.as_str())
            ),
            json(&assessment.mechanics_maturity_key_sha256),
            json(&assessment.mechanics_maturity_state),
            json(&assessment.authority_support_key_sha256),
            json(&assessment.authority_support_state),
            job::steps_json(&assessment.would_be_steps),
            job::effects_json(&assessment.known_persistent_effects),
            job::key_values_json(&assessment.data_impact),
            job::key_values_json(&assessment.unknowns),
            job::key_values_json(&assessment.evidence_requirements),
            assessment_blockers(assessment).join(","),
        ),
    };
    let plan_json = match plan {
        None => "null".to_string(),
        Some(plan) => {
            let acknowledgements = plan_tokens(plan, &partial.extra_acknowledgements);
            format!(
                "{{\"plan_id\":{},\"plan_sha256\":{},\"provider_execution_plan_sha256\":{},\"public_projection_sha256\":{},\"execution_purpose\":{},\"expires_at_epoch_ms\":{},\"ordered_steps\":{},\"persistent_effects\":{},\"required_acknowledgements\":{},\"execution_context\":{{\"mechanics_maturity\":{},\"authority_support\":{},\"hardware_campaign\":{}}}}}",
                json(&plan.plan_id),
                json(&plan.plan_sha256),
                json(&plan.provider_execution_plan_sha256),
                json(&plan.public_projection_sha256),
                json(&plan.execution_purpose),
                plan.expires_at_epoch_ms,
                job::steps_json(&plan.public_steps),
                job::effects_json(&plan.persistent_effects),
                json_strings(&acknowledgements),
                json(&plan.mechanics_maturity_state),
                json(&plan.authority_support_state),
                optional_json(sealed_campaign(plan).as_deref()),
            )
        }
    };
    format!(
        "{{\"schema\":\"arkforge.flash-plan/v2\",\"resolved\":{{\"artifact\":{},\"device\":{},\"profile\":{},\"intent\":{}}},\"assessment\":{},\"plan\":{},\"apply_command\":{},\"next_commands\":{}}}",
        partial.artifact.as_deref().unwrap_or("null"),
        partial.device.as_deref().unwrap_or("null"),
        partial.profile.as_deref().unwrap_or("null"),
        partial.intent.as_deref().unwrap_or("null"),
        assessment_json,
        plan_json,
        optional_json(
            plan.map(|plan| {
                apply_command(
                    plan,
                    partial.sealed_campaign.as_deref(),
                    &partial.extra_acknowledgements,
                )
            })
            .as_deref()
        ),
        json_strings(&flash_plan_next_commands(partial, assessment, plan)),
    )
}

fn assessment_blockers(assessment: &Assessment) -> Vec<String> {
    let mut blockers = Vec::new();
    if !execution_support_state_permits(&assessment.mechanics_maturity_state) {
        blockers.push(format!(
            "{{\"code\":\"MECHANICS_MATURITY_UNAVAILABLE\",\"state\":{},\"key_sha256\":{},\"remediation\":\"Run only a named reviewed hardware campaign or wait for production mechanics support.\"}}",
            json(&assessment.mechanics_maturity_state),
            json(&assessment.mechanics_maturity_key_sha256),
        ));
    }
    if !execution_support_state_permits(&assessment.authority_support_state) {
        blockers.push(format!(
            "{{\"code\":\"AUTHORITY_SUPPORT_UNAVAILABLE\",\"state\":{},\"key_sha256\":{},\"remediation\":\"Bind exact HDC and use a named acceptance campaign, or wait for exact-key production support.\"}}",
            json(&assessment.authority_support_state),
            json(&assessment.authority_support_key_sha256),
        ));
    }
    if blockers.is_empty() && !assessment_is_executable(assessment) {
        blockers.push(format!(
            "{{\"code\":\"PLAN_PRECONDITION_UNAVAILABLE\",\"state\":{},\"key_sha256\":null,\"remediation\":\"Inspect unavailable_reason and repeat only after the named precondition changes.\"}}",
            json(&assessment.availability),
        ));
    }
    blockers
}

/// The exact command that executes this sealed plan, with nothing to look up.
fn apply_command(plan: &ExecutablePlan, campaign: Option<&str>, extra: &[String]) -> String {
    format!(
        "arkforge apply --plan {} --expect-plan-sha256 {}{}{}",
        plan.plan_id,
        plan.plan_sha256,
        campaign
            .map(|campaign| format!(" --hardware-campaign {campaign}"))
            .unwrap_or_default(),
        plan_tokens(plan, extra)
            .iter()
            .map(|token| format!(" --ack {token}"))
            .collect::<String>(),
    )
}

fn flash_plan_next_commands(
    partial: &PartialResolution,
    assessment: Option<&Assessment>,
    plan: Option<&ExecutablePlan>,
) -> Vec<String> {
    if let Some(plan) = plan {
        return vec![apply_command(
            plan,
            partial.sealed_campaign.as_deref(),
            &partial.extra_acknowledgements,
        )];
    }
    match assessment {
        Some(assessment) if assessment_is_executable(assessment) => {
            vec!["arkforge flash plan --file <firmware-file>".to_string()]
        }
        Some(_) => vec!["arkforge status".to_string()],
        None => vec!["arkforge device list --deep".to_string()],
    }
}

pub(crate) fn print_flash_plan_v2(
    output: Output,
    resolved: &Resolved,
    partial: &PartialResolution,
    assessment: Option<&Assessment>,
    plan: Option<&ExecutablePlan>,
) {
    match output {
        Output::Human => {
            let next = flash_plan_next_commands(partial, assessment, plan);
            match plan {
                Some(plan) => println!("Normal flash plan {}", plan.plan_id),
                None => println!("Flash assessment (no plan materialized)"),
            }
            println!("  artifact   {}", resolved.artifact.artifact_id);
            println!(
                "  format     {}  imported={}",
                resolved.artifact.manifest.format_id, resolved.artifact.imported
            );
            println!(
                "  device     {}  mode={}",
                resolved.device.observation.observation_id, resolved.device.observation.mode
            );
            println!(
                "  model      {}  strength={}",
                resolved
                    .device
                    .identification
                    .model
                    .as_deref()
                    .unwrap_or("unproven"),
                resolved.device.identification.strength.as_str()
            );
            println!(
                "  profile    {} ({})",
                resolved.profile.reference, resolved.profile.resolution
            );
            println!(
                "  intent     {} ({})",
                resolved.intent.value, resolved.intent.resolution
            );
            if let Some(assessment) = assessment {
                println!("  executable {}", assessment_is_executable(assessment));
                println!("  steps      {}", assessment.would_be_steps.len());
                job::print_effects_human(
                    "known persistent effects",
                    &assessment.known_persistent_effects,
                );
                job::print_key_values_human("data impact", &assessment.data_impact);
                job::print_key_values_human("unknowns", &assessment.unknowns);
                if !assessment.unavailable_reason.is_empty() {
                    println!("  reason     {}", assessment.unavailable_reason);
                }
            }
            if let Some(plan) = plan {
                println!("  plan SHA-256 {}", plan.plan_sha256);
                println!("  expires      {}", plan.expires_at_epoch_ms);
                println!("Required acknowledgements:");
                for token in plan_tokens(plan, &partial.extra_acknowledgements) {
                    println!("  {token}");
                }
            }
            println!("Next: {}", next[0]);
        }
        Output::Json => println!("{}", flash_plan_v2_json(partial, assessment, plan)),
    }
}

/// Every token this plan requires, including the ones its origin adds.
///
/// A superseding recovery plan requires naming the job it supersedes, so the
/// document and the apply command it prints must both carry that token, or the
/// command handed back would be refused as incomplete.
fn plan_tokens(plan: &ExecutablePlan, extra: &[String]) -> Vec<String> {
    let mut tokens = plan_acknowledgements(plan);
    tokens.extend_from_slice(extra);
    tokens.sort();
    tokens.dedup();
    tokens
}

fn plan_acknowledgements(plan: &ExecutablePlan) -> Vec<String> {
    if plan
        .persistent_effects
        .iter()
        .any(|effect| effect.target == "userdata")
    {
        return vec!["data-loss:userdata".into()];
    }
    plan.persistent_effects
        .iter()
        .map(|effect| format!("overwrite:partition={}", effect.target))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
fn execution_support_state_permits(state: &str) -> bool {
    matches!(state, "productionVerified" | "hardwareCampaign")
}

#[cfg(test)]
mod campaign_status_tests {
    use super::*;

    #[test]
    fn unavailable_status_is_not_a_non_campaign_runtime() {
        let root =
            std::env::temp_dir().join(format!("arkforge-absent-status-{}", std::process::id()));
        assert!(!root.exists());
        assert!(require_campaign_acknowledgement(&root, None).is_err());
        assert!(require_campaign_acknowledgement(&root, Some("campaign")).is_err());
    }
}
