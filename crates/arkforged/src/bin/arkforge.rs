//! Canonical ArkForge command frontend.
//!
//! Explicit native rescue and read-only host diagnostics land here before the
//! normal-flash authority surface. No canonical command is a compatibility
//! wrapper around an older binary.

use arkforge_core::Sha256Digest;
use arkforge_ipc::messages::{Assessment, Effect, InspectArtifactResponse, JobSummary, KeyValue};
use arkforged::dispatch::executable_digest;
use arkforged::packaging::{self, ContractMode, SignedCode};
use arkforged::public_client::{
    DeviceObservationView, DeviceProbeView, PublicClient, PublicClientError, RecoveryGuideView,
};
use arkforged::rescue::{
    NativeRescueBackend, RescueApplyResult, RescueDevice, RescueError, RescueInspection,
    RescueManager, RescuePlanSummary, RescueReadReceipt, now_epoch_ms,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const HELP_SCHEMA: &str = "arkforge.command-help/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Output {
    Human,
    Json,
}

impl Output {
    fn parse(value: &str) -> Result<Self, CliError> {
        match value {
            "human" => Ok(Self::Human),
            "json" => Ok(Self::Json),
            _ => Err(CliError::invalid(
                "--output accepts exactly 'human' or 'json'.",
            )),
        }
    }
}

#[derive(Debug)]
struct Globals {
    runtime_dir: Option<PathBuf>,
    output: Output,
}

#[derive(Debug)]
struct CliError {
    code: String,
    message: String,
    exit_code: i32,
    retryable: bool,
}

impl CliError {
    fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        exit_code: i32,
        retryable: bool,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            exit_code,
            retryable,
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new("INVALID_ARGUMENT", message, 2, false)
    }
}

impl From<RescueError> for CliError {
    fn from(error: RescueError) -> Self {
        Self {
            code: error.code.to_string(),
            message: error.message,
            exit_code: error.exit_code,
            retryable: error.retryable,
        }
    }
}

impl From<PublicClientError> for CliError {
    fn from(error: PublicClientError) -> Self {
        Self {
            code: error.code,
            message: error.message,
            exit_code: error.exit_code,
            retryable: error.retryable,
        }
    }
}

#[derive(Debug)]
struct HelpSpec {
    command: &'static str,
    summary: &'static str,
    usage: &'static str,
    effect: &'static str,
    requires: &'static [&'static str],
    produces: &'static [&'static str],
    options: &'static [(&'static str, &'static str)],
    examples: &'static [&'static str],
    next: &'static [&'static str],
    exits: &'static [(i32, &'static str)],
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let fallback_output = requested_output(&arguments).unwrap_or(Output::Human);
    match run(&arguments) {
        Ok(exit_code) => std::process::exit(exit_code),
        Err(error) => {
            print_error(fallback_output, &error);
            std::process::exit(error.exit_code);
        }
    }
}

fn run(arguments: &[String]) -> Result<i32, CliError> {
    let (globals, command) = parse_globals(arguments)?;
    if command.is_empty() {
        print_help(help_spec(&[])?, globals.output);
        return Ok(0);
    }
    if command == ["--version"] || command == ["-V"] {
        match globals.output {
            Output::Human => println!("arkforge {}", env!("CARGO_PKG_VERSION")),
            Output::Json => println!(
                "{{\"schema_version\":\"arkforge.version/v1\",\"name\":\"arkforge\",\"version\":{}}}",
                json(env!("CARGO_PKG_VERSION"))
            ),
        }
        return Ok(0);
    }
    if command[0] == "help" {
        return run_help(&command[1..], globals.output);
    }
    if let Some(help_index) = command
        .iter()
        .position(|argument| argument == "--help" || argument == "-h")
    {
        let topic = command[..help_index]
            .iter()
            .take_while(|argument| !argument.starts_with('-'))
            .cloned()
            .collect::<Vec<_>>();
        print_help(help_spec(&topic)?, globals.output);
        return Ok(0);
    }
    match command[0].as_str() {
        "device" => run_device(&command[1..], globals),
        "artifact" => run_artifact(&command[1..], globals),
        "flash" => run_flash(&command[1..], globals),
        "job" => run_job(&command[1..], globals),
        "rescue" => run_rescue(&command[1..], globals),
        "signing" => run_signing(&command[1..], globals.output),
        other => Err(CliError::invalid(format!(
            "Unknown command {other:?}. Run 'arkforge help' for the command tree."
        ))),
    }
}

fn parse_globals(arguments: &[String]) -> Result<(Globals, Vec<String>), CliError> {
    let mut runtime_dir = None;
    let mut output = Output::Human;
    let mut output_seen = false;
    let mut command = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--runtime-dir" => {
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| CliError::invalid("--runtime-dir requires a directory path."))?;
                if runtime_dir.replace(PathBuf::from(value)).is_some() {
                    return Err(CliError::invalid(
                        "--runtime-dir may be supplied only once.",
                    ));
                }
            }
            "--output" => {
                if output_seen {
                    return Err(CliError::invalid("--output may be supplied only once."));
                }
                output_seen = true;
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| CliError::invalid("--output requires human or json."))?;
                output = Output::parse(value)?;
            }
            "--no-color" => {}
            argument => command.push(argument.to_string()),
        }
        index += 1;
    }
    Ok((
        Globals {
            runtime_dir,
            output,
        },
        command,
    ))
}

fn requested_output(arguments: &[String]) -> Option<Output> {
    arguments.windows(2).find_map(|pair| {
        (pair[0] == "--output")
            .then(|| Output::parse(&pair[1]).ok())
            .flatten()
    })
}

fn run_help(arguments: &[String], global_output: Output) -> Result<i32, CliError> {
    let mut topic = Vec::new();
    let mut output = global_output;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--format" => {
                index += 1;
                output = Output::parse(
                    arguments
                        .get(index)
                        .ok_or_else(|| CliError::invalid("--format requires human or json."))?,
                )?;
            }
            argument if argument.starts_with('-') => {
                return Err(CliError::invalid(format!(
                    "Unknown help option {argument:?}."
                )));
            }
            argument => topic.push(argument.to_string()),
        }
        index += 1;
    }
    print_help(help_spec(&topic)?, output);
    Ok(0)
}

fn run_signing(arguments: &[String], output: Output) -> Result<i32, CliError> {
    let Some(subcommand) = arguments.first() else {
        print_help(help_spec(&["signing".into()])?, output);
        return Ok(0);
    };
    if subcommand != "verify" {
        return Err(CliError::invalid(format!(
            "Unknown signing command {subcommand:?}. Run 'arkforge help signing'."
        )));
    }

    let options = Options::parse(&arguments[1..])?;
    options.ensure_only(&["file", "mode"])?;
    let file = Path::new(options.one("file")?);
    let (mode, mode_name) = match options.one("mode")? {
        "development" => (ContractMode::Development, "development"),
        "release" => (ContractMode::Release, "release"),
        value => {
            return Err(CliError::invalid(format!(
                "--mode accepts 'development' or 'release', not {value:?}."
            )));
        }
    };
    let code = packaging::read_file(file).map_err(|error| {
        CliError::new(
            "SIGNING_INPUT_REFUSED",
            format!("Unable to inspect {}: {error}", file.display()),
            3,
            false,
        )
    })?;
    let violations = code.violations(mode);
    print_signing(output, file, mode_name, &code, &violations);
    Ok(if violations.is_empty() { 0 } else { 3 })
}

fn run_device(arguments: &[String], globals: Globals) -> Result<i32, CliError> {
    let Some(subcommand) = arguments.first() else {
        print_help(help_spec(&["device".into()])?, globals.output);
        return Ok(0);
    };
    let options = Options::parse(&arguments[1..])?;
    match subcommand.as_str() {
        "list" => {
            options.ensure_only(&[])?;
            let mut client = public_client(&globals)?;
            let observations = client.device_list()?;
            print_device_observations(globals.output, &observations);
            Ok(0)
        }
        "probe" => {
            options.ensure_only(&["device", "profile"])?;
            let device = options.one("device")?;
            let profile = options.one("profile")?;
            let mut client = public_client(&globals)?;
            let probe = client.device_probe(device, profile)?;
            print_device_probe(globals.output, &probe);
            Ok(0)
        }
        other => Err(CliError::invalid(format!(
            "Unknown device command {other:?}. Run 'arkforge help device'."
        ))),
    }
}

fn run_artifact(arguments: &[String], globals: Globals) -> Result<i32, CliError> {
    let Some(subcommand) = arguments.first() else {
        print_help(help_spec(&["artifact".into()])?, globals.output);
        return Ok(0);
    };
    let options = Options::parse(&arguments[1..])?;
    match subcommand.as_str() {
        "show" => {
            options.ensure_only(&["artifact"])?;
            let artifact = options.one("artifact")?;
            let mut client = public_client(&globals)?;
            let manifest = client.artifact_show(artifact)?;
            print_artifact(globals.output, artifact, &manifest);
            Ok(0)
        }
        other => Err(CliError::invalid(format!(
            "Unknown artifact command {other:?}. Run 'arkforge help artifact'."
        ))),
    }
}

fn run_flash(arguments: &[String], globals: Globals) -> Result<i32, CliError> {
    let Some(subcommand) = arguments.first() else {
        print_help(help_spec(&["flash".into()])?, globals.output);
        return Ok(0);
    };
    let options = Options::parse(&arguments[1..])?;
    match subcommand.as_str() {
        "assess" => {
            options.ensure_only(&["artifact", "profile", "device", "intent"])?;
            let intent = options.one("intent")?;
            if intent != "full-restore" {
                return Err(CliError::invalid(format!(
                    "--intent accepts exactly 'full-restore', not {intent:?}."
                )));
            }
            let artifact = options.one("artifact")?;
            let profile = options.one("profile")?;
            let device = options.one("device")?;
            let mut client = public_client(&globals)?;
            let assessment = client.flash_assess(artifact, profile, device)?;
            print_flash_assessment(
                globals.output,
                artifact,
                profile,
                device,
                intent,
                &assessment,
            );
            Ok(0)
        }
        other => Err(CliError::invalid(format!(
            "Unknown flash command {other:?}. Run 'arkforge help flash'."
        ))),
    }
}

fn run_job(arguments: &[String], globals: Globals) -> Result<i32, CliError> {
    let Some(subcommand) = arguments.first() else {
        print_help(help_spec(&["job".into()])?, globals.output);
        return Ok(0);
    };
    match subcommand.as_str() {
        "list" => {
            let options = Options::parse(&arguments[1..])?;
            options.ensure_only(&[])?;
            let mut client = public_client(&globals)?;
            let jobs = client.job_list()?;
            print_jobs(globals.output, &jobs);
            Ok(0)
        }
        "show" => {
            let options = Options::parse(&arguments[1..])?;
            options.ensure_only(&["job"])?;
            let job_id = options.one("job")?;
            let mut client = public_client(&globals)?;
            let job = client.job_show(job_id)?;
            print_job(globals.output, &job);
            Ok(0)
        }
        "recovery" => run_job_recovery(&arguments[1..], globals),
        other => Err(CliError::invalid(format!(
            "Unknown job command {other:?}. Run 'arkforge help job'."
        ))),
    }
}

fn run_job_recovery(arguments: &[String], globals: Globals) -> Result<i32, CliError> {
    let Some(subcommand) = arguments.first() else {
        print_help(
            help_spec(&["job".into(), "recovery".into()])?,
            globals.output,
        );
        return Ok(0);
    };
    match subcommand.as_str() {
        "guide" => {
            let options = Options::parse(&arguments[1..])?;
            options.ensure_only(&["job"])?;
            let job = options.one("job")?;
            let mut client = public_client(&globals)?;
            let guide = client.recovery_guide(job)?;
            print_recovery_guide(globals.output, &guide);
            Ok(0)
        }
        other => Err(CliError::invalid(format!(
            "Unknown recovery command {other:?}. Run 'arkforge help job recovery'."
        ))),
    }
}

fn public_client(globals: &Globals) -> Result<PublicClient, CliError> {
    let runtime_dir = match &globals.runtime_dir {
        Some(path) => path.clone(),
        None => default_runtime_dir()?,
    };
    PublicClient::connect(&runtime_dir).map_err(Into::into)
}

fn run_rescue(arguments: &[String], globals: Globals) -> Result<i32, CliError> {
    let Some(subcommand) = arguments.first() else {
        print_help(help_spec(&["rescue".into()])?, globals.output);
        return Ok(0);
    };
    let options = Options::parse(&arguments[1..])?;
    let runtime_dir = match globals.runtime_dir {
        Some(path) => path,
        None => default_runtime_dir()?,
    };
    let executable = std::env::current_exe().map_err(|error| {
        CliError::invalid(format!("Cannot locate this arkforge build: {error}"))
    })?;
    let build_digest = executable_digest(&executable).map_err(|message| {
        CliError::invalid(format!("Cannot hash this arkforge build: {message}"))
    })?;
    let manager = RescueManager::new(runtime_dir, build_digest, NativeRescueBackend::new())?;

    match subcommand.as_str() {
        "list" => {
            options.ensure_only(&[])?;
            print_devices(globals.output, &manager.list_devices()?);
            Ok(0)
        }
        "inspect" => {
            options.ensure_only(&["device"])?;
            let result = manager.inspect(options.one("device")?)?;
            print_inspection(globals.output, &result);
            Ok(0)
        }
        "read" => {
            options.ensure_only(&["device", "start-sector", "sector-count", "out"])?;
            let start = parse_u64("--start-sector", options.one("start-sector")?)?;
            let count = parse_u64("--sector-count", options.one("sector-count")?)?;
            let result = manager.read_sectors(
                options.one("device")?,
                start,
                count,
                Path::new(options.one("out")?),
            )?;
            print_read(globals.output, &result);
            Ok(0)
        }
        "plan" => {
            options.ensure_only(&[
                "device",
                "operation",
                "partition",
                "image",
                "expect-image-sha256",
            ])?;
            let now = now_epoch_ms()?;
            let result = match options.one("operation")? {
                "write-partition" => manager.plan_write(
                    options.one("device")?,
                    options.one("partition")?,
                    Path::new(options.one("image")?),
                    parse_digest("--expect-image-sha256", options.one("expect-image-sha256")?)?,
                    now,
                )?,
                "reset-device" => {
                    options.ensure_absent(&["partition", "image", "expect-image-sha256"])?;
                    manager.plan_reset(options.one("device")?, now)?
                }
                operation => {
                    return Err(CliError::invalid(format!(
                        "--operation accepts 'write-partition' or 'reset-device', not {operation:?}."
                    )));
                }
            };
            print_plan(globals.output, &result);
            Ok(0)
        }
        "apply" => {
            options.ensure_only(&["plan", "expect-plan-sha256", "ack"])?;
            let result = manager.apply(
                options.one("plan")?,
                parse_digest("--expect-plan-sha256", options.one("expect-plan-sha256")?)?,
                options.many_required("ack")?,
                now_epoch_ms()?,
            )?;
            let exit = result.exit_code();
            print_apply(globals.output, &result)?;
            Ok(exit)
        }
        other => Err(CliError::invalid(format!(
            "Unknown rescue command {other:?}. Run 'arkforge help rescue'."
        ))),
    }
}

fn print_signing(
    output: Output,
    file: &Path,
    mode: &str,
    code: &SignedCode,
    violations: &[packaging::ContractViolation],
) {
    let compliant = violations.is_empty();
    match output {
        Output::Human => {
            println!(
                "Signing contract: {}",
                if compliant { "compliant" } else { "refused" }
            );
            println!("file     {}", file.display());
            println!("mode     {mode}");
            println!("facts    {}", code.summary());
            if compliant {
                println!("No further signing action is required.");
            } else {
                println!("violations");
                for violation in violations {
                    println!("  {}: {violation}", violation.code());
                }
                println!(
                    "Next: Correct every listed signing fact, then run this exact verification again."
                );
            }
        }
        Output::Json => {
            let violations_json = violations
                .iter()
                .map(|violation| {
                    format!(
                        "{{\"code\":{},\"message\":{}}}",
                        json(violation.code()),
                        json(&violation.to_string())
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            let next = if compliant {
                Vec::new()
            } else {
                vec![format!(
                    "arkforge signing verify --file {} --mode {mode}",
                    file.display()
                )]
            };
            println!(
                "{{\"schema_version\":\"arkforge.signing-verification/v1\",\"file\":{},\"mode\":{},\"compliant\":{},\"facts\":{},\"violations\":[{}],\"contract\":{},\"next_commands\":{}}}",
                json(&file.display().to_string()),
                json(mode),
                compliant,
                json(&code.summary()),
                violations_json,
                json(packaging::CONTRACT_DOC),
                json_array(&next)
            );
        }
    }
}

fn print_device_observations(output: Output, observations: &[DeviceObservationView]) {
    let next = if observations.is_empty() {
        vec!["arkforge device list".to_string()]
    } else {
        vec!["arkforge device probe --device <observation-id> --profile <profile-id>".to_string()]
    };
    match output {
        Output::Human => {
            if observations.is_empty() {
                println!("No device observations are available.");
                println!(
                    "Connect a supported device and make sure the ArkForge runtime is running."
                );
                println!("Next: arkforge device list");
                return;
            }
            println!("Device observations ({})", observations.len());
            for observation in observations {
                println!(
                    "{}  mode={}  identity={}  observed_at_epoch_ms={}",
                    observation.observation_id,
                    observation.mode,
                    observation.identity_strength,
                    observation.observed_at_epoch_ms
                );
                if observation.malformed_descriptor {
                    println!("  descriptor: malformed");
                }
            }
            println!("Next: {}", next[0]);
        }
        Output::Json => {
            let values = observations
                .iter()
                .map(observation_json)
                .collect::<Vec<_>>()
                .join(",");
            println!(
                "{{\"schema_version\":\"arkforge.device-list/v1\",\"observations\":[{values}],\"next_commands\":{}}}",
                json_array(&next)
            );
        }
    }
}

fn print_device_probe(output: Output, probe: &DeviceProbeView) {
    let next = format!(
        "arkforge flash assess --artifact <artifact-id> --profile {} --device {} --intent full-restore",
        probe.profile_id, probe.observation.observation_id
    );
    match output {
        Output::Human => {
            println!("device             {}", probe.observation.observation_id);
            println!("profile            {}", probe.profile_id);
            println!("mode               {}", probe.observation.mode);
            println!("identity_strength  {}", probe.observation.identity_strength);
            println!("facts_sha256       {}", probe.facts_sha256);
            print_key_values_human("protocol facts", &probe.protocol_facts);
            println!("Next: {next}");
        }
        Output::Json => println!(
            "{{\"schema_version\":\"arkforge.device-probe/v1\",\"observation\":{},\"profile_id\":{},\"facts_sha256\":{},\"protocol_facts\":{},\"next_commands\":[{}]}}",
            observation_json(&probe.observation),
            json(&probe.profile_id),
            json(&probe.facts_sha256),
            key_values_json(&probe.protocol_facts),
            json(&next)
        ),
    }
}

fn print_artifact(output: Output, artifact_id: &str, manifest: &InspectArtifactResponse) {
    let next = format!(
        "arkforge flash assess --artifact {artifact_id} --profile <profile-id> --device <observation-id> --intent full-restore"
    );
    match output {
        Output::Human => {
            println!("artifact_id      {artifact_id}");
            println!("format           {}", manifest.format_id);
            println!("content_sha256   {}", manifest.content_sha256);
            println!("manifest_sha256  {}", manifest.manifest_sha256);
            println!("size_bytes       {}", manifest.size_bytes);
            println!("confidence       {}", manifest.confidence);
            println!("members ({})", manifest.members.len());
            for member in &manifest.members {
                println!(
                    "  {}  size={}  role={}  sha256={}",
                    member.path, member.size_bytes, member.role, member.sha256
                );
            }
            println!("partitions ({})", manifest.partitions.len());
            for partition in &manifest.partitions {
                let sectors = partition
                    .size_sectors
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "remainder".into());
                println!(
                    "  {}  index={}  start={}  sectors={}  attribute={}  grammar={}",
                    partition.name,
                    partition.index,
                    partition.offset_sectors,
                    sectors,
                    partition.attribute,
                    partition.grammar_branch
                );
            }
            print_key_values_human("build facts", &manifest.build_facts);
            if !manifest.unclassified_members.is_empty() {
                println!("unclassified members");
                for member in &manifest.unclassified_members {
                    println!("  {member}");
                }
            }
            print_key_values_human(
                "execution-relevant unknowns",
                &manifest.execution_relevant_unknowns,
            );
            println!("Next: {next}");
        }
        Output::Json => {
            let members = manifest
                .members
                .iter()
                .map(|member| {
                    format!(
                        "{{\"path\":{},\"size_bytes\":{},\"sha256\":{},\"role\":{}}}",
                        json(&member.path),
                        member.size_bytes,
                        json(&member.sha256),
                        json(&member.role)
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            let partitions = manifest
                .partitions
                .iter()
                .map(|partition| {
                    format!(
                        "{{\"index\":{},\"name\":{},\"offset_sectors\":{},\"size_sectors\":{},\"attribute\":{},\"grammar_branch\":{}}}",
                        partition.index,
                        json(&partition.name),
                        partition.offset_sectors,
                        optional_u64(partition.size_sectors),
                        json(&partition.attribute),
                        json(&partition.grammar_branch)
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            println!(
                "{{\"schema_version\":\"arkforge.artifact/v1\",\"artifact_id\":{},\"format_id\":{},\"content_sha256\":{},\"manifest_sha256\":{},\"size_bytes\":{},\"confidence\":{},\"members\":[{}],\"partitions\":[{}],\"build_facts\":{},\"unclassified_members\":{},\"execution_relevant_unknowns\":{},\"next_commands\":[{}]}}",
                json(artifact_id),
                json(&manifest.format_id),
                json(&manifest.content_sha256),
                json(&manifest.manifest_sha256),
                manifest.size_bytes,
                json(&manifest.confidence),
                members,
                partitions,
                key_values_json(&manifest.build_facts),
                json_array(&manifest.unclassified_members),
                key_values_json(&manifest.execution_relevant_unknowns),
                json(&next)
            );
        }
    }
}

fn print_flash_assessment(
    output: Output,
    artifact: &str,
    profile: &str,
    device: &str,
    intent: &str,
    assessment: &Assessment,
) {
    let next = vec![format!(
        "arkforge flash assess --artifact {artifact} --profile {profile} --device {device} --intent {intent}"
    )];
    match output {
        Output::Human => {
            println!("Flash assessment (not executable)");
            println!("artifact      {artifact}");
            println!("profile       {profile}");
            println!("device        {device}");
            println!("intent        {intent}");
            println!("availability  {}", assessment.availability);
            if !assessment.unavailable_reason.is_empty() {
                println!("reason        {}", assessment.unavailable_reason);
            }
            println!("would-be steps ({})", assessment.would_be_steps.len());
            for step in &assessment.would_be_steps {
                println!(
                    "  {}  kind={}  effect={}  target={}  cancellation={}",
                    step.step_id, step.kind, step.effect, step.semantic_target, step.cancellation
                );
            }
            print_effects_human(
                "known persistent effects",
                &assessment.known_persistent_effects,
            );
            print_key_values_human("data impact", &assessment.data_impact);
            print_key_values_human("unknowns", &assessment.unknowns);
            print_key_values_human("evidence requirements", &assessment.evidence_requirements);
            println!("Next: {}", next[0]);
        }
        Output::Json => println!(
            "{{\"schema_version\":\"arkforge.flash-assessment/v1\",\"executable\":false,\"artifact_id\":{},\"profile_id\":{},\"device_id\":{},\"intent\":{},\"availability\":{},\"unavailable_reason\":{},\"would_be_steps\":{},\"known_persistent_effects\":{},\"data_impact\":{},\"unknowns\":{},\"evidence_requirements\":{},\"next_commands\":{}}}",
            json(artifact),
            json(profile),
            json(device),
            json(intent),
            json(&assessment.availability),
            optional_json(
                (!assessment.unavailable_reason.is_empty())
                    .then_some(assessment.unavailable_reason.as_str())
            ),
            steps_json(&assessment.would_be_steps),
            effects_json(&assessment.known_persistent_effects),
            key_values_json(&assessment.data_impact),
            key_values_json(&assessment.unknowns),
            key_values_json(&assessment.evidence_requirements),
            json_array(&next)
        ),
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
            println!(
                "{{\"schema_version\":\"arkforge.job-list/v1\",\"jobs\":[{values}],\"next_commands\":{}}}",
                json_array(&next)
            );
        }
    }
}

fn print_job(output: Output, job: &JobSummary) {
    let next = if job.state == "outcomeUnknown" {
        vec![format!("arkforge job recovery guide --job {}", job.job_id)]
    } else if job.terminal {
        Vec::new()
    } else {
        vec![format!("arkforge job show --job {}", job.job_id)]
    };
    match output {
        Output::Human => {
            print_job_human(job);
            if let Some(command) = next.first() {
                println!("Next: {command}");
            }
        }
        Output::Json => println!(
            "{{\"schema_version\":\"arkforge.job/v1\",\"job\":{},\"next_commands\":{}}}",
            job_json(job),
            json_array(&next)
        ),
    }
}

fn print_recovery_guide(output: Output, guide: &RecoveryGuideView) {
    match output {
        Output::Human => {
            println!("job                           {}", guide.job_id);
            println!("original_state                {}", guide.original_state);
            println!(
                "original_outcome_immutable     {}",
                guide.original_outcome_immutable
            );
            println!(
                "automatic_replay_forbidden     {}",
                guide.automatic_replay_forbidden
            );
            println!(
                "complete_overwrite_supported   {}",
                guide.complete_overwrite_supported
            );
            if !guide.contract_id.is_empty() {
                println!("recovery_contract             {}", guide.contract_id);
                println!("recovery_contract_version     {}", guide.contract_version);
                println!("recovery_contract_sha256      {}", guide.contract_sha256);
            }
            println!("actions");
            for action in &guide.actions {
                println!("  {action}");
            }
            println!("Next: Follow the actions in order. Never replay the original intent.");
        }
        Output::Json => println!(
            "{{\"schema_version\":\"arkforge.recovery-guide/v1\",\"job_id\":{},\"original_state\":{},\"original_outcome_immutable\":{},\"automatic_replay_forbidden\":{},\"complete_overwrite_supported\":{},\"contract\":{},\"actions\":{},\"next_commands\":[]}}",
            json(&guide.job_id),
            json(&guide.original_state),
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
    }
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

fn print_key_values_human(title: &str, values: &[KeyValue]) {
    if values.is_empty() {
        return;
    }
    println!("{title}");
    for value in values {
        println!("  {} = {}", value.key, value.value);
    }
}

fn print_effects_human(title: &str, effects: &[Effect]) {
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

fn observation_json(observation: &DeviceObservationView) -> String {
    format!(
        "{{\"observation_id\":{},\"observed_at_epoch_ms\":{},\"mode\":{},\"topology_sha256\":{},\"descriptor_sha256\":{},\"identity_strength\":{},\"malformed_descriptor\":{},\"protocol_identity\":{}}}",
        json(&observation.observation_id),
        observation.observed_at_epoch_ms,
        json(&observation.mode),
        json(&observation.topology_sha256),
        json(&observation.descriptor_sha256),
        json(&observation.identity_strength),
        observation.malformed_descriptor,
        key_values_json(&observation.protocol_identity)
    )
}

fn key_values_json(values: &[KeyValue]) -> String {
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

fn steps_json(steps: &[arkforge_ipc::messages::PublicStep]) -> String {
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

fn effects_json(effects: &[Effect]) -> String {
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

fn hex_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[derive(Debug)]
struct Options {
    values: BTreeMap<String, Vec<String>>,
}

impl Options {
    fn parse(arguments: &[String]) -> Result<Self, CliError> {
        let mut values: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut index = 0;
        while index < arguments.len() {
            let option = arguments[index].strip_prefix("--").ok_or_else(|| {
                CliError::invalid(format!(
                    "Unexpected positional argument {:?}; command inputs are named options.",
                    arguments[index]
                ))
            })?;
            if option.is_empty() {
                return Err(CliError::invalid("'--' is not a command option."));
            }
            index += 1;
            let value = arguments
                .get(index)
                .ok_or_else(|| CliError::invalid(format!("--{option} requires a value.")))?;
            if value.starts_with("--") {
                return Err(CliError::invalid(format!("--{option} requires a value.")));
            }
            values
                .entry(option.to_string())
                .or_default()
                .push(value.clone());
            index += 1;
        }
        Ok(Self { values })
    }

    fn ensure_only(&self, allowed: &[&str]) -> Result<(), CliError> {
        let allowed: BTreeSet<&str> = allowed.iter().copied().collect();
        if let Some(name) = self
            .values
            .keys()
            .find(|name| !allowed.contains(name.as_str()))
        {
            return Err(CliError::invalid(format!("Unknown option --{name}.")));
        }
        for (name, values) in &self.values {
            if name != "ack" && values.len() != 1 {
                return Err(CliError::invalid(format!(
                    "--{name} may be supplied only once."
                )));
            }
        }
        Ok(())
    }

    fn ensure_absent(&self, names: &[&str]) -> Result<(), CliError> {
        if let Some(name) = names.iter().find(|name| self.values.contains_key(**name)) {
            return Err(CliError::invalid(format!(
                "--{name} is not valid for --operation reset-device."
            )));
        }
        Ok(())
    }

    fn one(&self, name: &str) -> Result<&str, CliError> {
        match self.values.get(name).map(Vec::as_slice) {
            Some([value]) => Ok(value),
            Some(_) => Err(CliError::invalid(format!(
                "--{name} may be supplied only once."
            ))),
            None => Err(CliError::invalid(format!("Missing required --{name}."))),
        }
    }

    fn many_required(&self, name: &str) -> Result<&[String], CliError> {
        self.values
            .get(name)
            .filter(|values| !values.is_empty())
            .map(Vec::as_slice)
            .ok_or_else(|| CliError::invalid(format!("Supply at least one --{name}.")))
    }
}

fn parse_u64(option: &str, value: &str) -> Result<u64, CliError> {
    value
        .parse()
        .map_err(|_| CliError::invalid(format!("{option} requires an unsigned decimal integer.")))
}

fn parse_digest(option: &str, value: &str) -> Result<Sha256Digest, CliError> {
    Sha256Digest::parse_hex(value.strip_prefix("sha256:").unwrap_or(value)).map_err(|error| {
        CliError::invalid(format!("{option} requires one SHA-256 digest: {error}"))
    })
}

fn default_runtime_dir() -> Result<PathBuf, CliError> {
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        return Ok(PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("ArkForge"));
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Some(state) = std::env::var_os("XDG_STATE_HOME") {
            return Ok(PathBuf::from(state).join("arkforge"));
        }
        if let Some(home) = std::env::var_os("HOME") {
            return Ok(PathBuf::from(home).join(".local/state/arkforge"));
        }
    }
    Err(CliError::invalid(
        "Cannot determine the per-user state directory. Supply --runtime-dir <dir>.",
    ))
}

fn print_devices(output: Output, devices: &[RescueDevice]) {
    match output {
        Output::Human => {
            if devices.is_empty() {
                println!("No DAYU200 Loader devices found.");
                println!("Next: Put one device in Loader mode, then run 'arkforge rescue list'.");
            } else {
                println!("Native Loader devices ({})", devices.len());
                for device in devices {
                    println!(
                        "{}  {:04x}:{:04x}  location=0x{:08x}  mode={}",
                        device.device_id,
                        device.vendor_id,
                        device.product_id,
                        device.location_id,
                        device.mode
                    );
                }
                println!("Next: arkforge rescue inspect --device <device-id>");
            }
        }
        Output::Json => {
            let values = devices
                .iter()
                .map(device_json)
                .collect::<Vec<_>>()
                .join(",");
            println!(
                "{{\"schema_version\":\"arkforge.rescue-device-list/v1\",\"devices\":[{values}],\"next_commands\":[\"arkforge rescue inspect --device <device-id>\"]}}"
            );
        }
    }
}

fn print_inspection(output: Output, result: &RescueInspection) {
    match output {
        Output::Human => {
            println!("device              {}", result.device.device_id);
            println!("capacity_sectors    {}", result.capacity_sectors);
            println!("layout_sha256       {}", result.layout_digest);
            println!("profile_compatible  {}", result.profile_compatible);
            if let Some(blocker) = &result.profile_blocker {
                println!("blocker             {blocker}");
            }
            println!("partitions");
            for entry in &result.table.entries {
                let size = entry
                    .size_sectors
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "remainder".into());
                println!(
                    "  {:<16} start={} sectors={size}",
                    entry.name, entry.offset_sectors
                );
            }
            println!("Next: Create a write or reset plan with 'arkforge rescue plan --help'.");
        }
        Output::Json => {
            let partitions = result
                .table
                .entries
                .iter()
                .map(|entry| {
                    format!(
                        "{{\"name\":{},\"start_sector\":{},\"sector_count\":{}}}",
                        json(&entry.name),
                        entry.offset_sectors,
                        entry
                            .size_sectors
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "null".into())
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            println!(
                "{{\"schema_version\":\"arkforge.rescue-inspection/v1\",\"device\":{},\"capacity_sectors\":{},\"capacity_evidence_sha256\":{},\"layout_sha256\":{},\"layout_evidence_sha256\":{},\"profile_compatible\":{},\"profile_blocker\":{},\"partitions\":[{}],\"next_commands\":[\"arkforge rescue plan --device <device-id> --operation <write-partition|reset-device> ...\"]}}",
                device_json(&result.device),
                result.capacity_sectors,
                json(&result.capacity_evidence_digest.to_string()),
                json(&result.layout_digest.to_string()),
                json(&result.layout_evidence_digest.to_string()),
                result.profile_compatible,
                optional_json(result.profile_blocker.as_deref()),
                partitions
            );
        }
    }
}

fn print_read(output: Output, result: &RescueReadReceipt) {
    match output {
        Output::Human => {
            println!(
                "Read {} bytes to {}.",
                result.bytes,
                result.output.display()
            );
            println!("sha256  {}", result.sha256);
            println!("The device was not mutated.");
        }
        Output::Json => println!(
            "{{\"schema_version\":\"arkforge.rescue-read-receipt/v1\",\"device_id\":{},\"start_sector\":{},\"sector_count\":{},\"bytes\":{},\"sha256\":{},\"output\":{},\"device_mutated\":false}}",
            json(&result.device.device_id),
            result.begin_sector,
            result.sector_count,
            result.bytes,
            json(&result.sha256.to_string()),
            json(&result.output.display().to_string())
        ),
    }
}

fn print_plan(output: Output, result: &RescuePlanSummary) {
    let acknowledgements = result.plan.required_acknowledgements();
    match output {
        Output::Human => {
            println!("Rescue plan created. The device has not been mutated.");
            println!("plan_id             {}", result.plan_id);
            println!("plan_sha256         {}", result.plan_sha256);
            println!("operation           {}", result.plan.operation.as_str());
            println!("expires_at_epoch_ms {}", result.plan.expires_at_epoch_ms);
            println!("required acknowledgements");
            for acknowledgement in &acknowledgements {
                println!("  {acknowledgement}");
            }
            print!(
                "Next: arkforge rescue apply --plan {} --expect-plan-sha256 {}",
                result.plan_id, result.plan_sha256
            );
            for acknowledgement in &acknowledgements {
                print!(" --ack {acknowledgement}");
            }
            println!();
        }
        Output::Json => {
            let acknowledgements_json = json_array(&acknowledgements);
            let next = format!(
                "arkforge rescue apply --plan {} --expect-plan-sha256 {}{}",
                result.plan_id,
                result.plan_sha256,
                acknowledgements
                    .iter()
                    .map(|value| format!(" --ack {value}"))
                    .collect::<String>()
            );
            println!(
                "{{\"schema_version\":\"arkforge.rescue-plan-summary/v1\",\"plan_id\":{},\"plan_sha256\":{},\"device_id\":{},\"operation\":{},\"expires_at_epoch_ms\":{},\"required_acknowledgements\":{},\"device_mutated\":false,\"next_commands\":[{}]}}",
                json(&result.plan_id),
                json(&result.plan_sha256.to_string()),
                json(&result.plan.device_id),
                json(result.plan.operation.as_str()),
                result.plan.expires_at_epoch_ms,
                acknowledgements_json,
                json(&next)
            );
        }
    }
}

fn print_apply(output: Output, result: &RescueApplyResult) -> Result<(), CliError> {
    let receipt = &result.receipt;
    let receipt_digest = receipt.digest()?;
    let receipt_id = format!("rescue-receipt:{receipt_digest}");
    match output {
        Output::Human => {
            println!("Rescue result: {}", receipt.disposition.as_str());
            println!("receipt_id      {receipt_id}");
            println!("receipt_sha256  {receipt_digest}");
            println!("plan_id         {}", receipt.plan_id);
            println!("operation       {}", receipt.operation);
            println!("evidence_sha256 {}", receipt.evidence_digest);
            println!("detail          {}", receipt.detail);
            if receipt.disposition.as_str() == "outcome-unknown" {
                println!(
                    "Next: Do not replay this plan. Inspect and reconcile the device manually."
                );
            }
        }
        Output::Json => println!(
            "{{\"schema_version\":\"arkforge.rescue-receipt/v1\",\"receipt_id\":{},\"receipt_sha256\":{},\"plan_id\":{},\"plan_sha256\":{},\"device_id\":{},\"operation\":{},\"disposition\":{},\"evidence_sha256\":{},\"completed_at_epoch_ms\":{},\"detail\":{},\"payload_bytes\":{},\"payload_sha256\":{},\"replay_allowed\":false,\"next_commands\":{}}}",
            json(&receipt_id),
            json(&receipt_digest.to_string()),
            json(&receipt.plan_id),
            json(&receipt.plan_digest.to_string()),
            json(&receipt.device_id),
            json(&receipt.operation),
            json(receipt.disposition.as_str()),
            json(&receipt.evidence_digest.to_string()),
            receipt.completed_at_epoch_ms,
            json(&receipt.detail),
            optional_u64(receipt.payload_bytes),
            receipt
                .payload_digest
                .map(|value| json(&value.to_string()))
                .unwrap_or_else(|| "null".into()),
            if receipt.disposition.as_str() == "outcome-unknown" {
                "[\"arkforge rescue inspect --device <device-id>\"]"
            } else {
                "[]"
            }
        ),
    }
    Ok(())
}

fn device_json(device: &RescueDevice) -> String {
    format!(
        "{{\"device_id\":{},\"facts_sha256\":{},\"usb_vendor_id\":{},\"usb_product_id\":{},\"usb_location_id\":{},\"mode\":{},\"serial_present\":{}}}",
        json(&device.device_id),
        json(&device.facts_digest.to_string()),
        device.vendor_id,
        device.product_id,
        device.location_id,
        json(&device.mode),
        device.serial_present
    )
}

fn print_error(output: Output, error: &CliError) {
    let remediation = remediation(&error.code);
    match output {
        Output::Human => {
            eprintln!("arkforge: {}: {}", error.code, error.message);
            if let Some(next) = remediation {
                eprintln!("Next: {next}");
            }
        }
        Output::Json => eprintln!(
            "{{\"schema_version\":\"arkforge.error/v1\",\"code\":{},\"message\":{},\"remediation\":{},\"retryable\":{},\"next_commands\":{}}}",
            json(&error.code),
            json(&error.message),
            optional_json(remediation),
            error.retryable,
            remediation
                .map(|value| format!("[{}]", json(value)))
                .unwrap_or_else(|| "[]".into())
        ),
    }
}

fn remediation(code: &str) -> Option<&'static str> {
    match code {
        "INVALID_ARGUMENT" => Some("arkforge help --format json"),
        "SIGNING_INPUT_REFUSED" => Some("arkforge help signing verify --format json"),
        "DAEMON_UNAVAILABLE" | "IPC_IO_FAILED" => Some("arkforged --runtime-dir <dir>"),
        "PROTOCOL_REFUSED" | "IPC_RESPONSE_INVALID" | "IPC_RESPONSE_MISMATCH" => {
            Some("arkforge --version")
        }
        "ARTIFACT_NOT_FOUND" | "ARTIFACT_NOT_INSPECTED" => {
            Some("arkforge help artifact --format json")
        }
        "OBSERVATION_NOT_FOUND" => Some("arkforge device list"),
        "PROFILE_NOT_FOUND" | "NO_PROVIDER_FOR_PROFILE" => {
            Some("arkforge help device probe --format json")
        }
        "UNKNOWN_JOB" => Some("arkforge job list"),
        "DEVICE_NOT_FOUND" | "NATIVE_USB_REFUSED" => Some("arkforge rescue list"),
        "PLAN_EXPIRED" | "DEVICE_CHANGED" | "NATIVE_BUILD_CHANGED" | "PROFILE_CHANGED" => {
            Some("arkforge rescue inspect --device <device-id>")
        }
        "RESCUE_PLAN_ALREADY_APPLIED" => Some("arkforge rescue inspect --device <device-id>"),
        _ => None,
    }
}

fn help_spec(topic: &[String]) -> Result<&'static HelpSpec, CliError> {
    let key = topic
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    HELP.iter().find(|spec| spec.command == key).ok_or_else(|| {
        CliError::invalid(format!(
            "No help topic matches {:?}. Run 'arkforge help --format json'.",
            key
        ))
    })
}

fn print_help(spec: &HelpSpec, output: Output) {
    let children = child_specs(spec.command);
    match output {
        Output::Human => {
            println!("{}", spec.summary);
            println!();
            println!("Usage:\n  {}", spec.usage);
            println!();
            println!("Effect:\n  {}", spec.effect);
            if !children.is_empty() {
                println!();
                println!("Commands:");
                for child in &children {
                    let name = child
                        .command
                        .rsplit_once(' ')
                        .map_or(child.command, |(_, name)| name);
                    println!("  {name:<16} {}", child.summary);
                }
            }
            section("Requires", spec.requires);
            section("Produces", spec.produces);
            if !spec.options.is_empty() {
                println!();
                println!("Options:");
                for (option, description) in spec.options {
                    println!("  {option:<39} {description}");
                }
            }
            section("Examples", spec.examples);
            section("Next", spec.next);
            println!();
            println!("Exit codes:");
            for (code, meaning) in spec.exits {
                println!("  {code:<3} {meaning}");
            }
        }
        Output::Json => {
            let options = spec
                .options
                .iter()
                .map(|(name, description)| {
                    format!(
                        "{{\"name\":{},\"description\":{}}}",
                        json(name),
                        json(description)
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            let exits = spec
                .exits
                .iter()
                .map(|(code, meaning)| format!("{{\"code\":{code},\"meaning\":{}}}", json(meaning)))
                .collect::<Vec<_>>()
                .join(",");
            let subcommands = children
                .iter()
                .map(|child| {
                    format!(
                        "{{\"command\":{},\"summary\":{}}}",
                        json(child.command),
                        json(child.summary)
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            println!(
                "{{\"schema_version\":{},\"command\":{},\"summary\":{},\"usage\":{},\"effect\":{},\"subcommands\":[{}],\"requires\":{},\"produces\":{},\"options\":[{}],\"examples\":{},\"next_commands\":{},\"exits\":[{}]}}",
                json(HELP_SCHEMA),
                json(spec.command),
                json(spec.summary),
                json(spec.usage),
                json(spec.effect),
                subcommands,
                json_array(spec.requires),
                json_array(spec.produces),
                options,
                json_array(spec.examples),
                json_array(spec.next),
                exits
            );
        }
    }
}

fn child_specs(parent: &str) -> Vec<&'static HelpSpec> {
    HELP.iter()
        .filter(|candidate| {
            if candidate.command.is_empty() {
                return false;
            }
            if parent.is_empty() {
                return !candidate.command.contains(' ');
            }
            candidate
                .command
                .strip_prefix(parent)
                .and_then(|rest| rest.strip_prefix(' '))
                .is_some_and(|rest| !rest.is_empty() && !rest.contains(' '))
        })
        .collect()
}

fn section(title: &str, values: &[&str]) {
    if values.is_empty() {
        return;
    }
    println!();
    println!("{title}:");
    for value in values {
        println!("  {value}");
    }
}

fn json(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character <= '\u{1f}' => {
                escaped.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}

fn json_array<T: AsRef<str>>(values: &[T]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| json(value.as_ref()))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn optional_json(value: Option<&str>) -> String {
    value.map(json).unwrap_or_else(|| "null".into())
}

fn optional_u64(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".into())
}

static HELP: &[HelpSpec] = &[
    HelpSpec {
        command: "",
        summary: "ArkForge performs explicit, evidence-bound device firmware operations.",
        usage: "arkforge [--runtime-dir <dir>] [--output <human|json>] <command> [options]",
        effect: "The root command only describes capabilities. It does not access or mutate a device.",
        requires: &[],
        produces: &["Human help or arkforge.command-help/v1 JSON."],
        options: &[
            ("--runtime-dir <dir>", "Per-user ArkForge state directory."),
            (
                "--output <human|json>",
                "Stable presentation format; default: human.",
            ),
            (
                "--no-color",
                "Disable color; accepted for deterministic scripts.",
            ),
            ("-V, --version", "Print this build's ArkForge version."),
        ],
        examples: &[
            "arkforge help --format json",
            "arkforge help flash assess --format json",
        ],
        next: &["arkforge device list"],
        exits: &[
            (0, "Help or version produced."),
            (2, "Command or option is invalid."),
        ],
    },
    HelpSpec {
        command: "device",
        summary: "Discover and probe exact device observations through the public runtime.",
        usage: "arkforge device <list|probe> [options]",
        effect: "Read-only. Device commands cannot select a target, materialize authority, or mutate a device.",
        requires: &["A running ArkForge runtime for the selected --runtime-dir."],
        produces: &["Current observations or provider-specific probe evidence."],
        options: &[],
        examples: &[
            "arkforge device list",
            "arkforge help device probe --format json",
        ],
        next: &["arkforge device list"],
        exits: &[
            (0, "Query completed, including an empty list."),
            (2, "Command or option is invalid."),
            (3, "The public runtime refused the query."),
            (5, "The runtime or requested observation was not found."),
            (10, "The runtime response or local IPC failed."),
        ],
    },
    HelpSpec {
        command: "device list",
        summary: "List every current device observation without choosing a default.",
        usage: "arkforge device list",
        effect: "Read-only discovery through runtime-dir/public.sock. The device is not mutated and no observation is selected.",
        requires: &["A running ArkForge runtime."],
        produces: &[
            "arkforge.device-list/v1 observations with identity evidence and current mode.",
        ],
        options: &[],
        examples: &["arkforge --output json device list"],
        next: &["arkforge device probe --device <observation-id> --profile <profile-id>"],
        exits: &[
            (0, "Observation list produced, including an empty list."),
            (3, "Discovery was refused."),
            (5, "The runtime is not available."),
            (10, "Discovery or IPC failed."),
        ],
    },
    HelpSpec {
        command: "device probe",
        summary: "Probe one exact observation against one explicit device profile.",
        usage: "arkforge device probe --device <observation-id> --profile <profile-id>",
        effect: "Read-only provider probe. It neither selects the device for later commands nor mutates it.",
        requires: &[
            "An exact observation_id from the current device list.",
            "An explicit loaded profile id.",
        ],
        produces: &["arkforge.device-probe/v1 with provider facts and a facts digest."],
        options: &[
            (
                "--device <observation-id>",
                "Exact current observation; required.",
            ),
            (
                "--profile <profile-id>",
                "Explicit loaded profile; required.",
            ),
        ],
        examples: &[
            "arkforge --output json device probe --device OBS-PREFLIGHT --profile org.openharmony.dayu200",
        ],
        next: &[
            "arkforge flash assess --artifact <artifact-id> --profile <profile-id> --device <observation-id> --intent full-restore",
        ],
        exits: &[
            (0, "Probe evidence produced."),
            (2, "Required inputs are missing or invalid."),
            (3, "The provider refused the probe."),
            (
                5,
                "The runtime, observation, profile, or provider was not found.",
            ),
            (10, "Probe or IPC failed."),
        ],
    },
    HelpSpec {
        command: "artifact",
        summary: "Read inspected firmware artifact facts from the public runtime.",
        usage: "arkforge artifact show --artifact <artifact-id>",
        effect: "Read-only. This implemented slice does not import or modify artifact bytes.",
        requires: &["A content-addressed artifact already present in the selected runtime."],
        produces: &["A complete inspected artifact manifest."],
        options: &[],
        examples: &["arkforge help artifact show --format json"],
        next: &["arkforge artifact show --artifact <artifact-id>"],
        exits: &[
            (0, "Artifact query completed."),
            (2, "Command or option is invalid."),
            (3, "Artifact inspection was refused."),
            (5, "The runtime or artifact was not found."),
            (10, "Inspection or IPC failed."),
        ],
    },
    HelpSpec {
        command: "artifact show",
        summary: "Show the complete manifest for one content-addressed artifact.",
        usage: "arkforge artifact show --artifact <artifact-id>",
        effect: "Read-only public-socket inspection. It does not import bytes, alter the store, or mutate a device.",
        requires: &["One exact artifact SHA-256 id already present in the runtime store."],
        produces: &[
            "arkforge.artifact/v1 with members, partitions, facts, unknowns, confidence, and manifest digest.",
        ],
        options: &[(
            "--artifact <artifact-id>",
            "Exact content SHA-256 id; required.",
        )],
        examples: &["arkforge --output json artifact show --artifact <64-lowercase-hex>"],
        next: &[
            "arkforge flash assess --artifact <artifact-id> --profile <profile-id> --device <observation-id> --intent full-restore",
        ],
        exits: &[
            (0, "Artifact manifest produced."),
            (2, "The artifact id is missing or malformed."),
            (3, "Artifact bytes were rejected by inspection."),
            (5, "The runtime or artifact was not found."),
            (10, "Inspection or IPC failed."),
        ],
    },
    HelpSpec {
        command: "flash",
        summary: "Assess normal firmware work without granting execution authority.",
        usage: "arkforge flash assess --artifact <id> --profile <id> --device <id> --intent full-restore",
        effect: "The implemented public command is assessment-only and can never return or store an executable plan.",
        requires: &["Explicit artifact, profile, device observation, and semantic intent."],
        produces: &["Projected steps, effects, data impact, unknowns, and evidence requirements."],
        options: &[],
        examples: &["arkforge help flash assess --format json"],
        next: &[
            "arkforge flash assess --artifact <artifact-id> --profile <profile-id> --device <observation-id> --intent full-restore",
        ],
        exits: &[
            (0, "Assessment produced, including unavailable assessments."),
            (2, "Command or option is invalid."),
            (3, "Materialization was refused."),
            (5, "A required runtime object was not found."),
            (10, "Assessment or IPC failed."),
        ],
    },
    HelpSpec {
        command: "flash assess",
        summary: "Project the full-restore steps and effects for one exact semantic target.",
        usage: "arkforge flash assess --artifact <artifact-id> --profile <profile-id> --device <observation-id> --intent full-restore",
        effect: "Read-only assessment through the public socket. The result is structurally non-executable; no plan is stored and no device is mutated.",
        requires: &[
            "An inspected artifact id.",
            "A loaded profile id.",
            "An exact current device observation id.",
            "The explicit intent full-restore.",
        ],
        produces: &[
            "arkforge.flash-assessment/v1 with executable=false, projected steps, effects, data impact, blockers, and evidence requirements.",
        ],
        options: &[
            (
                "--artifact <artifact-id>",
                "Exact inspected artifact; required.",
            ),
            (
                "--profile <profile-id>",
                "Explicit loaded profile; required.",
            ),
            (
                "--device <observation-id>",
                "Exact current observation; required.",
            ),
            (
                "--intent full-restore",
                "Only supported semantic intent; required.",
            ),
        ],
        examples: &[
            "arkforge --output json flash assess --artifact <artifact-id> --profile org.openharmony.dayu200 --device OBS-PREFLIGHT --intent full-restore",
        ],
        next: &[
            "Resolve every unknowns[] and evidence_requirements[] item, then repeat the exact assessment.",
        ],
        exits: &[
            (
                0,
                "Assessment produced; availability may still be unavailable.",
            ),
            (2, "Required inputs are missing or invalid."),
            (3, "Materialization was refused."),
            (
                5,
                "The artifact, profile, observation, provider, or runtime was not found.",
            ),
            (
                10,
                "The response violated the public assessment contract or IPC failed.",
            ),
        ],
    },
    HelpSpec {
        command: "job",
        summary: "Read durable job state and no-replay recovery guidance.",
        usage: "arkforge job <list|show|recovery> [options]",
        effect: "Read-only public-socket queries. These commands cannot cancel, reconcile, replay, or authorize a job.",
        requires: &["A running ArkForge runtime."],
        produces: &["Durable point-in-time job status or typed recovery guidance."],
        options: &[],
        examples: &[
            "arkforge job list",
            "arkforge help job recovery --format json",
        ],
        next: &["arkforge job list"],
        exits: &[
            (0, "Job query completed, including an empty list."),
            (2, "Command or option is invalid."),
            (3, "The runtime refused the query."),
            (5, "The runtime or requested job was not found."),
            (10, "Job query or IPC failed."),
        ],
    },
    HelpSpec {
        command: "job list",
        summary: "List point-in-time durable status for every job in this runtime.",
        usage: "arkforge job list",
        effect: "Read-only. It does not watch, resume, cancel, reconcile, or mutate a job.",
        requires: &["A running ArkForge runtime."],
        produces: &["arkforge.job-list/v1, including an empty jobs array."],
        options: &[],
        examples: &["arkforge --output json job list"],
        next: &["arkforge job show --job <job-id>"],
        exits: &[
            (0, "Job list produced."),
            (5, "The runtime is not available."),
            (10, "Job query or IPC failed."),
        ],
    },
    HelpSpec {
        command: "job show",
        summary: "Show one exact durable job summary.",
        usage: "arkforge job show --job <job-id>",
        effect: "Read-only point-in-time status. It neither waits for events nor changes the job.",
        requires: &["One exact job id from job list or a prior command result."],
        produces: &[
            "arkforge.job/v1 with plan binding, state, progress, sequence, and stopped reason.",
        ],
        options: &[("--job <job-id>", "Exact durable job id; required.")],
        examples: &["arkforge --output json job show --job <job-id>"],
        next: &["If state is outcomeUnknown, run 'arkforge job recovery guide --job <job-id>'."],
        exits: &[
            (0, "Job summary produced."),
            (2, "The job id is missing."),
            (5, "The runtime or job was not found."),
            (10, "Job query or IPC failed."),
        ],
    },
    HelpSpec {
        command: "job recovery",
        summary: "Read safe recovery guidance without replaying or reconciling the original intent.",
        usage: "arkforge job recovery guide --job <job-id>",
        effect: "Read-only. The original outcome and intent remain immutable and automatic replay stays forbidden.",
        requires: &["One exact durable job id."],
        produces: &[
            "Typed operator actions and complete-overwrite recovery contract facts, when available.",
        ],
        options: &[],
        examples: &["arkforge help job recovery guide --format json"],
        next: &["arkforge job recovery guide --job <job-id>"],
        exits: &[
            (0, "Recovery guide produced."),
            (2, "Command or option is invalid."),
            (5, "The runtime or job was not found."),
            (10, "Recovery query or IPC failed."),
        ],
    },
    HelpSpec {
        command: "job recovery guide",
        summary: "Show the no-replay recovery guide for one durable job.",
        usage: "arkforge job recovery guide --job <job-id>",
        effect: "Read-only. It never replays the original intent, reuses a permit, or creates a superseding plan.",
        requires: &["One exact durable job id."],
        produces: &[
            "arkforge.recovery-guide/v1 with immutable/no-replay guards, ordered actions, and recovery contract identity.",
        ],
        options: &[("--job <job-id>", "Exact durable job id; required.")],
        examples: &["arkforge --output json job recovery guide --job <job-id>"],
        next: &[
            "Follow actions[] in order through the paired authority; never replay the original intent.",
        ],
        exits: &[
            (0, "Recovery guide produced."),
            (2, "The job id is missing."),
            (5, "The runtime or job was not found."),
            (10, "Recovery query or IPC failed."),
        ],
    },
    HelpSpec {
        command: "rescue",
        summary: "Perform an explicit native RockUSB recovery operation.",
        usage: "arkforge rescue <list|inspect|read|plan|apply> [options]",
        effect: "Rescue is never automatic. It uses ArkForge's compiled-in RockUSB protocol and cannot produce a normal-flash receipt.",
        requires: &["A DAYU200 in Loader mode for device operations."],
        produces: &["Read evidence, a sealed RescuePlan, or a separate RescueReceipt."],
        options: &[],
        examples: &[
            "arkforge rescue list",
            "arkforge help rescue plan --format json",
        ],
        next: &["arkforge rescue list"],
        exits: &[
            (0, "Command succeeded."),
            (2, "Inputs are invalid."),
            (3, "Safety precondition refused."),
            (8, "Mutation outcome is unknown; never replay the plan."),
        ],
    },
    HelpSpec {
        command: "rescue list",
        summary: "List exact DAYU200 Loader observations reachable by native RockUSB.",
        usage: "arkforge rescue list",
        effect: "Read-only USB enumeration and readiness classification. The device is not mutated.",
        requires: &[],
        produces: &["A list of opaque rescue device IDs; raw serial values are not printed."],
        options: &[],
        examples: &["arkforge --output json rescue list"],
        next: &["arkforge rescue inspect --device <device-id>"],
        exits: &[
            (0, "List produced, including an empty list."),
            (3, "Native USB enumeration was refused."),
        ],
    },
    HelpSpec {
        command: "rescue inspect",
        summary: "Read capacity and the partition table from one exact Loader observation.",
        usage: "arkforge rescue inspect --device <device-id>",
        effect: "Read-only native RockUSB commands. The device is not mutated.",
        requires: &["One exact device ID returned by the current 'rescue list'."],
        produces: &[
            "Capacity, layout digest, partition extents, evidence digests, and profile compatibility.",
        ],
        options: &[(
            "--device <device-id>",
            "Exact current Loader observation; required.",
        )],
        examples: &["arkforge --output json rescue inspect --device <device-id>"],
        next: &["arkforge rescue plan --device <device-id> --operation reset-device"],
        exits: &[
            (0, "Inspection produced."),
            (2, "Inputs are invalid."),
            (3, "Exact device or native read was refused."),
        ],
    },
    HelpSpec {
        command: "rescue read",
        summary: "Read a bounded sector range from one exact Loader device into a new file.",
        usage: "arkforge rescue read --device <device-id> --start-sector <u64> --sector-count <u64> --out <new-file>",
        effect: "Reads the device and creates one host file. It never overwrites the output and does not mutate the device.",
        requires: &[
            "An exact current device ID.",
            "A range within reported capacity and at most 512 MiB.",
            "An output path that does not exist.",
        ],
        produces: &["A new file plus byte count and SHA-256 read receipt."],
        options: &[
            (
                "--device <device-id>",
                "Exact current Loader observation; required.",
            ),
            (
                "--start-sector <u64>",
                "First 512-byte logical sector; required.",
            ),
            (
                "--sector-count <u64>",
                "Number of sectors to read; required and nonzero.",
            ),
            (
                "--out <new-file>",
                "New output file; required and never overwritten.",
            ),
        ],
        examples: &[
            "arkforge rescue read --device <device-id> --start-sector 0 --sector-count 64 --out ./sectors.bin",
        ],
        next: &["Compare the returned sha256 with independently trusted evidence."],
        exits: &[
            (0, "Read completed and the file was synced."),
            (2, "Inputs are invalid."),
            (3, "Safety or range check refused."),
            (7, "Native read failed after USB I/O began."),
        ],
    },
    HelpSpec {
        command: "rescue plan",
        summary: "Seal one native write-partition or reset-device operation without mutating the device.",
        usage: "arkforge rescue plan --device <device-id> --operation <write-partition|reset-device> [write options]",
        effect: "Reads current device facts and writes a short-lived content-addressed RescuePlan to host state. The device is not mutated.",
        requires: &[
            "One exact current Loader device.",
            "For write-partition: a profile-allowed partition, image path, and independently supplied image SHA-256.",
        ],
        produces: &[
            "plan_id, plan_sha256, expiry, sealed effects, and the exact acknowledgement set required by apply.",
        ],
        options: &[
            (
                "--device <device-id>",
                "Exact current Loader observation; required.",
            ),
            (
                "--operation <operation>",
                "write-partition or reset-device; required.",
            ),
            (
                "--partition <name>",
                "Profile-allowed partition; required for write-partition.",
            ),
            (
                "--image <file>",
                "Image bytes to import; required for write-partition.",
            ),
            (
                "--expect-image-sha256 <sha256>",
                "Caller expectation; required for write-partition.",
            ),
        ],
        examples: &[
            "arkforge rescue plan --device <device-id> --operation write-partition --partition boot_linux --image ./boot_linux.img --expect-image-sha256 <sha256>",
            "arkforge rescue plan --device <device-id> --operation reset-device",
        ],
        next: &["Use the returned next_commands entry verbatim after reviewing its effect tokens."],
        exits: &[
            (0, "Plan sealed; no device mutation occurred."),
            (2, "Inputs are invalid."),
            (3, "Image, target, layout, or exact device was refused."),
        ],
    },
    HelpSpec {
        command: "rescue apply",
        summary: "Apply one exact, short-lived, single-use native RescuePlan.",
        usage: "arkforge rescue apply --plan <plan-id> --expect-plan-sha256 <sha256> --ack <token> [--ack <token>...]",
        effect: "Destructive for write-partition and mutating for reset-device. Records a durable one-shot intent before native USB mutation and never replays it.",
        requires: &[
            "The exact plan digest.",
            "Exactly every acknowledgement returned by plan, with no extras.",
            "Unchanged build, profile, device identity, and (for writes) partition layout.",
        ],
        produces: &[
            "A separate RescueReceipt with semantic-success, confirmed-no-effect, or outcome-unknown disposition.",
        ],
        options: &[
            ("--plan <plan-id>", "Stored rescue-plan:<sha256>; required."),
            (
                "--expect-plan-sha256 <sha256>",
                "Caller expectation; required.",
            ),
            (
                "--ack <token>",
                "Exact sealed effect acknowledgement; repeat as returned.",
            ),
        ],
        examples: &[
            "arkforge rescue apply --plan <plan-id> --expect-plan-sha256 <sha256> --ack rescue:native-rockusb --ack overwrite:partition=boot_linux",
        ],
        next: &[
            "On outcome-unknown, do not replay this plan; inspect and reconcile the device manually.",
        ],
        exits: &[
            (0, "Native mutation has semantic success evidence."),
            (3, "A safety precondition refused before intent."),
            (4, "Acknowledgement set is not exact."),
            (6, "Plan already has an intent and cannot be replayed."),
            (
                7,
                "Intent exists but mutation is confirmed to have had no effect.",
            ),
            (
                8,
                "Intent exists and mutation outcome is unknown; never replay.",
            ),
            (10, "Local durable state failed."),
        ],
    },
    HelpSpec {
        command: "signing",
        summary: "Inspect a Mach-O file against the ArkForge signing contract.",
        usage: "arkforge signing verify --file <mach-o> --mode <development|release>",
        effect: "Read-only and offline. It reads one local Mach-O file and does not modify the file, host configuration, or a device.",
        requires: &["A thin or universal Mach-O file no larger than 64 MiB."],
        produces: &[
            "Observed signing facts and a typed contract verdict for every architecture slice.",
        ],
        options: &[],
        examples: &[
            "arkforge signing verify --file ./arkforged --mode development",
            "arkforge help signing verify --format json",
        ],
        next: &["arkforge signing verify --file <mach-o> --mode <development|release>"],
        exits: &[
            (
                0,
                "Signing verification completed and the file is compliant.",
            ),
            (2, "Command or option is invalid."),
            (
                3,
                "The file cannot be inspected or violates the selected contract.",
            ),
        ],
    },
    HelpSpec {
        command: "signing verify",
        summary: "Verify every Mach-O slice against one explicit signing mode.",
        usage: "arkforge signing verify --file <mach-o> --mode <development|release>",
        effect: "Read-only and offline. Development mode permits clean local ad-hoc signatures; release mode requires the complete shipping contract. Both modes require an empty entitlement dictionary.",
        requires: &[
            "An explicit file path.",
            "An explicit development or release mode; no mode is inferred.",
        ],
        produces: &[
            "arkforge.signing-verification/v1 with compliant, signing facts, stable violation codes, remediation, and the contract reference.",
        ],
        options: &[
            (
                "--file <mach-o>",
                "Thin or universal Mach-O file; required.",
            ),
            (
                "--mode <development|release>",
                "Signing contract strength; required.",
            ),
        ],
        examples: &[
            "arkforge --output json signing verify --file ./target/debug/arkforged --mode development",
            "arkforge signing verify --file ./target/release/arkforged --mode release",
        ],
        next: &[
            "If compliant is false, correct every violations[] item and repeat the exact command.",
        ],
        exits: &[
            (0, "The file meets the selected contract."),
            (2, "Required inputs are missing or invalid."),
            (
                3,
                "The file is unreadable as Mach-O or violates the selected contract.",
            ),
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globals_are_accepted_before_or_after_the_command() {
        let arguments = strings(&[
            "rescue",
            "list",
            "--output",
            "json",
            "--runtime-dir",
            "/tmp/arkforge-test",
        ]);
        let (globals, command) = parse_globals(&arguments).unwrap();
        assert_eq!(globals.output, Output::Json);
        assert_eq!(
            globals.runtime_dir,
            Some(PathBuf::from("/tmp/arkforge-test"))
        );
        assert_eq!(command, strings(&["rescue", "list"]));
    }

    #[test]
    fn rescue_options_reject_positionals_duplicates_and_unknowns() {
        assert!(Options::parse(&strings(&["device-id"])).is_err());
        let duplicate = Options::parse(&strings(&["--device", "a", "--device", "b"])).unwrap();
        assert!(duplicate.ensure_only(&["device"]).is_err());
        let unknown = Options::parse(&strings(&["--backend", "external"])).unwrap();
        assert!(unknown.ensure_only(&[]).is_err());
    }

    #[test]
    fn help_tree_has_every_implemented_leaf_and_agent_fields() {
        for topic in [
            "device list",
            "device probe",
            "artifact show",
            "flash assess",
            "job list",
            "job show",
            "job recovery guide",
            "rescue list",
            "rescue inspect",
            "rescue read",
            "rescue plan",
            "rescue apply",
            "signing verify",
        ] {
            let topic = strings(&topic.split_whitespace().collect::<Vec<_>>());
            let help = help_spec(&topic).unwrap();
            assert!(!help.effect.is_empty());
            assert!(!help.produces.is_empty());
            assert!(!help.examples.is_empty());
            assert!(!help.next.is_empty());
            assert!(!help.exits.is_empty());
        }
        let root_children = child_specs("")
            .iter()
            .map(|spec| spec.command)
            .collect::<Vec<_>>();
        assert_eq!(
            root_children,
            vec!["device", "artifact", "flash", "job", "rescue", "signing"]
        );
        assert_eq!(
            child_specs("job")
                .iter()
                .map(|spec| spec.command)
                .collect::<Vec<_>>(),
            vec!["job list", "job show", "job recovery"]
        );
        assert_eq!(child_specs("job recovery")[0].command, "job recovery guide");
        assert_eq!(child_specs("signing")[0].command, "signing verify");
        assert_eq!(HELP_SCHEMA, "arkforge.command-help/v1");
    }

    #[test]
    fn signing_requires_explicit_file_and_mode_options() {
        let valid =
            Options::parse(&strings(&["--file", "./arkforged", "--mode", "release"])).unwrap();
        valid.ensure_only(&["file", "mode"]).unwrap();
        assert_eq!(valid.one("file").unwrap(), "./arkforged");
        assert_eq!(valid.one("mode").unwrap(), "release");

        let implicit_release = Options::parse(&strings(&["--release", "true"])).unwrap();
        assert!(implicit_release.ensure_only(&["file", "mode"]).is_err());
    }

    #[test]
    fn json_escaping_covers_agent_visible_control_characters() {
        assert_eq!(json("a\n\"b\\c\t"), "\"a\\n\\\"b\\\\c\\t\"");
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }
}
