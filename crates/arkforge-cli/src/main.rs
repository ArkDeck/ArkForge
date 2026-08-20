//! Canonical ArkForge command frontend and local authority process.
//!
//! Explicit native rescue and read-only host diagnostics land here before the
//! normal-flash authority surface. No canonical command is a compatibility
//! wrapper around an older binary.

mod authority_support;
mod controller_client;
mod hdc_control;
mod supervisor;

use arkforge_artifact::cas::{CasError, CasQuota, ContentAddressedStore, ImportedObject};
use arkforge_core::profile;
use arkforge_core::{OpaqueId, Sha256Digest, Version};
use arkforge_ipc::messages::{
    Assessment, Effect, ExecutablePlan, InspectArtifactResponse, JobEvent, JobSummary, KeyValue,
    MaterializePlanResponse,
};
use arkforged::artifact_ops::{
    ProfileCoverage, inspect_container, manifest_response, profile_coverage,
};
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
use std::fs::File;
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
            "json" | "jsonl" => Ok(Self::Json),
            _ => Err(CliError::invalid(
                "--output accepts exactly 'human', 'json', or 'jsonl'.",
            )),
        }
    }
}

#[derive(Debug)]
struct Globals {
    runtime_dir: Option<PathBuf>,
    output: Output,
    jsonl: bool,
    no_color: bool,
    quiet: bool,
    verbose: bool,
}

#[derive(Debug)]
struct CliError {
    code: String,
    message: String,
    exit_code: i32,
    retryable: bool,
    required_acknowledgements: Vec<String>,
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
            required_acknowledgements: Vec::new(),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new("INVALID_ARGUMENT", message, 2, false)
    }

    fn with_required_acknowledgements(mut self, tokens: Vec<String>) -> Self {
        if !tokens.is_empty() {
            self.retryable = true;
        }
        self.required_acknowledgements = tokens;
        self
    }
}

impl From<RescueError> for CliError {
    fn from(error: RescueError) -> Self {
        let required_acknowledgements = if error.code == "ACKNOWLEDGEMENT_REQUIRED" {
            bracketed_values_after(&error.message, "Missing: [")
        } else {
            Vec::new()
        };
        Self {
            code: error.code.to_string(),
            message: error.message,
            exit_code: error.exit_code,
            retryable: error.retryable,
            required_acknowledgements,
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
            required_acknowledgements: Vec::new(),
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct TypedOptionSpec {
    name: String,
    value_type: String,
    required: bool,
    repeatable: bool,
    enum_values: Vec<String>,
    sensitive: bool,
    effect_relevant: bool,
    requires: Vec<String>,
    conflicts: Vec<String>,
    description: &'static str,
}

impl HelpSpec {
    fn path(&self) -> Vec<&str> {
        self.command.split_whitespace().collect()
    }

    fn typed_options(&self) -> Vec<TypedOptionSpec> {
        self.options
            .iter()
            .filter_map(|(signature, description)| {
                let name = signature
                    .split(|character: char| character == ',' || character.is_whitespace())
                    .find_map(|part| part.strip_prefix("--"))?
                    .trim_end_matches(',')
                    .to_string();
                let shape = signature
                    .split_once('<')
                    .and_then(|(_, rest)| rest.split_once('>').map(|(shape, _)| shape));
                let enum_values: Vec<String> = shape
                    .filter(|shape| shape.contains('|') || matches!(*shape, "full-restore"))
                    .map(|shape| shape.split('|').map(str::to_string).collect())
                    .unwrap_or_default();
                let value_type = match (name.as_str(), shape) {
                    ("artifact", _) => "sha256",
                    (_, None) => "boolean",
                    (_, Some("u64")) => "uint64",
                    (_, Some("sha256")) => "sha256",
                    (
                        _,
                        Some(
                            "file" | "new-file" | "firmware-file" | "mach-o" | "absolute-path"
                            | "dir",
                        ),
                    ) => "path",
                    (_, Some("profile-id" | "id@version")) => "profile-id",
                    (_, Some("observation-id" | "device-id")) => "observation-id",
                    (_, Some("job-id")) => "job-id",
                    (_, Some("plan-id")) => "plan-id",
                    (_, Some("campaign-id")) => "campaign-id",
                    (_, Some("name")) if name == "partition" => "partition-id",
                    (_, Some("mode")) => "device-mode",
                    (_, Some("token")) => "acknowledgement-token",
                    (_, Some(_)) if !enum_values.is_empty() => "enum",
                    (_, Some(_)) => "string",
                }
                .to_string();
                let prose = description.to_ascii_lowercase();
                let required = (prose.contains("required.")
                    && !prose.contains("required for")
                    && !prose.contains("when required"))
                    || (name == "ack" && matches!(self.command, "flash apply" | "rescue apply"));
                let repeatable = prose.contains("repeatable") || prose.contains("repeat ");
                let mut requires = Vec::new();
                for token in description.split_whitespace() {
                    if let Some(required) = token.strip_prefix("--") {
                        let required = required.trim_end_matches(['.', ',', ';']);
                        if required != name && prose.contains("requires") {
                            requires.push(required.to_string());
                        }
                    }
                }
                let conflicts = match name.as_str() {
                    "quiet" => vec!["verbose".into()],
                    "verbose" => vec!["quiet".into()],
                    _ => Vec::new(),
                };
                let effect_relevant = !matches!(
                    name.as_str(),
                    "output"
                        | "no-color"
                        | "quiet"
                        | "verbose"
                        | "help"
                        | "version"
                        | "format"
                        | "shell"
                        | "detach"
                        | "timeout-ms"
                        | "after-sequence"
                );
                let sensitive = matches!(
                    name.as_str(),
                    "runtime-dir" | "file" | "profile-file" | "image" | "out" | "hdc"
                );
                Some(TypedOptionSpec {
                    name,
                    value_type,
                    required,
                    repeatable,
                    enum_values,
                    sensitive,
                    effect_relevant,
                    requires,
                    conflicts,
                    description,
                })
            })
            .collect()
    }

    fn effect_class(&self) -> &'static str {
        match self.command {
            "flash apply" | "rescue apply" => "destructive",
            "job cancel" => "mutating-control",
            "artifact import" | "rescue read" => "host-write",
            "flash plan" | "job recovery plan" | "rescue plan" => "read-device-and-host-write",
            "daemon status" => "read-only",
            command if command.starts_with("daemon") => "service-lifecycle",
            _ => "read-only",
        }
    }

    fn output_schemas(&self) -> Vec<String> {
        let mut schemas = self
            .produces
            .iter()
            .flat_map(|description| description.split_whitespace())
            .map(|word| {
                word.trim_matches(|character: char| {
                    matches!(character, ',' | '.' | ';' | ':' | '(' | ')')
                })
            })
            .filter(|word| word.starts_with("arkforge.") && word.contains("/v1"))
            .map(str::to_string)
            .collect::<Vec<_>>();
        schemas.sort();
        schemas.dedup();
        if schemas.is_empty() {
            schemas.push("arkforge.command-help/v1".into());
        }
        schemas
    }

    fn requires_controller(&self) -> bool {
        matches!(
            self.command,
            "flash assess"
                | "flash plan"
                | "flash apply"
                | "job cancel"
                | "job reconcile"
                | "job recovery plan"
                | "daemon stop"
        )
    }

    fn requires_daemon(&self) -> bool {
        self.command.starts_with("device ")
            || self.command == "artifact show"
            || self.command.starts_with("flash ")
            || self.command.starts_with("job ")
            || matches!(self.command, "daemon status" | "daemon stop")
    }
}

fn validate_against_command_tree(arguments: &[String]) -> Result<(), CliError> {
    let path_len = arguments
        .iter()
        .position(|argument| argument.starts_with('-'))
        .unwrap_or(arguments.len());
    let path = &arguments[..path_len];
    let spec = help_spec(path)?;
    let metadata = spec.typed_options();
    let known = metadata
        .iter()
        .map(|option| (option.name.as_str(), option))
        .collect::<BTreeMap<_, _>>();
    let mut supplied: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut index = path_len;
    while index < arguments.len() {
        let token = &arguments[index];
        let name = token.strip_prefix("--").ok_or_else(|| {
            CliError::invalid(format!(
                "Unexpected positional argument {token:?}; command inputs are named options."
            ))
        })?;
        let option = known
            .get(name)
            .ok_or_else(|| CliError::invalid(format!("Unknown option --{name}.")))?;
        if option.value_type == "boolean" {
            supplied.entry(name).or_default().push("true");
            index += 1;
            continue;
        }
        let value = arguments.get(index + 1).ok_or_else(|| {
            CliError::invalid(format!("--{name} requires a {} value.", option.value_type))
        })?;
        if value.starts_with("--") {
            return Err(CliError::invalid(format!(
                "--{name} requires a {} value.",
                option.value_type
            )));
        }
        if value.contains('<') || value.contains('>') || value == "..." || value.contains('…') {
            return Err(CliError::invalid(format!(
                "--{name} requires a concrete value; help placeholders and ellipses are not inputs."
            )));
        }
        if !option.enum_values.is_empty()
            && !option.enum_values.iter().any(|allowed| allowed == value)
        {
            return Err(CliError::invalid(format!(
                "--{name} accepts exactly {}, not {value:?}.",
                option.enum_values.join(", ")
            )));
        }
        match option.value_type.as_str() {
            "uint64" => {
                value.parse::<u64>().map_err(|_| {
                    CliError::invalid(format!("--{name} requires an unsigned 64-bit integer."))
                })?;
            }
            "sha256" => {
                let digest = Sha256Digest::parse_hex(value).map_err(|error| {
                    CliError::invalid(format!("--{name} requires one SHA-256 digest: {error}"))
                })?;
                if digest.to_hex() != *value {
                    return Err(CliError::invalid(format!(
                        "--{name} requires canonical 64-character lowercase SHA-256 hex."
                    )));
                }
            }
            "profile-id" => validate_profile_id(name, value)?,
            "observation-id" | "job-id" | "plan-id" | "campaign-id" | "partition-id"
            | "device-mode" => validate_opaque_id(name, value)?,
            "acknowledgement-token" => validate_acknowledgement(name, value)?,
            _ => {}
        }
        supplied.entry(name).or_default().push(value);
        index += 2;
    }
    for option in &metadata {
        let count = supplied
            .get(option.name.as_str())
            .map_or(0, |values| values.len());
        if option.required && count == 0 {
            return Err(CliError::invalid(format!(
                "Missing required --{}.",
                option.name
            )));
        }
        if !option.repeatable && count > 1 {
            return Err(CliError::invalid(format!(
                "--{} may be supplied only once.",
                option.name
            )));
        }
        if count > 0 {
            for required in &option.requires {
                if !supplied.contains_key(required.as_str()) {
                    return Err(CliError::invalid(format!(
                        "--{} requires --{}.",
                        option.name, required
                    )));
                }
            }
            for conflict in &option.conflicts {
                if supplied.contains_key(conflict.as_str()) {
                    return Err(CliError::invalid(format!(
                        "--{} conflicts with --{}.",
                        option.name, conflict
                    )));
                }
            }
        }
    }
    if spec.command == "rescue plan" {
        let operation = supplied
            .get("operation")
            .and_then(|values| values.first())
            .copied()
            .unwrap_or_default();
        let write_options = ["partition", "image", "expect-image-sha256"];
        if operation == "write-partition" {
            if let Some(missing) = write_options
                .iter()
                .find(|name| !supplied.contains_key(**name))
            {
                return Err(CliError::invalid(format!(
                    "--operation write-partition requires --{missing}."
                )));
            }
        } else if let Some(present) = write_options
            .iter()
            .find(|name| supplied.contains_key(**name))
        {
            return Err(CliError::invalid(format!(
                "--{present} is not valid for --operation reset-device."
            )));
        }
    }
    if matches!(spec.command, "daemon run" | "daemon start")
        && supplied.contains_key("hdc") != supplied.contains_key("expect-hdc-sha256")
    {
        return Err(CliError::invalid(
            "--hdc and --expect-hdc-sha256 must be supplied together.",
        ));
    }
    Ok(())
}

fn validate_opaque_id(option: &str, value: &str) -> Result<(), CliError> {
    OpaqueId::new(value).map_err(|error| {
        CliError::invalid(format!(
            "--{option} requires a canonical ArkForge identifier: {error}"
        ))
    })?;
    Ok(())
}

fn validate_profile_id(option: &str, value: &str) -> Result<(), CliError> {
    let (id, version) = value.rsplit_once('@').ok_or_else(|| {
        CliError::invalid(format!(
            "--{option} requires an exact profile id@major.minor.patch."
        ))
    })?;
    validate_opaque_id(option, id)?;
    Version::parse(version).ok_or_else(|| {
        CliError::invalid(format!(
            "--{option} requires an exact profile id@major.minor.patch."
        ))
    })?;
    Ok(())
}

fn validate_acknowledgement(option: &str, value: &str) -> Result<(), CliError> {
    if value.is_empty()
        || value.len() > 256
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-' | b'=')
        })
    {
        return Err(CliError::invalid(format!(
            "--{option} requires one exact ASCII acknowledgement token."
        )));
    }
    Ok(())
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let fallback_output = requested_output(&arguments).unwrap_or(Output::Human);
    let command_path = requested_command_path(&arguments);
    match run(&arguments) {
        Ok(exit_code) => std::process::exit(exit_code),
        Err(error) => {
            print_error(fallback_output, &command_path, &arguments, &error);
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
                "{{\"schema\":\"arkforge.version/v1\",\"name\":\"arkforge\",\"version\":{}}}",
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
    validate_against_command_tree(&command)?;
    match command[0].as_str() {
        "doctor" => run_doctor(globals),
        "device" => run_device(&command[1..], globals),
        "artifact" => run_artifact(&command[1..], globals),
        "flash" => run_flash(&command[1..], globals),
        "job" => run_job(&command[1..], globals),
        "rescue" => run_rescue(&command[1..], globals),
        "daemon" => run_daemon(&command[1..], globals),
        "signing" => run_signing(&command[1..], globals.output),
        "completion" => run_completion(&command[1..], globals.output),
        other => Err(CliError::invalid(format!(
            "Unknown command {other:?}. Run 'arkforge help' for the command tree."
        ))),
    }
}

fn run_daemon(arguments: &[String], globals: Globals) -> Result<i32, CliError> {
    let Some(subcommand) = arguments.first() else {
        print_help(help_spec(&["daemon".into()])?, globals.output);
        return Ok(0);
    };
    let runtime_dir = command_runtime_dir(&globals)?;
    match subcommand.as_str() {
        "run" => {
            let options = supervisor::DaemonOptions::parse(&arguments[1..])?;
            supervisor::run(
                runtime_dir,
                options,
                globals.output == Output::Human && !globals.quiet,
            )?;
            Ok(0)
        }
        "start" => {
            let options = supervisor::DaemonOptions::parse(&arguments[1..])?;
            let status = supervisor::start(runtime_dir, options)?;
            print_daemon_status(&status, globals.output);
            Ok(0)
        }
        "status" => {
            reject_extra(&arguments[1..], "daemon status")?;
            let status = supervisor::status(&runtime_dir)?;
            print_daemon_status(&status, globals.output);
            Ok(0)
        }
        "stop" => {
            reject_extra(&arguments[1..], "daemon stop")?;
            let status = supervisor::stop(&runtime_dir)?;
            match globals.output {
                Output::Human => {
                    println!("ArkForge runtime stopped (epoch {}).", status.epoch);
                    println!("Next: arkforge daemon start");
                }
                Output::Json => println!(
                    "{{\"schema\":\"arkforge.daemon-stop/v1\",\"stopped\":true,\"pairing_epoch\":{},\"next_commands\":[\"arkforge daemon start\"]}}",
                    status.epoch
                ),
            }
            Ok(0)
        }
        other => Err(CliError::invalid(format!(
            "Unknown daemon command {other:?}. Run 'arkforge help daemon'."
        ))),
    }
}

fn print_daemon_status(status: &supervisor::DaemonStatus, output: Output) {
    let next = daemon_next_commands(status);
    match output {
        Output::Human => {
            println!("ArkForge runtime: running");
            println!("  supervisor pid: {}", status.supervisor_pid);
            println!("  mechanics pid: {}", status.daemon_pid);
            println!(
                "  protocol: {}.{}",
                status.protocol_major, status.protocol_minor
            );
            println!("  daemon: {}", status.daemon_version);
            println!("  authority: arkforge.cli (epoch {})", status.epoch);
            println!("  mechanics ready: {}", status.mechanics_ready);
            println!(
                "  authority support available: {}",
                status.authority_support_available
            );
            println!("  HDC bound: {}", status.hdc_bound);
            if status.hdc_bound {
                println!("  HDC SHA-256: {}", status.hdc_sha256);
            }
            if !status.hardware_campaign.is_empty() {
                println!("  hardware campaign: {}", status.hardware_campaign);
            }
            println!("  active jobs: {}", status.active_jobs);
            if !status.blockers.is_empty() {
                println!("  blockers: {}", status.blockers.join(", "));
            }
            println!("Next: {}", next[0]);
        }
        Output::Json => println!(
            "{{\"schema\":\"arkforge.daemon-status/v1\",\"running\":true,\"supervisor_pid\":{},\"daemon_pid\":{},\"protocol\":{{\"major\":{},\"minor\":{}}},\"daemon_version\":{},\"authority\":{{\"namespace\":\"arkforge.cli\",\"pairing_epoch\":{},\"support_records_available\":{},\"hardware_campaign\":{},\"hdc\":{{\"bound\":{},\"sha256\":{}}}}},\"mechanics_ready\":{},\"active_jobs\":{},\"blockers\":{},\"next_commands\":{}}}",
            status.supervisor_pid,
            status.daemon_pid,
            status.protocol_major,
            status.protocol_minor,
            json(&status.daemon_version),
            status.epoch,
            status.authority_support_available,
            optional_json(
                (!status.hardware_campaign.is_empty()).then_some(status.hardware_campaign.as_str())
            ),
            status.hdc_bound,
            optional_json(status.hdc_bound.then_some(status.hdc_sha256.as_str())),
            status.mechanics_ready,
            status.active_jobs,
            json_strings(&status.blockers),
            json_strings(&next),
        ),
    }
}

fn daemon_next_commands(status: &supervisor::DaemonStatus) -> Vec<String> {
    if status.active_jobs > 0 {
        return vec!["arkforge job list".into()];
    }
    if !status.hdc_bound || !status.authority_support_available {
        return vec![
            "arkforge daemon stop".into(),
            "arkforge help daemon start --format json".into(),
        ];
    }
    if status.mechanics_ready && status.blockers.is_empty() {
        vec!["arkforge device list".into()]
    } else {
        vec!["arkforge daemon status".into()]
    }
}

fn parse_globals(arguments: &[String]) -> Result<(Globals, Vec<String>), CliError> {
    let mut runtime_dir = None;
    let mut output = Output::Human;
    let mut jsonl = false;
    let mut output_seen = false;
    let mut no_color = false;
    let mut quiet = false;
    let mut verbose = false;
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
                    .ok_or_else(|| CliError::invalid("--output requires human, json, or jsonl."))?;
                output = Output::parse(value)?;
                jsonl = value == "jsonl";
            }
            "--no-color" => no_color = true,
            "--quiet" => quiet = true,
            "--verbose" => verbose = true,
            argument => command.push(argument.to_string()),
        }
        index += 1;
    }
    if quiet && verbose {
        return Err(CliError::invalid(
            "--quiet conflicts with --verbose; select at most one presentation mode.",
        ));
    }
    if output == Output::Json {
        no_color = true;
    }
    Ok((
        Globals {
            runtime_dir,
            output,
            jsonl,
            no_color,
            quiet,
            verbose,
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
                output = match arguments
                    .get(index)
                    .map(String::as_str)
                    .ok_or_else(|| CliError::invalid("--format requires human or json."))?
                {
                    "human" => Output::Human,
                    "json" => Output::Json,
                    value => {
                        return Err(CliError::invalid(format!(
                            "--format accepts exactly 'human' or 'json', not {value:?}."
                        )));
                    }
                };
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

fn run_doctor(globals: Globals) -> Result<i32, CliError> {
    let runtime_dir = command_runtime_dir(&globals)?;
    let platform_supported = cfg!(target_os = "macos");
    let runtime = supervisor::status(&runtime_dir).ok();
    let runtime_running = runtime.is_some();
    let mechanics_ready = runtime
        .as_ref()
        .is_some_and(|status| status.mechanics_ready);
    let blockers = runtime
        .as_ref()
        .map(|status| status.blockers.clone())
        .unwrap_or_else(|| vec!["DAEMON_UNAVAILABLE".into()]);
    let inspect_ready = platform_supported;
    let authority_ready = runtime
        .as_ref()
        .is_some_and(|status| status.authority_support_available);
    let hdc_bound = runtime.as_ref().is_some_and(|status| status.hdc_bound);
    let execute_ready = platform_supported
        && mechanics_ready
        && authority_ready
        && hdc_bound
        && blockers.is_empty();
    let next = runtime.as_ref().map_or_else(
        || vec!["arkforge daemon start".to_string()],
        daemon_next_commands,
    );

    match globals.output {
        Output::Human => {
            if !globals.quiet {
                println!("ArkForge host assessment");
            }
            println!("  platform supported: {platform_supported}");
            println!("  inspect ready:      {inspect_ready}");
            println!("  runtime running:    {runtime_running}");
            println!("  mechanics ready:    {mechanics_ready}");
            println!("  authority ready:    {authority_ready}");
            println!("  HDC bound:          {hdc_bound}");
            println!("  execute ready:      {execute_ready}");
            if globals.verbose {
                println!("  structured output disables color: {}", globals.no_color);
                for blocker in &blockers {
                    println!("  blocker: {blocker}");
                }
            }
            println!("Next: {}", next[0]);
        }
        Output::Json => println!(
            "{{\"schema\":\"arkforge.doctor/v1\",\"ok\":true,\"platform_supported\":{},\"inspect_ready\":{},\"runtime_running\":{},\"mechanics_ready\":{},\"authority_support_available\":{},\"hdc_bound\":{},\"execute_ready\":{},\"blockers\":{},\"next_commands\":{}}}",
            platform_supported,
            inspect_ready,
            runtime_running,
            mechanics_ready,
            authority_ready,
            hdc_bound,
            execute_ready,
            json_strings(&blockers),
            json_strings(&next),
        ),
    }
    Ok(0)
}

fn run_completion(arguments: &[String], output: Output) -> Result<i32, CliError> {
    let options = Options::parse(arguments)?;
    let shell = options.one("shell")?;
    let script = completion_script(shell)?;
    match output {
        Output::Human => print!("{script}"),
        Output::Json => println!(
            "{{\"schema\":\"arkforge.completion/v1\",\"ok\":true,\"shell\":{},\"script\":{}}}",
            json(shell),
            json(&script)
        ),
    }
    Ok(0)
}

fn completion_script(shell: &str) -> Result<String, CliError> {
    let mut words = BTreeSet::new();
    for spec in HELP {
        words.extend(spec.command.split_whitespace().map(str::to_string));
        for option in spec.typed_options() {
            words.insert(format!("--{}", option.name));
        }
    }
    let words = words.into_iter().collect::<Vec<_>>().join(" ");
    match shell {
        "bash" => Ok(format!(
            "_arkforge_complete() {{\n  local cur=\"${{COMP_WORDS[COMP_CWORD]}}\"\n  COMPREPLY=( $(compgen -W '{}' -- \"$cur\") )\n}}\ncomplete -F _arkforge_complete arkforge\n",
            words
        )),
        "zsh" => Ok(format!(
            "#compdef arkforge\n_arkforge() {{\n  local -a words\n  words=({})\n  _describe 'arkforge command or option' words\n}}\ncompdef _arkforge arkforge\n",
            words
        )),
        "fish" => Ok(format!("complete -c arkforge -f -a '{}'\n", words)),
        value => Err(CliError::invalid(format!(
            "--shell accepts 'bash', 'zsh', or 'fish', not {value:?}."
        ))),
    }
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
    let input = std::fs::read(file).map_err(|error| {
        CliError::new(
            "SIGNING_INPUT_REFUSED",
            format!("Unable to read the selected signing input: {error}"),
            3,
            false,
        )
    })?;
    let input_digest = arkforge_core::digest::sha256(&input);
    let code = packaging::read(&input).map_err(|error| {
        CliError::new(
            "SIGNING_INPUT_REFUSED",
            format!("Unable to inspect the selected signing input: {error}"),
            3,
            false,
        )
    })?;
    let violations = code.violations(mode);
    print_signing(output, file, input_digest, mode_name, &code, &violations);
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
            let mut client = public_client(&globals)?;
            let observations = client.device_list()?;
            print_device_observations(globals.output, &observations);
            Ok(0)
        }
        "show" => {
            let device = options.one("device")?;
            let mut client = public_client(&globals)?;
            let observations = client.device_list()?;
            let observation = observations
                .iter()
                .find(|observation| observation.observation_id == device)
                .ok_or_else(|| {
                    CliError::new(
                        "OBSERVATION_NOT_FOUND",
                        format!("No current observation has id {device}."),
                        5,
                        false,
                    )
                })?;
            print_device_observation(globals.output, observation);
            Ok(0)
        }
        "probe" => {
            let device = options.one("device")?;
            let profile = options.one("profile")?;
            let mut client = public_client(&globals)?;
            let probe = client.device_probe(device, profile)?;
            print_device_probe(globals.output, &probe);
            Ok(0)
        }
        "wait" => {
            let profile = options.one("profile")?;
            let mode = options.one("mode")?;
            if mode.trim().is_empty() {
                return Err(CliError::invalid("--mode cannot be empty."));
            }
            let timeout_ms = options
                .optional_one("timeout-ms")?
                .map(|value| parse_u64("--timeout-ms", value))
                .transpose()?
                .unwrap_or(30_000);
            let probe = wait_for_device(&globals, profile, mode, timeout_ms)?;
            print_device_wait(globals.output, profile, mode, timeout_ms, &probe);
            Ok(0)
        }
        other => Err(CliError::invalid(format!(
            "Unknown device command {other:?}. Run 'arkforge help device'."
        ))),
    }
}

fn wait_for_device(
    globals: &Globals,
    profile: &str,
    mode: &str,
    timeout_ms: u64,
) -> Result<DeviceProbeView, CliError> {
    let started = std::time::Instant::now();
    let mut client = public_client(globals)?;
    loop {
        let observations = client.device_list()?;
        let mut matches = Vec::new();
        for observation in observations
            .iter()
            .filter(|observation| observation.mode == mode)
        {
            match client.device_probe(&observation.observation_id, profile) {
                Ok(probe) => matches.push(probe),
                Err(error)
                    if matches!(
                        error.code.as_str(),
                        "PROBE_REFUSED" | "OBSERVATION_NOT_FOUND"
                    ) => {}
                Err(error) => return Err(error.into()),
            }
        }
        match matches.len() {
            1 => return Ok(matches.remove(0)),
            count if count > 1 => {
                return Err(CliError::new(
                    "AMBIGUOUS_DEVICE",
                    format!(
                        "{count} observations match profile {profile} in mode {mode}; an exact target cannot be selected."
                    ),
                    6,
                    true,
                ));
            }
            _ => {}
        }
        if started.elapsed().as_millis() as u64 >= timeout_ms {
            return Err(CliError::new(
                "DEVICE_WAIT_TIMEOUT",
                format!(
                    "No unique observation matched profile {profile} in mode {mode} within {timeout_ms} ms."
                ),
                5,
                true,
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn run_artifact(arguments: &[String], globals: Globals) -> Result<i32, CliError> {
    let Some(subcommand) = arguments.first() else {
        print_help(help_spec(&["artifact".into()])?, globals.output);
        return Ok(0);
    };
    let options = Options::parse(&arguments[1..])?;
    match subcommand.as_str() {
        "import" => {
            let file = Path::new(options.one("file")?);
            let expected = options
                .optional_one("expect-sha256")?
                .map(|value| parse_digest("--expect-sha256", value))
                .transpose()?;
            let metadata = std::fs::metadata(file).map_err(|error| {
                CliError::new(
                    "ARTIFACT_FILE_NOT_FOUND",
                    format!("Cannot read artifact input {}: {error}", file.display()),
                    5,
                    false,
                )
            })?;
            if !metadata.is_file() {
                return Err(CliError::invalid(format!(
                    "--file must name one regular file, not {}.",
                    file.display()
                )));
            }
            let store = open_artifact_store(&globals)?;
            let input = File::open(file).map_err(|error| {
                CliError::new(
                    "ARTIFACT_FILE_NOT_FOUND",
                    format!("Cannot open artifact input {}: {error}", file.display()),
                    5,
                    false,
                )
            })?;
            let imported = store
                .import(input, metadata.len(), expected)
                .map_err(artifact_store_error)?;
            print_artifact_import(globals.output, &imported);
            Ok(0)
        }
        "inspect" => {
            let artifact_id = options.one("artifact")?;
            let digest = parse_digest("--artifact", artifact_id)?;
            let store = open_existing_artifact_store(&globals)?;
            let object = store.open_object(&digest).map_err(artifact_store_error)?;
            let manifest = inspect_container(object)
                .map_err(|message| CliError::new("ARTIFACT_REJECTED", message, 3, false))?;
            let response = manifest_response(&manifest);
            let coverage = options
                .optional_one("profile-file")?
                .map(|path| load_profile_coverage(Path::new(path), &manifest))
                .transpose()?;
            print_artifact_inspection(globals.output, artifact_id, &response, coverage.as_ref());
            Ok(0)
        }
        "list" => {
            let objects = list_artifacts(&globals)?;
            print_artifact_list(globals.output, &objects);
            Ok(0)
        }
        "show" => {
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
            let intent = options.one("intent")?;
            if intent != "full-restore" {
                return Err(CliError::invalid(format!(
                    "--intent accepts exactly 'full-restore', not {intent:?}."
                )));
            }
            let artifact = options.one("artifact")?;
            let profile = options.one("profile")?;
            let device = options.one("device")?;
            let runtime_dir = command_runtime_dir(&globals)?;
            let assessment = supervisor::assess_plan(&runtime_dir, artifact, profile, device)?;
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
        "plan" => {
            let intent = options.one("intent")?;
            if intent != "full-restore" {
                return Err(CliError::invalid(format!(
                    "--intent accepts exactly 'full-restore', not {intent:?}."
                )));
            }
            let artifact = options.one("artifact")?;
            let profile = options.one("profile")?;
            let device = options.one("device")?;
            let runtime_dir = command_runtime_dir(&globals)?;
            match supervisor::materialize_plan(&runtime_dir, artifact, profile, device)? {
                MaterializePlanResponse::Plan(plan) => {
                    print_flash_plan(globals.output, &plan, &[]);
                    Ok(0)
                }
                MaterializePlanResponse::Assessment(assessment) => Err(CliError::new(
                    "PLAN_UNAVAILABLE",
                    format!(
                        "No executable plan was created: {}",
                        if assessment.unavailable_reason.is_empty() {
                            assessment.availability
                        } else {
                            assessment.unavailable_reason
                        }
                    ),
                    3,
                    false,
                )),
            }
        }
        "apply" => {
            let plan_id = options.one("plan")?;
            let expected = options.one("expect-plan-sha256")?;
            parse_digest("--expect-plan-sha256", expected)?;
            let acknowledgements = options.many_required("ack")?.to_vec();
            let detach = options.optional_one("detach")?.is_some();
            let runtime_dir = command_runtime_dir(&globals)?;
            let job_id =
                supervisor::apply_plan(&runtime_dir, plan_id, expected, &acknowledgements, detach)?;
            if detach {
                match globals.output {
                    Output::Human => {
                        println!("Started durable job {job_id}.");
                        println!("The authority supervisor continues to drive it.");
                        println!("Next: arkforge job watch --job {job_id}");
                    }
                    Output::Json => println!(
                        "{{\"schema\":\"arkforge.flash-apply/v1\",\"job_id\":{},\"detached\":true,\"authority_continues\":true,\"next_commands\":[{}]}}",
                        json(&job_id),
                        json(&format!("arkforge job watch --job {job_id}")),
                    ),
                }
                return Ok(0);
            }
            let (events, summary, _) = watch_job(&globals, &job_id, 0, u64::MAX)?;
            print_job_watch(
                &globals,
                &["flash", "apply"],
                0,
                u64::MAX,
                &events,
                &summary,
                false,
            );
            Ok(match summary.state.as_str() {
                "succeeded" => 0,
                "outcomeUnknown" => 8,
                "cancelledSafe" => 7,
                _ if summary.terminal => 7,
                _ => 9,
            })
        }
        other => Err(CliError::invalid(format!(
            "Unknown flash command {other:?}. Run 'arkforge help flash'."
        ))),
    }
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

fn print_flash_plan(output: Output, plan: &ExecutablePlan, extra_acknowledgements: &[String]) {
    let mut acknowledgements = plan_acknowledgements(plan);
    acknowledgements.extend_from_slice(extra_acknowledgements);
    acknowledgements.sort();
    acknowledgements.dedup();
    match output {
        Output::Human => {
            println!("Normal flash plan {}", plan.plan_id);
            println!("  plan SHA-256: {}", plan.plan_sha256);
            println!("  execution purpose: {}", plan.execution_purpose);
            println!("  expires: {}", plan.expires_at_epoch_ms);
            println!(
                "  mechanics: {} ({})",
                plan.mechanics_maturity_state, plan.mechanics_maturity_key_sha256
            );
            println!(
                "  authority: {} ({})",
                plan.authority_support_state, plan.authority_support_key_sha256
            );
            println!("  ordered steps: {}", plan.public_steps.len());
            println!("  persistent effects: {}", plan.persistent_effects.len());
            println!("Required acknowledgements:");
            for token in &acknowledgements {
                println!("  {token}");
            }
            let ack = acknowledgements
                .iter()
                .map(|token| format!(" --ack {token}"))
                .collect::<String>();
            println!(
                "Next: arkforge flash apply --plan {} --expect-plan-sha256 {}{}",
                plan.plan_id, plan.plan_sha256, ack
            );
        }
        Output::Json => {
            let steps = plan
                .public_steps
                .iter()
                .map(|step| {
                    format!(
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
                        json(&step.private_action_sha256),
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            let effects = effects_json(&plan.persistent_effects);
            println!(
                "{{\"schema\":\"arkforge.flash-plan/v1\",\"plan_id\":{},\"plan_sha256\":{},\"provider_execution_plan_sha256\":{},\"public_projection_sha256\":{},\"execution_purpose\":{},\"expires_at_epoch_ms\":{},\"mechanics_maturity\":{{\"key_sha256\":{},\"state\":{},\"campaign\":{}}},\"authority_support\":{{\"key_sha256\":{},\"state\":{},\"campaign\":{}}},\"ordered_steps\":[{}],\"persistent_effects\":{},\"required_acknowledgements\":{},\"device_mutated\":false,\"next_commands\":[{}]}}",
                json(&plan.plan_id),
                json(&plan.plan_sha256),
                json(&plan.provider_execution_plan_sha256),
                json(&plan.public_projection_sha256),
                json(&plan.execution_purpose),
                plan.expires_at_epoch_ms,
                json(&plan.mechanics_maturity_key_sha256),
                json(&plan.mechanics_maturity_state),
                optional_json(
                    (!plan.mechanics_maturity_campaign.is_empty())
                        .then_some(plan.mechanics_maturity_campaign.as_str())
                ),
                json(&plan.authority_support_key_sha256),
                json(&plan.authority_support_state),
                optional_json(
                    (!plan.authority_support_campaign.is_empty())
                        .then_some(plan.authority_support_campaign.as_str())
                ),
                steps,
                effects,
                json_strings(&acknowledgements),
                json(&format!(
                    "arkforge flash apply --plan {} --expect-plan-sha256 {}{}",
                    plan.plan_id,
                    plan.plan_sha256,
                    acknowledgements
                        .iter()
                        .map(|token| format!(" --ack {token}"))
                        .collect::<String>()
                )),
            );
        }
    }
}

fn run_job(arguments: &[String], globals: Globals) -> Result<i32, CliError> {
    let Some(subcommand) = arguments.first() else {
        print_help(help_spec(&["job".into()])?, globals.output);
        return Ok(0);
    };
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
            print_job(globals.output, &job);
            Ok(0)
        }
        "watch" => {
            let options = Options::parse(&arguments[1..])?;
            let job_id = options.one("job")?;
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
            let (events, summary, timed_out) =
                watch_job(&globals, job_id, after_sequence, timeout_ms)?;
            print_job_watch(
                &globals,
                &["job", "watch"],
                after_sequence,
                timeout_ms,
                &events,
                &summary,
                timed_out,
            );
            Ok(0)
        }
        "cancel" => {
            let options = Options::parse(&arguments[1..])?;
            let job_id = options.one("job")?;
            let expected_sequence =
                parse_u64("--expect-sequence", options.one("expect-sequence")?)?;
            let runtime_dir = command_runtime_dir(&globals)?;
            let state = supervisor::cancel_job(&runtime_dir, job_id, expected_sequence)?;
            match globals.output {
                Output::Human => {
                    println!("Cancellation result for {job_id}: {state}");
                    println!("The original journal remains durable; no action was replayed.");
                }
                Output::Json => println!(
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
        "recovery" => run_job_recovery(&arguments[1..], globals),
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
                println!("Next: arkforge job recovery guide --job {}", status.job_id);
            }
        }
        Output::Json => println!(
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
                    json(&format!(
                        "arkforge job recovery guide --job {}",
                        status.job_id
                    ))
                )
            } else {
                "[]".into()
            }
        ),
    }
}

fn watch_job(
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
            let job = options.one("job")?;
            let mut client = public_client(&globals)?;
            let guide = client.recovery_guide(job)?;
            print_recovery_guide(globals.output, &guide);
            Ok(0)
        }
        "plan" => {
            let options = Options::parse(&arguments[1..])?;
            let job_id = options.one("job")?;
            let artifact = options.one("artifact")?;
            let profile = options.one("profile")?;
            let device = options.one("device")?;
            let runtime_dir = command_runtime_dir(&globals)?;
            match supervisor::materialize_recovery_plan(
                &runtime_dir,
                job_id,
                artifact,
                profile,
                device,
            )? {
                MaterializePlanResponse::Plan(plan) => {
                    print_flash_plan(
                        globals.output,
                        &plan,
                        &[format!("recovery:supersedes-job={job_id}")],
                    );
                    Ok(0)
                }
                MaterializePlanResponse::Assessment(assessment) => Err(CliError::new(
                    "RECOVERY_PLAN_UNAVAILABLE",
                    format!(
                        "No executable superseding plan was created: {}",
                        if assessment.unavailable_reason.is_empty() {
                            assessment.availability
                        } else {
                            assessment.unavailable_reason
                        }
                    ),
                    3,
                    false,
                )),
            }
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

fn artifact_store_root(globals: &Globals) -> Result<PathBuf, CliError> {
    let runtime_dir = match &globals.runtime_dir {
        Some(path) => path.clone(),
        None => default_runtime_dir()?,
    };
    Ok(runtime_dir.join("store"))
}

fn open_artifact_store(globals: &Globals) -> Result<ContentAddressedStore, CliError> {
    ContentAddressedStore::open(artifact_store_root(globals)?, CasQuota::dayu200_default())
        .map_err(artifact_store_error)
}

fn open_existing_artifact_store(globals: &Globals) -> Result<ContentAddressedStore, CliError> {
    let root = artifact_store_root(globals)?;
    if !root.exists() {
        return Err(CliError::new(
            "ARTIFACT_NOT_FOUND",
            "The selected runtime has no artifact store.",
            5,
            false,
        ));
    }
    ContentAddressedStore::open(root, CasQuota::dayu200_default()).map_err(artifact_store_error)
}

fn list_artifacts(globals: &Globals) -> Result<Vec<(Sha256Digest, u64)>, CliError> {
    let root = artifact_store_root(globals)?;
    if !root.exists() {
        return Ok(Vec::new());
    }
    let store = ContentAddressedStore::open(root, CasQuota::dayu200_default())
        .map_err(artifact_store_error)?;
    let mut objects = Vec::new();
    for digest in store.list_objects().map_err(artifact_store_error)? {
        let size = store
            .open_object(&digest)
            .and_then(|file| file.metadata().map_err(CasError::from))
            .map_err(artifact_store_error)?
            .len();
        objects.push((digest, size));
    }
    Ok(objects)
}

fn load_profile_coverage(
    path: &Path,
    manifest: &arkforge_artifact::manifest::ArtifactManifest,
) -> Result<ProfileCoverage, CliError> {
    let source = std::fs::read_to_string(path).map_err(|error| {
        CliError::new(
            "PROFILE_FILE_NOT_FOUND",
            format!("Cannot read profile {}: {error}", path.display()),
            5,
            false,
        )
    })?;
    let profile = profile::load(&source).map_err(|error| {
        CliError::new(
            "PROFILE_REJECTED",
            format!("Profile {} is invalid: {error}", path.display()),
            3,
            false,
        )
    })?;
    profile_coverage(manifest, &profile)
        .map_err(|message| CliError::new("PROFILE_REJECTED", message, 3, false))
}

fn artifact_store_error(error: CasError) -> CliError {
    match error {
        CasError::NotFound(_) => CliError::new("ARTIFACT_NOT_FOUND", error.to_string(), 5, false),
        CasError::QuotaExceeded(_)
        | CasError::DigestMismatch { .. }
        | CasError::ArtifactTooLarge { .. } => {
            CliError::new("ARTIFACT_IMPORT_REFUSED", error.to_string(), 3, false)
        }
        CasError::LeaseHeld { .. } | CasError::InvalidHolder(_) => {
            CliError::new("ARTIFACT_STATE_CONFLICT", error.to_string(), 6, false)
        }
        CasError::Io(_) => CliError::new("ARTIFACT_STORE_FAILED", error.to_string(), 10, true),
    }
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
            print_devices(globals.output, &manager.list_devices()?);
            Ok(0)
        }
        "inspect" => {
            let result = manager.inspect(options.one("device")?)?;
            print_inspection(globals.output, &result);
            Ok(0)
        }
        "read" => {
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
    input_digest: Sha256Digest,
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
            println!("sha256   {input_digest}");
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
                vec!["arkforge help signing verify --format json".to_string()]
            };
            println!(
                "{{\"schema\":\"arkforge.signing-verification/v1\",\"input_sha256\":{},\"mode\":{},\"compliant\":{},\"facts\":{},\"violations\":[{}],\"contract\":{},\"next_commands\":{}}}",
                json(&input_digest.to_string()),
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
                "{{\"schema\":\"arkforge.device-list/v1\",\"observations\":[{values}],\"next_commands\":{}}}",
                json_array(&next)
            );
        }
    }
}

fn print_device_observation(output: Output, observation: &DeviceObservationView) {
    let next = format!(
        "arkforge device probe --device {} --profile <profile-id>",
        observation.observation_id
    );
    match output {
        Output::Human => {
            println!("device              {}", observation.observation_id);
            println!("observed_at_epoch_ms {}", observation.observed_at_epoch_ms);
            println!("mode                {}", observation.mode);
            println!("identity_strength   {}", observation.identity_strength);
            println!("topology_sha256     {}", observation.topology_sha256);
            println!("descriptor_sha256   {}", observation.descriptor_sha256);
            println!("malformed_descriptor {}", observation.malformed_descriptor);
            print_key_values_human("protocol identity", &observation.protocol_identity);
            println!("Next: {next}");
        }
        Output::Json => println!(
            "{{\"schema\":\"arkforge.device-observation/v1\",\"observation\":{},\"next_commands\":[{}]}}",
            observation_json(observation),
            json(&next)
        ),
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
            "{{\"schema\":\"arkforge.device-probe/v1\",\"observation\":{},\"profile_id\":{},\"facts_sha256\":{},\"protocol_facts\":{},\"next_commands\":[{}]}}",
            observation_json(&probe.observation),
            json(&probe.profile_id),
            json(&probe.facts_sha256),
            key_values_json(&probe.protocol_facts),
            json(&next)
        ),
    }
}

fn print_device_wait(
    output: Output,
    requested_profile: &str,
    requested_mode: &str,
    timeout_ms: u64,
    probe: &DeviceProbeView,
) {
    let next = format!(
        "arkforge flash assess --artifact <artifact-id> --profile {} --device {} --intent full-restore",
        probe.profile_id, probe.observation.observation_id
    );
    match output {
        Output::Human => {
            println!("Unique matching device observed.");
            println!("requested_profile {}", requested_profile);
            println!("requested_mode    {}", requested_mode);
            println!("timeout_ms        {timeout_ms}");
            println!("device            {}", probe.observation.observation_id);
            println!("facts_sha256      {}", probe.facts_sha256);
            println!("Next: {next}");
        }
        Output::Json => println!(
            "{{\"schema\":\"arkforge.device-wait/v1\",\"requested_profile\":{},\"requested_mode\":{},\"timeout_ms\":{},\"match\":{{\"observation\":{},\"profile_id\":{},\"facts_sha256\":{},\"protocol_facts\":{}}},\"next_commands\":[{}]}}",
            json(requested_profile),
            json(requested_mode),
            timeout_ms,
            observation_json(&probe.observation),
            json(&probe.profile_id),
            json(&probe.facts_sha256),
            key_values_json(&probe.protocol_facts),
            json(&next)
        ),
    }
}

fn print_artifact_import(output: Output, imported: &ImportedObject) {
    let artifact_id = imported.digest.to_hex();
    let next = format!("arkforge artifact inspect --artifact {artifact_id}");
    match output {
        Output::Human => {
            println!("Artifact imported into the content-addressed store.");
            println!("artifact_id   {artifact_id}");
            println!("sha256       {artifact_id}");
            println!("size_bytes   {}", imported.size_bytes);
            println!("deduplicated {}", imported.deduplicated);
            println!("No device was accessed or mutated.");
            println!("Next: {next}");
        }
        Output::Json => println!(
            "{{\"schema\":\"arkforge.artifact-import/v1\",\"artifact_id\":{},\"sha256\":{},\"size_bytes\":{},\"deduplicated\":{},\"host_store_mutated\":{},\"device_accessed\":false,\"next_commands\":[{}]}}",
            json(&artifact_id),
            json(&artifact_id),
            imported.size_bytes,
            imported.deduplicated,
            !imported.deduplicated,
            json(&next)
        ),
    }
}

fn print_artifact_list(output: Output, objects: &[(Sha256Digest, u64)]) {
    let next = if objects.is_empty() {
        vec!["arkforge artifact import --file <firmware-file>".to_string()]
    } else {
        vec!["arkforge artifact inspect --artifact <artifact-id>".to_string()]
    };
    match output {
        Output::Human => {
            if objects.is_empty() {
                println!("No artifacts are stored in this runtime.");
                println!("Next: {}", next[0]);
                return;
            }
            println!("Stored artifacts ({})", objects.len());
            for (digest, size) in objects {
                println!("{}  size_bytes={size}", digest.to_hex());
            }
            println!("Next: {}", next[0]);
        }
        Output::Json => {
            let artifacts = objects
                .iter()
                .map(|(digest, size)| {
                    format!(
                        "{{\"artifact_id\":{},\"sha256\":{},\"size_bytes\":{size}}}",
                        json(&digest.to_hex()),
                        json(&digest.to_hex())
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            println!(
                "{{\"schema\":\"arkforge.artifact-list/v1\",\"artifacts\":[{artifacts}],\"next_commands\":{}}}",
                json_array(&next)
            );
        }
    }
}

fn print_artifact_inspection(
    output: Output,
    artifact_id: &str,
    manifest: &InspectArtifactResponse,
    coverage: Option<&ProfileCoverage>,
) {
    let next = format!(
        "arkforge flash assess --artifact {artifact_id} --profile <profile-id> --device <observation-id> --intent full-restore"
    );
    match output {
        Output::Human => {
            print_artifact_human(artifact_id, manifest);
            if let Some(coverage) = coverage {
                print_profile_coverage_human(coverage);
            }
            println!("No device was accessed or mutated.");
            println!("Next: {next}");
        }
        Output::Json => println!(
            "{{\"schema\":\"arkforge.artifact-inspection/v1\",{},\"profile_coverage\":{},\"device_accessed\":false,\"next_commands\":[{}]}}",
            artifact_fields_json(artifact_id, manifest),
            coverage
                .map(profile_coverage_json)
                .unwrap_or_else(|| "null".into()),
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
            print_artifact_human(artifact_id, manifest);
            println!("Next: {next}");
        }
        Output::Json => println!(
            "{{\"schema\":\"arkforge.artifact/v1\",{},\"next_commands\":[{}]}}",
            artifact_fields_json(artifact_id, manifest),
            json(&next)
        ),
    }
}

fn print_artifact_human(artifact_id: &str, manifest: &InspectArtifactResponse) {
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
}

fn print_profile_coverage_human(coverage: &ProfileCoverage) {
    println!(
        "profile coverage  {} {} sha256={} complete={}",
        coverage.profile_id, coverage.profile_version, coverage.profile_sha256, coverage.complete
    );
    for target in &coverage.targets {
        println!(
            "  order={} partition={} source={} size_bytes={} present={}",
            target.write_order,
            target.partition,
            target.source_member.as_deref().unwrap_or("none"),
            target
                .source_size_bytes
                .map(|value| value.to_string())
                .unwrap_or_else(|| "null".into()),
            target.present
        );
    }
}

fn artifact_fields_json(artifact_id: &str, manifest: &InspectArtifactResponse) -> String {
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
    format!(
        "\"artifact_id\":{},\"format_id\":{},\"content_sha256\":{},\"manifest_sha256\":{},\"size_bytes\":{},\"confidence\":{},\"members\":[{}],\"partitions\":[{}],\"build_facts\":{},\"unclassified_members\":{},\"execution_relevant_unknowns\":{}",
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
        key_values_json(&manifest.execution_relevant_unknowns)
    )
}

fn profile_coverage_json(coverage: &ProfileCoverage) -> String {
    let targets = coverage
        .targets
        .iter()
        .map(|target| {
            format!(
                "{{\"write_order\":{},\"partition\":{},\"source_member\":{},\"source_size_bytes\":{},\"present\":{}}}",
                target.write_order,
                json(&target.partition),
                optional_json(target.source_member.as_deref()),
                optional_u64(target.source_size_bytes),
                target.present
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"profile_id\":{},\"profile_version\":{},\"profile_sha256\":{},\"complete\":{},\"targets\":[{}]}}",
        json(&coverage.profile_id),
        json(&coverage.profile_version),
        json(&coverage.profile_sha256),
        coverage.complete,
        targets
    )
}

fn print_flash_assessment(
    output: Output,
    artifact: &str,
    profile: &str,
    device: &str,
    intent: &str,
    assessment: &Assessment,
) {
    let mechanics_permits = execution_support_state_permits(&assessment.mechanics_maturity_state);
    let authority_permits = execution_support_state_permits(&assessment.authority_support_state);
    let executable =
        assessment.availability == "available" && mechanics_permits && authority_permits;
    let next = if executable {
        vec![format!(
            "arkforge flash plan --artifact {artifact} --profile {profile} --device {device} --intent {intent}"
        )]
    } else {
        vec!["arkforge daemon status".to_string()]
    };
    let mut blockers = Vec::new();
    let mut blocker_codes = Vec::new();
    if !mechanics_permits {
        blocker_codes.push("MECHANICS_MATURITY_UNAVAILABLE");
        blockers.push(format!(
            "{{\"code\":\"MECHANICS_MATURITY_UNAVAILABLE\",\"state\":{},\"key_sha256\":{},\"remediation\":\"Run only a named reviewed hardware campaign or wait for production mechanics support.\"}}",
            json(&assessment.mechanics_maturity_state),
            json(&assessment.mechanics_maturity_key_sha256),
        ));
    }
    if !authority_permits {
        blocker_codes.push("AUTHORITY_SUPPORT_UNAVAILABLE");
        blockers.push(format!(
            "{{\"code\":\"AUTHORITY_SUPPORT_UNAVAILABLE\",\"state\":{},\"key_sha256\":{},\"remediation\":\"Bind exact HDC and use a named acceptance campaign, or wait for exact-key production support.\"}}",
            json(&assessment.authority_support_state),
            json(&assessment.authority_support_key_sha256),
        ));
    }
    if blockers.is_empty() && !executable {
        blocker_codes.push("PLAN_PRECONDITION_UNAVAILABLE");
        blockers.push(format!(
            "{{\"code\":\"PLAN_PRECONDITION_UNAVAILABLE\",\"state\":{},\"key_sha256\":null,\"remediation\":\"Inspect unavailable_reason and repeat assessment only after the named precondition changes.\"}}",
            json(&assessment.availability),
        ));
    }
    match output {
        Output::Human => {
            println!("Flash assessment (executable: {executable})");
            println!("artifact      {artifact}");
            println!("profile       {profile}");
            println!("device        {device}");
            println!("intent        {intent}");
            println!("availability  {}", assessment.availability);
            println!(
                "mechanics      {} ({})",
                assessment.mechanics_maturity_state, assessment.mechanics_maturity_key_sha256
            );
            println!(
                "authority      {} ({})",
                assessment.authority_support_state, assessment.authority_support_key_sha256
            );
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
            for blocker in &blocker_codes {
                println!("blocker       {blocker}");
            }
            println!("Next: {}", next[0]);
        }
        Output::Json => println!(
            "{{\"schema\":\"arkforge.flash-assessment/v1\",\"executable\":{},\"artifact_id\":{},\"profile_id\":{},\"device_id\":{},\"intent\":{},\"availability\":{},\"unavailable_reason\":{},\"mechanics_maturity\":{{\"key_sha256\":{},\"state\":{}}},\"authority_support\":{{\"key_sha256\":{},\"state\":{}}},\"blockers\":[{}],\"would_be_steps\":{},\"known_persistent_effects\":{},\"data_impact\":{},\"unknowns\":{},\"evidence_requirements\":{},\"next_commands\":{}}}",
            executable,
            json(artifact),
            json(profile),
            json(device),
            json(intent),
            json(&assessment.availability),
            optional_json(
                (!assessment.unavailable_reason.is_empty())
                    .then_some(assessment.unavailable_reason.as_str())
            ),
            json(&assessment.mechanics_maturity_key_sha256),
            json(&assessment.mechanics_maturity_state),
            json(&assessment.authority_support_key_sha256),
            json(&assessment.authority_support_state),
            blockers.join(","),
            steps_json(&assessment.would_be_steps),
            effects_json(&assessment.known_persistent_effects),
            key_values_json(&assessment.data_impact),
            key_values_json(&assessment.unknowns),
            key_values_json(&assessment.evidence_requirements),
            json_array(&next)
        ),
    }
}

fn execution_support_state_permits(state: &str) -> bool {
    matches!(state, "productionVerified" | "hardwareCampaign")
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
                "{{\"schema\":\"arkforge.job-list/v1\",\"jobs\":[{values}],\"next_commands\":{}}}",
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
            "{{\"schema\":\"arkforge.job/v1\",\"job\":{},\"next_commands\":{}}}",
            job_json(job),
            json_array(&next)
        ),
    }
}

fn print_job_watch(
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
            "arkforge job watch --job {} --after-sequence {cursor}",
            summary.job_id
        )]
    };
    if globals.jsonl {
        for record in render_job_jsonl(
            command,
            after_sequence,
            timeout_ms,
            events,
            summary,
            timed_out,
        ) {
            println!("{record}");
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
            println!(
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

fn render_job_jsonl(
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
            "arkforge job watch --job {} --after-sequence {cursor}",
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
            "{{\"schema\":\"arkforge.recovery-guide/v1\",\"job_id\":{},\"original_state\":{},\"original_outcome_immutable\":{},\"automatic_replay_forbidden\":{},\"complete_overwrite_supported\":{},\"contract\":{},\"actions\":{},\"next_commands\":[]}}",
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
        "{{\"observation_id\":{},\"observed_at_epoch_ms\":{},\"mode\":{},\"topology_sha256\":{},\"descriptor_sha256\":{},\"serial_sha256\":{},\"serial_evidence_kind\":{},\"identity_strength\":{},\"malformed_descriptor\":{},\"protocol_identity\":{}}}",
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

fn job_event_json(event: &JobEvent) -> String {
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
        .map(|receipt| {
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
                optional_json((!receipt.typed_skip_reason.is_empty()).then_some(receipt.typed_skip_reason.as_str())),
                optional_json((!receipt.failure_classification.is_empty()).then_some(receipt.failure_classification.as_str())),
                key_values_json(&receipt.facts)
            )
        })
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
            if option == "detach" {
                values
                    .entry(option.to_string())
                    .or_default()
                    .push("true".into());
                index += 1;
                continue;
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

    fn optional_one(&self, name: &str) -> Result<Option<&str>, CliError> {
        match self.values.get(name).map(Vec::as_slice) {
            Some([value]) => Ok(Some(value)),
            Some(_) => Err(CliError::invalid(format!(
                "--{name} may be supplied only once."
            ))),
            None => Ok(None),
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

fn command_runtime_dir(globals: &Globals) -> Result<PathBuf, CliError> {
    globals
        .runtime_dir
        .clone()
        .map(Ok)
        .unwrap_or_else(default_runtime_dir)
}

fn reject_extra(arguments: &[String], command: &str) -> Result<(), CliError> {
    if arguments.is_empty() {
        Ok(())
    } else {
        Err(CliError::invalid(format!(
            "{command} accepts no command options; unexpected {:?}.",
            arguments[0]
        )))
    }
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
                "{{\"schema\":\"arkforge.rescue-device-list/v1\",\"devices\":[{values}],\"next_commands\":[\"arkforge rescue inspect --device <device-id>\"]}}"
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
                "{{\"schema\":\"arkforge.rescue-inspection/v1\",\"device\":{},\"capacity_sectors\":{},\"capacity_evidence_sha256\":{},\"layout_sha256\":{},\"layout_evidence_sha256\":{},\"profile_compatible\":{},\"profile_blocker\":{},\"partitions\":[{}],\"next_commands\":[\"arkforge help rescue plan --format json\"]}}",
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
            "{{\"schema\":\"arkforge.rescue-read-receipt/v1\",\"device_id\":{},\"start_sector\":{},\"sector_count\":{},\"bytes\":{},\"sha256\":{},\"output_written\":true,\"device_mutated\":false,\"next_commands\":[{}]}}",
            json(&result.device.device_id),
            result.begin_sector,
            result.sector_count,
            result.bytes,
            json(&result.sha256.to_string()),
            json(&format!(
                "arkforge rescue inspect --device {}",
                result.device.device_id
            ))
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
                "{{\"schema\":\"arkforge.rescue-plan-summary/v1\",\"plan_id\":{},\"plan_sha256\":{},\"device_id\":{},\"operation\":{},\"expires_at_epoch_ms\":{},\"required_acknowledgements\":{},\"device_mutated\":false,\"next_commands\":[{}]}}",
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
            "{{\"schema\":\"arkforge.rescue-receipt/v1\",\"receipt_id\":{},\"receipt_sha256\":{},\"plan_id\":{},\"plan_sha256\":{},\"device_id\":{},\"operation\":{},\"disposition\":{},\"evidence_sha256\":{},\"completed_at_epoch_ms\":{},\"detail\":{},\"payload_bytes\":{},\"payload_sha256\":{},\"replay_allowed\":false,\"next_commands\":{}}}",
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

fn print_error(output: Output, command: &[String], arguments: &[String], error: &CliError) {
    let fallback_next = if matches!(
        error.code.as_str(),
        "PLAN_DIGEST_MISMATCH" | "UNEXPECTED_ACKNOWLEDGEMENT"
    ) && command == ["rescue", "apply"]
    {
        Some("arkforge help rescue apply --format json".to_string())
    } else {
        remediation(&error.code).map(str::to_string)
    };
    let exact_retry = acknowledgement_retry_command(arguments, error);
    let next_commands = exact_retry
        .or(fallback_next)
        .into_iter()
        .collect::<Vec<_>>();
    let remediation_text = match error.code.as_str() {
        "ACKNOWLEDGEMENT_REQUIRED" => {
            "Review the sealed effects and supply every required acknowledgement exactly once."
        }
        "OUTCOME_UNKNOWN" => {
            "Do not retry automatically; reconcile the recorded job or device state."
        }
        "INVALID_ARGUMENT" => "Read the machine help for the exact command and option constraints.",
        _ if next_commands.is_empty() => {
            "Inspect the stable error code and preserve all durable evidence."
        }
        _ => "Follow one next_commands entry without changing effect-relevant identifiers.",
    };
    let structured_message = structured_error_message(error, arguments);
    match output {
        Output::Human => {
            eprintln!("arkforge: {}: {}", error.code, error.message);
            eprintln!("Remediation: {remediation_text}");
            if let Some(next) = next_commands.first() {
                eprintln!("Next: {next}");
            }
        }
        Output::Json => println!(
            "{{\"schema\":\"arkforge.command-result/v1\",\"ok\":false,\"command\":{},\"error\":{{\"code\":{},\"message\":{},\"remediation\":{},\"retryable\":{},\"required_acknowledgements\":{},\"next_commands\":{}}}}}",
            json_strings(command),
            json(&error.code),
            json(&structured_message),
            json(remediation_text),
            error.retryable,
            json_strings(&error.required_acknowledgements),
            json_strings(&next_commands),
        ),
    }
}

fn structured_error_message(error: &CliError, arguments: &[String]) -> String {
    let mut message = match error.code.as_str() {
        "DAEMON_UNAVAILABLE" => {
            "No CLI authority supervisor is listening in the selected runtime.".to_string()
        }
        "CONTROLLER_UNAVAILABLE" => {
            "The selected runtime has no available authority controller.".to_string()
        }
        "MECHANICS_DAEMON_UNAVAILABLE" | "MECHANICS_DAEMON_START_FAILED" => {
            "The signed sibling mechanics daemon is unavailable.".to_string()
        }
        code if code.ends_with("_IO_FAILED") || code == "ARTIFACT_STORE_FAILED" => {
            "The operation failed at a local I/O boundary; no device action was inferred from the failure."
                .to_string()
        }
        _ => error.message.clone(),
    };
    let mut index = 0;
    while index < arguments.len() {
        if matches!(
            arguments[index].as_str(),
            "--runtime-dir" | "--file" | "--profile-file" | "--image" | "--out" | "--hdc"
        ) {
            if let Some(value) = arguments.get(index + 1) {
                message = message.replace(value, "<redacted-path>");
            }
            index += 2;
        } else {
            index += 1;
        }
    }
    message
}

fn requested_command_path(arguments: &[String]) -> Vec<String> {
    let mut command = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--runtime-dir" | "--output" => index += 2,
            "--no-color" | "--quiet" | "--verbose" => index += 1,
            value if value.starts_with('-') => break,
            value => {
                command.push(value.to_string());
                index += 1;
            }
        }
    }
    let longest = (0..=command.len()).rev().find(|length| {
        let key = command[..*length].join(" ");
        HELP.iter().any(|spec| spec.command == key)
    });
    longest
        .map(|length| command[..length].to_vec())
        .unwrap_or(command)
}

fn acknowledgement_retry_command(arguments: &[String], error: &CliError) -> Option<String> {
    if error.code != "ACKNOWLEDGEMENT_REQUIRED" || error.required_acknowledgements.is_empty() {
        return None;
    }
    let mut command = vec!["arkforge".to_string()];
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--runtime-dir" | "--output" => index += 2,
            "--no-color" | "--quiet" | "--verbose" => index += 1,
            _ => {
                command.push(arguments[index].clone());
                index += 1;
            }
        }
    }
    for token in &error.required_acknowledgements {
        command.push("--ack".into());
        command.push(token.clone());
    }
    Some(
        command
            .iter()
            .map(|word| shell_word(word))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn shell_word(value: &str) -> String {
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"-._:/=@".contains(&byte))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn bracketed_values_after(message: &str, marker: &str) -> Vec<String> {
    message
        .split_once(marker)
        .and_then(|(_, rest)| rest.split_once(']'))
        .map(|(values, _)| {
            values
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn remediation(code: &str) -> Option<&'static str> {
    match code {
        "INVALID_ARGUMENT" => Some("arkforge help --format json"),
        "SIGNING_INPUT_REFUSED" => Some("arkforge help signing verify --format json"),
        "DAEMON_UNAVAILABLE" | "IPC_IO_FAILED" => Some("arkforge daemon start"),
        "PROTOCOL_REFUSED" | "IPC_RESPONSE_INVALID" | "IPC_RESPONSE_MISMATCH" => {
            Some("arkforge --version")
        }
        "ARTIFACT_NOT_FOUND" | "ARTIFACT_NOT_INSPECTED" => {
            Some("arkforge help artifact --format json")
        }
        "ARTIFACT_FILE_NOT_FOUND" => Some("arkforge help artifact import --format json"),
        "ARTIFACT_IMPORT_REFUSED" | "ARTIFACT_STORE_FAILED" => Some("arkforge artifact list"),
        "ARTIFACT_REJECTED" => Some("arkforge help artifact inspect --format json"),
        "PROFILE_FILE_NOT_FOUND" | "PROFILE_REJECTED" => {
            Some("arkforge help artifact inspect --format json")
        }
        "OBSERVATION_NOT_FOUND" => Some("arkforge device list"),
        "DEVICE_WAIT_TIMEOUT" | "AMBIGUOUS_DEVICE" => Some("arkforge device list"),
        "PROFILE_NOT_FOUND" | "NO_PROVIDER_FOR_PROFILE" => {
            Some("arkforge help device probe --format json")
        }
        "PLAN_UNAVAILABLE"
        | "RECOVERY_PLAN_UNAVAILABLE"
        | "AUTHORITY_SUPPORT_UNAVAILABLE"
        | "AUTHORITY_SUPPORT_SEAL_MISMATCH"
        | "MECHANICS_MATURITY_KEY_INVALID"
        | "HDC_BINDING_REFUSED"
        | "HDC_DIGEST_MISMATCH" => Some("arkforge daemon status"),
        "MECHANICS_RUNTIME_CHANGED" => Some("arkforge help flash plan --format json"),
        "PLAN_DIGEST_MISMATCH" | "UNEXPECTED_ACKNOWLEDGEMENT" => {
            Some("arkforge help flash apply --format json")
        }
        "UNKNOWN_JOB" => Some("arkforge job list"),
        "STALE_JOB_SEQUENCE" => Some("arkforge job show --job <job-id>"),
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

fn help_constraints(spec: &HelpSpec) -> Vec<String> {
    let mut constraints = Vec::new();
    for option in spec.typed_options() {
        for required in option.requires {
            constraints.push(format!(
                "{{\"kind\":\"requires\",\"if\":{},\"then\":{}}}",
                json(&format!("--{}", option.name)),
                json(&format!("--{required}"))
            ));
        }
        for conflict in option.conflicts {
            constraints.push(format!(
                "{{\"kind\":\"conflicts\",\"left\":{},\"right\":{}}}",
                json(&format!("--{}", option.name)),
                json(&format!("--{conflict}"))
            ));
        }
    }
    if matches!(spec.command, "daemon run" | "daemon start") {
        constraints.push(
            "{\"kind\":\"allOrNone\",\"options\":[\"--hdc\",\"--expect-hdc-sha256\"]}".into(),
        );
        constraints.push(
            "{\"kind\":\"campaignEvidenceOnly\",\"option\":\"--hardware-campaign\",\"productionSupport\":false}"
                .into(),
        );
    }
    if spec.command == "rescue plan" {
        constraints.push(
            "{\"kind\":\"oneOf\",\"branches\":[{\"when\":{\"--operation\":\"write-partition\"},\"required\":[\"--partition\",\"--image\",\"--expect-image-sha256\"]},{\"when\":{\"--operation\":\"reset-device\"},\"forbidden\":[\"--partition\",\"--image\",\"--expect-image-sha256\"]}]}"
                .into(),
        );
    }
    if matches!(spec.command, "flash apply" | "rescue apply") {
        constraints.push(
            "{\"kind\":\"exactAcknowledgementSet\",\"plan\":\"--plan\",\"digest\":\"--expect-plan-sha256\",\"tokens\":\"--ack\"}"
                .into(),
        );
    }
    constraints
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
                .typed_options()
                .into_iter()
                .map(|option| {
                    format!(
                        "{{\"name\":{},\"type\":{},\"required\":{},\"repeatable\":{},\"enum_values\":{},\"sensitive\":{},\"effect_relevant\":{},\"requires\":{},\"conflicts\":{},\"description\":{}}}",
                        json(&format!("--{}", option.name)),
                        json(&option.value_type),
                        option.required,
                        option.repeatable,
                        json_strings(&option.enum_values),
                        option.sensitive,
                        option.effect_relevant,
                        json_strings(&option.requires),
                        json_strings(&option.conflicts),
                        json(option.description)
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
            let constraints = help_constraints(spec).join(",");
            println!(
                "{{\"schema\":{},\"path\":{},\"command\":{},\"summary\":{},\"usage\":{},\"effect\":{},\"effect_detail\":{},\"interactive\":false,\"availability\":{{\"platforms\":[\"macos\"],\"requires_daemon\":{},\"requires_controller\":{}}},\"subcommands\":[{}],\"requires\":{},\"outputs\":{},\"output_descriptions\":{},\"options\":[{}],\"constraints\":[{}],\"examples\":{},\"next_commands\":{},\"exit_codes\":[{}]}}",
                json(HELP_SCHEMA),
                json_array(&spec.path()),
                json(spec.command),
                json(spec.summary),
                json(spec.usage),
                json(spec.effect_class()),
                json(spec.effect),
                spec.requires_daemon(),
                spec.requires_controller(),
                subcommands,
                json_array(spec.requires),
                json_strings(&spec.output_schemas()),
                json_array(spec.produces),
                options,
                constraints,
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

fn json_strings(values: &[String]) -> String {
    json_array(values)
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
        summary: "ArkForge plans, executes, verifies, and recovers device firmware operations.",
        usage: "arkforge [global options] <command> [<subcommand>] [options]",
        effect: "The root command only describes capabilities. It does not access or mutate a device.",
        requires: &[],
        produces: &["Human help or arkforge.command-help/v1 JSON."],
        options: &[
            ("--runtime-dir <dir>", "Per-user ArkForge state directory."),
            (
                "--output <human|json|jsonl>",
                "Stable presentation format; default: human.",
            ),
            (
                "--no-color",
                "Disable color; accepted for deterministic scripts.",
            ),
            ("--quiet", "Print only the final human result."),
            (
                "--verbose",
                "Include diagnostic evidence; never include secrets.",
            ),
            ("-h, --help", "Show help for the current command."),
            ("-V, --version", "Print this build's ArkForge version."),
        ],
        examples: &[
            "arkforge help --format json",
            "arkforge help flash assess --format json",
        ],
        next: &["arkforge doctor"],
        exits: &[
            (0, "Help or version produced."),
            (2, "Command or option is invalid."),
        ],
    },
    HelpSpec {
        command: "doctor",
        summary: "Check whether this host can inspect or execute.",
        usage: "arkforge doctor",
        effect: "Read-only host and runtime assessment. It never starts services, opens a device for mutation, or changes local state.",
        requires: &[],
        produces: &[
            "arkforge.doctor/v1 with inspect readiness, execution readiness, blockers, and an exact next command.",
        ],
        options: &[],
        examples: &["arkforge --output json doctor"],
        next: &["arkforge daemon start"],
        exits: &[
            (0, "Assessment produced, including a not-ready result."),
            (2, "A global option is invalid."),
            (10, "The host itself could not be assessed."),
        ],
    },
    HelpSpec {
        command: "device",
        summary: "Discover, show, probe, and wait for exact device observations.",
        usage: "arkforge device <list|show|probe|wait> [options]",
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
        command: "device show",
        summary: "Show complete identity evidence for one exact current observation.",
        usage: "arkforge device show --device <observation-id>",
        effect: "Read-only discovery followed by exact-id selection. It does not persist a target binding or mutate a device.",
        requires: &["One exact observation id returned by the current runtime."],
        produces: &[
            "arkforge.device-observation/v1 with mode, time, topology/descriptor digests, identity strength, and protocol identity.",
        ],
        options: &[(
            "--device <observation-id>",
            "Exact current observation; required.",
        )],
        examples: &["arkforge --output json device show --device OBS-PREFLIGHT"],
        next: &["arkforge device probe --device <observation-id> --profile <profile-id>"],
        exits: &[
            (0, "Exact observation produced."),
            (2, "The observation id is missing."),
            (5, "The runtime or observation was not found."),
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
            "arkforge --output json device probe --device OBS-PREFLIGHT --profile org.openharmony.dayu200@1.0.0",
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
        command: "device wait",
        summary: "Wait for exactly one observation matching an explicit profile and mode.",
        usage: "arkforge device wait --profile <profile-id> --mode <mode> [--timeout-ms <u64>]",
        effect: "Repeated read-only discovery and probing. It never chooses the first match; multiple matches are a typed ambiguity refusal.",
        requires: &["A loaded profile id and an explicit expected mode."],
        produces: &["arkforge.device-wait/v1 with the unique probed observation and facts digest."],
        options: &[
            (
                "--profile <profile-id>",
                "Explicit loaded profile; required.",
            ),
            ("--mode <mode>", "Exact declared device mode; required."),
            (
                "--timeout-ms <u64>",
                "Bounded wait; optional, default 30000.",
            ),
        ],
        examples: &[
            "arkforge --output json device wait --profile org.openharmony.dayu200@1.0.0 --mode loader --timeout-ms 30000",
        ],
        next: &[
            "arkforge flash assess --artifact <artifact-id> --profile <profile-id> --device <observation-id> --intent full-restore",
        ],
        exits: &[
            (0, "Exactly one matching probed observation was produced."),
            (2, "Profile, mode, or timeout is invalid."),
            (3, "A matching provider probe was refused."),
            (
                5,
                "The runtime was unavailable or the bounded wait expired.",
            ),
            (
                6,
                "More than one observation matched; no target was selected.",
            ),
            (10, "Discovery, probe, or IPC failed."),
        ],
    },
    HelpSpec {
        command: "artifact",
        summary: "Import, inspect, list, and show content-addressed firmware artifacts.",
        usage: "arkforge artifact <import|inspect|list|show> [options]",
        effect: "Import writes only the local content-addressed store. Inspect/list are offline; show queries the public runtime. No artifact command mutates a device.",
        requires: &["An explicit runtime directory or the per-user default."],
        produces: &["Artifact IDs, stored-object lists, or complete inspected manifests."],
        options: &[],
        examples: &[
            "arkforge artifact import --file <firmware-file>",
            "arkforge help artifact inspect --format json",
        ],
        next: &["arkforge artifact import --file <firmware-file>"],
        exits: &[
            (0, "Artifact query completed."),
            (2, "Command or option is invalid."),
            (3, "Artifact inspection was refused."),
            (5, "The runtime or artifact was not found."),
            (10, "Inspection or IPC failed."),
        ],
    },
    HelpSpec {
        command: "artifact import",
        summary: "Hash and atomically import one firmware file into the runtime store.",
        usage: "arkforge artifact import --file <firmware-file> [--expect-sha256 <sha256>]",
        effect: "Host write only. It creates or deduplicates one content-addressed object after quota, size, and optional digest checks; no daemon or device is accessed.",
        requires: &[
            "One regular input file.",
            "Enough store quota and volume reserve for the complete input.",
        ],
        produces: &[
            "arkforge.artifact-import/v1 with artifact_id, SHA-256, size, deduplication status, and the exact inspect command.",
        ],
        options: &[
            (
                "--file <firmware-file>",
                "Regular firmware container file; required.",
            ),
            (
                "--expect-sha256 <sha256>",
                "Independent expected lowercase SHA-256; optional.",
            ),
        ],
        examples: &["arkforge --output json artifact import --file ./firmware.tar.gz"],
        next: &["arkforge artifact inspect --artifact <returned-artifact-id>"],
        exits: &[
            (0, "Artifact imported or deduplicated and synced."),
            (2, "Input options or digest syntax are invalid."),
            (
                3,
                "Digest, size, quota, or volume precondition refused import.",
            ),
            (5, "The input file was not found."),
            (10, "The durable content store failed."),
        ],
    },
    HelpSpec {
        command: "artifact inspect",
        summary: "Inspect one stored artifact offline and optionally compare profile target coverage.",
        usage: "arkforge artifact inspect --artifact <artifact-id> [--profile-file <file>]",
        effect: "Read-only artifact parsing after opening bytes by content digest. It never reparses a caller path and never accesses a device.",
        requires: &["One exact artifact id already present in this runtime store."],
        produces: &[
            "arkforge.artifact-inspection/v1 with the complete manifest and optional ordered profile target coverage.",
        ],
        options: &[
            (
                "--artifact <artifact-id>",
                "Exact stored content SHA-256; required.",
            ),
            (
                "--profile-file <file>",
                "Optional DeviceProfile used only for coverage comparison.",
            ),
        ],
        examples: &[
            "arkforge --output json artifact inspect --artifact <64-lowercase-hex> --profile-file profiles/dayu200.yaml",
        ],
        next: &[
            "arkforge flash assess --artifact <artifact-id> --profile <profile-id> --device <observation-id> --intent full-restore",
        ],
        exits: &[
            (0, "Manifest and optional coverage produced."),
            (2, "The artifact id or options are invalid."),
            (3, "Artifact or profile parsing was refused."),
            (
                5,
                "The artifact store, object, or profile file was not found.",
            ),
            (10, "The content store failed."),
        ],
    },
    HelpSpec {
        command: "artifact list",
        summary: "List every content-addressed object stored in this runtime.",
        usage: "arkforge artifact list",
        effect: "Read-only when a store exists. An absent store is reported as an empty list and is not created.",
        requires: &[],
        produces: &["arkforge.artifact-list/v1 with artifact ids and byte sizes."],
        options: &[],
        examples: &["arkforge --output json artifact list"],
        next: &["arkforge artifact inspect --artifact <artifact-id>"],
        exits: &[
            (0, "Artifact list produced, including an empty list."),
            (10, "The content store could not be read."),
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
        summary: "Assess and seal normal firmware work against exact resources.",
        usage: "arkforge flash <assess|plan|apply> [options]",
        effect: "Assessment is read-only; plan stores a sealed host object; apply is the only destructive normal-flash command.",
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
        effect: "Read-only assessment through the paired runtime. The result is structurally non-executable; no binding or plan is stored and no device is mutated.",
        requires: &[
            "An inspected artifact id.",
            "A loaded profile id.",
            "An exact current device observation id.",
            "The explicit intent full-restore.",
        ],
        produces: &[
            "arkforge.flash-assessment/v1 with executable, exact mechanics/authority keys, projected steps, effects, data impact, blockers, and evidence requirements.",
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
                "--intent <full-restore>",
                "Only supported semantic intent; required.",
            ),
        ],
        examples: &[
            "arkforge --output json flash assess --artifact <artifact-id> --profile org.openharmony.dayu200@1.0.0 --device OBS-PREFLIGHT --intent full-restore",
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
        command: "flash plan",
        summary: "Seal one executable normal-flash plan for exact artifact, profile, and device facts.",
        usage: "arkforge flash plan --artifact <artifact-id> --profile <id@version> --device <observation-id> --intent full-restore",
        effect: "Reads the exact device through the paired runtime and stores a sealed plan. It does not mutate the device.",
        requires: &[
            "A running paired CLI authority supervisor.",
            "Exact mechanics maturity and independent authority support.",
            "An imported artifact, canonical profile id@version, and exact current observation.",
        ],
        produces: &[
            "arkforge.flash-plan/v1 with plan digest, ordered steps, sealed effects, expiry, and exact acknowledgement tokens.",
        ],
        options: &[
            (
                "--artifact <artifact-id>",
                "Exact imported content id; required.",
            ),
            (
                "--profile <id@version>",
                "Exact loaded profile identity; required.",
            ),
            (
                "--device <observation-id>",
                "Exact current observation; required.",
            ),
            (
                "--intent <full-restore>",
                "Only supported semantic intent; required.",
            ),
        ],
        examples: &[
            "arkforge --output json flash plan --artifact <artifact-id> --profile org.openharmony.dayu200@1.0.0 --device OBS-PREFLIGHT --intent full-restore",
        ],
        next: &[
            "Use the returned next_commands entry verbatim after reviewing required_acknowledgements.",
        ],
        exits: &[
            (0, "Executable plan sealed; the device was not mutated."),
            (2, "Inputs are invalid."),
            (3, "Mechanics or authority support is unavailable."),
            (5, "A named resource or runtime is unavailable."),
            (6, "Target binding conflicts with durable state."),
            (10, "Controller, store, or supervisor failed."),
        ],
    },
    HelpSpec {
        command: "flash apply",
        summary: "Apply one sealed normal-flash plan under persistent per-step authority.",
        usage: "arkforge flash apply --plan <plan-id> --expect-plan-sha256 <sha256> --ack <token> [--ack <token>...] [--detach]",
        effect: "Destructive. Starts only the exact sealed plan after digest and acknowledgement equality; the supervisor mints one durable single-use permit per admitted step.",
        requires: &[
            "A live paired authority supervisor and fresh exact target continuity.",
            "The exact plan digest and exactly every returned acknowledgement token, with no extras.",
        ],
        produces: &[
            "arkforge.job-event/v1 and arkforge.command-result/v1 with a durable job id, ordered events, and terminal classification; --detach returns after durable job creation while authority continues.",
        ],
        options: &[
            (
                "--plan <plan-id>",
                "Exact stored normal-flash plan; required.",
            ),
            (
                "--expect-plan-sha256 <sha256>",
                "Caller expectation for sealed plan bytes; required.",
            ),
            (
                "--ack <token>",
                "Exact required effect token; repeat exactly as returned.",
            ),
            (
                "--detach",
                "Return after job creation; does not cancel or transfer authority.",
            ),
        ],
        examples: &[
            "arkforge flash apply --plan PLAN-EXAMPLE --expect-plan-sha256 <64-lowercase-hex> --ack data-loss:userdata",
        ],
        next: &["arkforge job watch --job <job-id>"],
        exits: &[
            (0, "Detached job created or watched job succeeded."),
            (2, "Inputs are invalid."),
            (
                3,
                "Plan, target, authority, or freshness precondition refused.",
            ),
            (4, "Plan digest or acknowledgement set is not exact."),
            (5, "Runtime or plan was not found."),
            (7, "Operation ended with a known non-success outcome."),
            (8, "Outcome is unknown; never retry automatically."),
            (9, "Watching ended without a terminal result."),
            (10, "Controller, supervisor, or journal failed."),
        ],
    },
    HelpSpec {
        command: "job",
        summary: "Observe, cancel, reconcile, and recover durable jobs.",
        usage: "arkforge job <list|show|watch|cancel|reconcile|recovery> [options]",
        effect: "Observation and reconciliation are read-only; cancel is explicit optimistic control; recovery creates a distinct superseding plan and never replays the original job.",
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
        command: "job watch",
        summary: "Read ordered job events after a resume sequence until terminal state or timeout.",
        usage: "arkforge job watch --job <job-id> [--after-sequence <u64>] [--timeout-ms <u64>]",
        effect: "Read-only polling of durable events and point-in-time status. Timeout ends only this observation; it never cancels or changes the job.",
        requires: &[
            "One exact job id and a resume sequence no greater than the durable last_sequence.",
        ],
        produces: &[
            "arkforge.job-watch/v1, arkforge.job-event/v1, and arkforge.command-result/v1 with strictly ordered typed events, terminal/timed-out status, and an exact resume command.",
        ],
        options: &[
            ("--job <job-id>", "Exact durable job id; required."),
            (
                "--after-sequence <u64>",
                "Return events strictly after this cursor; optional, default 0.",
            ),
            (
                "--timeout-ms <u64>",
                "Bounded observation; optional, default 30000.",
            ),
        ],
        examples: &[
            "arkforge --output json job watch --job <job-id> --after-sequence 0 --timeout-ms 30000",
        ],
        next: &[
            "If non-terminal, repeat next_commands[0]; stopping the watch never cancels the job.",
        ],
        exits: &[
            (0, "Terminal state or bounded observation result produced."),
            (2, "Job id, sequence, or timeout is invalid."),
            (5, "The runtime or job was not found."),
            (6, "The supplied resume sequence is ahead of durable state."),
            (10, "Event decoding or IPC failed."),
        ],
    },
    HelpSpec {
        command: "job cancel",
        summary: "Request cancellation against one exact observed journal sequence.",
        usage: "arkforge job cancel --job <job-id> --expect-sequence <u64>",
        effect: "Mutating job control. It cancels only at the daemon's declared safe boundary and never kills an in-flight device action.",
        requires: &[
            "A live paired supervisor and the exact last_sequence returned by job show/watch.",
        ],
        produces: &[
            "arkforge.job-cancellation/v1 with cancelled-safe, queued-at-safe-boundary, already-terminal, or outcome-unknown disposition.",
        ],
        options: &[
            ("--job <job-id>", "Exact durable job; required."),
            (
                "--expect-sequence <u64>",
                "Optimistic journal sequence; required.",
            ),
        ],
        examples: &["arkforge --output json job cancel --job JOB-EXAMPLE --expect-sequence 4"],
        next: &["arkforge job watch --job <job-id> --after-sequence <u64>"],
        exits: &[
            (0, "Cancellation reached a confirmed safe terminal state."),
            (2, "Inputs are invalid."),
            (5, "Runtime or job was not found."),
            (6, "Expected sequence is stale."),
            (8, "Outcome is unknown."),
            (9, "Cancellation is queued at a safe boundary."),
            (10, "Controller or supervisor failed."),
        ],
    },
    HelpSpec {
        command: "job reconcile",
        summary: "Assess possible effects without replaying an unresolved job.",
        usage: "arkforge job reconcile --job <job-id>",
        effect: "Read-only controller assessment. It never dispatches, resumes, edits, or reclassifies the original job optimistically.",
        requires: &["A durable job in the selected paired runtime."],
        produces: &[
            "arkforge.job-reconciliation/v1 with immutable original state, possible-effect completeness, effects, and verdict.",
        ],
        options: &[("--job <job-id>", "Exact durable job; required.")],
        examples: &["arkforge --output json job reconcile --job JOB-EXAMPLE"],
        next: &["arkforge job recovery guide --job <job-id>"],
        exits: &[
            (0, "No unresolved outcome remains."),
            (5, "Runtime or job was not found."),
            (
                8,
                "Original outcome remains unknown; never retry automatically.",
            ),
            (10, "Controller or supervisor failed."),
        ],
    },
    HelpSpec {
        command: "job recovery",
        summary: "Guide recovery or seal a distinct complete-overwrite superseding plan.",
        usage: "arkforge job recovery <guide|plan> [options]",
        effect: "Guide is read-only; plan may store a new sealed host object. Neither edits, resumes, or replays the original job.",
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
        command: "job recovery plan",
        summary: "Seal a distinct superseding plan when the recovery contract covers every possible effect.",
        usage: "arkforge job recovery plan --job <job-id> --artifact <artifact-id> --profile <id@version> --device <observation-id>",
        effect: "Reads recovery eligibility and exact target facts, then stores a new sealed plan. It does not mutate the device or original job.",
        requires: &[
            "An outcome-unknown original job with bounded effects and a complete-overwrite recovery contract.",
            "Fresh explicit artifact, profile, and device binding.",
        ],
        produces: &[
            "arkforge.flash-plan/v1 for a new normal-flash plan requiring recovery:supersedes-job=<job-id> in addition to its effect acknowledgements.",
        ],
        options: &[
            ("--job <job-id>", "Immutable original outcome; required."),
            (
                "--artifact <artifact-id>",
                "Exact imported artifact; required.",
            ),
            ("--profile <id@version>", "Exact loaded profile; required."),
            (
                "--device <observation-id>",
                "Fresh exact target observation; required.",
            ),
        ],
        examples: &[
            "arkforge --output json job recovery plan --job JOB-EXAMPLE --artifact <artifact-id> --profile org.openharmony.dayu200@1.0.0 --device OBS-PREFLIGHT",
        ],
        next: &[
            "Use the returned flash apply command only after reviewing the new plan and superseding token.",
        ],
        exits: &[
            (0, "Distinct superseding plan sealed."),
            (2, "Inputs are invalid."),
            (
                3,
                "Recovery eligibility or execution support is unavailable.",
            ),
            (5, "Runtime or named resource was not found."),
            (10, "Controller, supervisor, or journal failed."),
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
        produces: &[
            "arkforge.rescue-device-list/v1 with opaque rescue device IDs; raw serial values are not printed.",
        ],
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
            "arkforge.rescue-inspection/v1 with capacity, layout digest, partition extents, evidence digests, and profile compatibility.",
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
        produces: &[
            "arkforge.rescue-read/v1 plus a new file, byte count, and SHA-256 read receipt.",
        ],
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
            "arkforge.rescue-plan/v1 with plan_id, plan_sha256, expiry, sealed effects, and the exact acknowledgement set required by apply.",
        ],
        options: &[
            (
                "--device <device-id>",
                "Exact current Loader observation; required.",
            ),
            (
                "--operation <write-partition|reset-device>",
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
            "arkforge.rescue-receipt/v1 with a separate RescueReceipt and semantic-success, confirmed-no-effect, or outcome-unknown disposition.",
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
            (4, "Plan digest or acknowledgement set is not exact."),
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
        command: "daemon",
        summary: "Run and manage one paired local ArkForge runtime.",
        usage: "arkforge daemon <run|start|stop|status> [options]",
        effect: "Service lifecycle. The supervisor owns pairing authority; lifecycle commands do not flash a device.",
        requires: &["arkforged installed beside the canonical arkforge executable."],
        produces: &[
            "Typed two-process runtime status with protocol, authority epoch, readiness, active jobs, and blockers.",
        ],
        options: &[],
        examples: &["arkforge daemon start", "arkforge daemon status"],
        next: &["arkforge device list"],
        exits: &[
            (0, "Requested lifecycle operation completed."),
            (3, "An exact executable or signing binding was refused."),
            (5, "The runtime or mechanics daemon is unavailable."),
            (6, "The runtime is already running or has active jobs."),
            (10, "Supervisor or local IPC failed."),
        ],
    },
    HelpSpec {
        command: "daemon run",
        summary: "Run the CLI authority supervisor and mechanics daemon in the foreground.",
        usage: "arkforge daemon run [--profile-file <file>]... [--hdc <absolute-path> --expect-hdc-sha256 <sha256>] [--hardware-campaign <campaign-id>] [--require-release-signing]",
        effect: "Service lifecycle. Creates owner-only local sockets and keeps both processes supervised until daemon stop is requested.",
        requires: &[
            "A runtime directory not owned by another live supervisor.",
            "HDC path and digest together when managed normal-mode control is required.",
        ],
        produces: &[
            "arkforge.daemon-status/v1 for a paired foreground runtime; the pairing secret exists only in supervisor/daemon memory and crosses an inherited stdin pipe.",
        ],
        options: &[
            (
                "--profile-file <file>",
                "Additional explicit profile; repeatable.",
            ),
            (
                "--hdc <absolute-path>",
                "Exact managed-control executable; requires its expected digest.",
            ),
            (
                "--expect-hdc-sha256 <sha256>",
                "Caller expectation for HDC bytes; requires --hdc.",
            ),
            (
                "--require-release-signing",
                "Refuse unless arkforged satisfies the release signing contract.",
            ),
            (
                "--hardware-campaign <campaign-id>",
                "Explicit mechanics and CLI-authority acceptance campaign; receipts remain campaign evidence and never publish production support.",
            ),
        ],
        examples: &["arkforge --runtime-dir /tmp/arkforge daemon run"],
        next: &["arkforge daemon status"],
        exits: &[
            (0, "Runtime stopped cleanly."),
            (2, "Options are invalid."),
            (3, "A tool or signing binding was refused."),
            (6, "The runtime is already running."),
            (10, "A supervised process or local IPC failed."),
        ],
    },
    HelpSpec {
        command: "daemon start",
        summary: "Start the same paired runtime under a background supervisor.",
        usage: "arkforge daemon start [--profile-file <file>]... [--hdc <absolute-path> --expect-hdc-sha256 <sha256>] [--hardware-campaign <campaign-id>] [--require-release-signing]",
        effect: "Service lifecycle. Starts a background supervisor and mechanics daemon; it does not access or mutate a device.",
        requires: &["The same exact bindings as daemon run."],
        produces: &["arkforge.daemon-status/v1 after the public protocol handshake succeeds."],
        options: &[
            (
                "--profile-file <file>",
                "Additional explicit profile; repeatable.",
            ),
            (
                "--hdc <absolute-path>",
                "Exact managed-control executable; requires its expected digest.",
            ),
            (
                "--expect-hdc-sha256 <sha256>",
                "Caller expectation for HDC bytes; requires --hdc.",
            ),
            (
                "--require-release-signing",
                "Require the daemon release signing contract.",
            ),
            (
                "--hardware-campaign <campaign-id>",
                "Explicit mechanics and CLI-authority acceptance campaign; receipts remain campaign evidence and never publish production support.",
            ),
        ],
        examples: &["arkforge --output json daemon start"],
        next: &["arkforge device list"],
        exits: &[
            (0, "Runtime is ready for commands."),
            (2, "Options are invalid."),
            (3, "A tool or signing binding was refused."),
            (5, "The mechanics daemon could not start."),
            (6, "The runtime is already running."),
            (10, "Supervisor startup failed."),
        ],
    },
    HelpSpec {
        command: "daemon status",
        summary: "Show protocol, process, authority, readiness, job, and blocker state.",
        usage: "arkforge daemon status",
        effect: "Read-only local runtime observation.",
        requires: &["A live CLI authority supervisor."],
        produces: &["arkforge.daemon-status/v1."],
        options: &[],
        examples: &["arkforge --output json daemon status"],
        next: &["arkforge device list"],
        exits: &[
            (0, "Runtime status produced."),
            (5, "No runtime is listening."),
            (10, "Status IPC failed."),
        ],
    },
    HelpSpec {
        command: "daemon stop",
        summary: "Stop an idle paired runtime without a force path.",
        usage: "arkforge daemon stop",
        effect: "Service lifecycle. Stops both processes only when no durable job is active; it never cancels a job implicitly.",
        requires: &["A live runtime with zero active jobs."],
        produces: &["arkforge.daemon-stop/v1."],
        options: &[],
        examples: &["arkforge --output json daemon stop"],
        next: &["arkforge daemon start"],
        exits: &[
            (0, "Idle runtime stopped."),
            (5, "No runtime is listening."),
            (6, "Active jobs prevent stopping."),
            (10, "Stop IPC failed."),
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
    HelpSpec {
        command: "completion",
        summary: "Generate shell completion from the canonical command tree.",
        usage: "arkforge completion --shell <bash|zsh|fish>",
        effect: "Read-only and offline. Completion text is generated from the same typed command definitions used by parsing and help.",
        requires: &["An explicit supported shell."],
        produces: &[
            "Shell completion source on stdout, or arkforge.completion/v1 for structured output.",
        ],
        options: &[("--shell <bash|zsh|fish>", "Target shell; required.")],
        examples: &["arkforge completion --shell zsh"],
        next: &[
            "Evaluate or install the generated source using the selected shell's normal mechanism.",
        ],
        exits: &[
            (0, "Completion source produced."),
            (2, "The shell or option is invalid."),
        ],
    },
    HelpSpec {
        command: "help",
        summary: "Describe commands for humans or Agents without runtime access.",
        usage: "arkforge help [<command> [<subcommand>...]] [--format <human|json>]",
        effect: "Read-only and offline. Help is generated from the canonical typed command tree.",
        requires: &[],
        produces: &["Human command guidance or arkforge.command-help/v1."],
        options: &[(
            "--format <human|json>",
            "Help presentation format; default follows --output.",
        )],
        examples: &[
            "arkforge help --format json",
            "arkforge help flash apply --format json",
        ],
        next: &["Follow one returned next_commands entry with exact identifiers."],
        exits: &[
            (0, "Help produced."),
            (2, "The topic or format is invalid."),
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_status_next_commands_do_not_loop_on_authority_blockers() {
        let mut status = supervisor::DaemonStatus {
            supervisor_pid: 1,
            daemon_pid: 2,
            epoch: 3,
            protocol_major: 1,
            protocol_minor: 0,
            daemon_version: "0.1.0".into(),
            mechanics_ready: true,
            authority_support_available: false,
            hdc_bound: false,
            hdc_sha256: String::new(),
            hardware_campaign: String::new(),
            active_jobs: 0,
            blockers: vec![
                "AUTHORITY_HDC_UNBOUND".into(),
                "AUTHORITY_SUPPORT_UNPUBLISHED".into(),
            ],
        };
        assert_eq!(
            daemon_next_commands(&status),
            vec![
                "arkforge daemon stop",
                "arkforge help daemon start --format json"
            ]
        );

        status.active_jobs = 1;
        assert_eq!(daemon_next_commands(&status), vec!["arkforge job list"]);

        status.active_jobs = 0;
        status.hdc_bound = true;
        status.authority_support_available = true;
        status.blockers.clear();
        assert_eq!(daemon_next_commands(&status), vec!["arkforge device list"]);
    }

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
        assert!(
            validate_against_command_tree(&strings(&[
                "rescue", "inspect", "--device", "a", "--device", "b"
            ]))
            .is_err()
        );
        assert!(
            validate_against_command_tree(&strings(&["rescue", "list", "--backend", "external"]))
                .is_err()
        );
    }

    #[test]
    fn help_tree_has_every_implemented_leaf_and_agent_fields() {
        for topic in [
            "doctor",
            "device list",
            "device show",
            "device probe",
            "device wait",
            "artifact import",
            "artifact inspect",
            "artifact list",
            "artifact show",
            "flash assess",
            "flash plan",
            "flash apply",
            "job list",
            "job show",
            "job watch",
            "job cancel",
            "job reconcile",
            "job recovery guide",
            "job recovery plan",
            "rescue list",
            "rescue inspect",
            "rescue read",
            "rescue plan",
            "rescue apply",
            "daemon run",
            "daemon start",
            "daemon status",
            "daemon stop",
            "signing verify",
            "completion",
            "help",
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
            vec![
                "doctor",
                "device",
                "artifact",
                "flash",
                "job",
                "rescue",
                "daemon",
                "signing",
                "completion",
                "help"
            ]
        );
        assert_eq!(
            child_specs("job")
                .iter()
                .map(|spec| spec.command)
                .collect::<Vec<_>>(),
            vec![
                "job list",
                "job show",
                "job watch",
                "job cancel",
                "job reconcile",
                "job recovery"
            ]
        );
        assert_eq!(
            child_specs("job recovery")
                .iter()
                .map(|spec| spec.command)
                .collect::<Vec<_>>(),
            vec!["job recovery guide", "job recovery plan"]
        );
        assert_eq!(child_specs("signing")[0].command, "signing verify");
        assert_eq!(HELP_SCHEMA, "arkforge.command-help/v1");
    }

    #[test]
    fn every_example_parses_from_the_same_typed_tree_without_io() {
        for spec in HELP {
            for example in spec.examples {
                let words = example
                    .split_whitespace()
                    .map(example_fixture)
                    .collect::<Vec<_>>();
                assert_eq!(words.first().map(String::as_str), Some("arkforge"));
                parse_only(&words[1..]).unwrap_or_else(|error| {
                    panic!(
                        "example for {:?} did not parse: {example:?}: {}: {}",
                        spec.command, error.code, error.message
                    )
                });
            }
        }
    }

    #[test]
    fn help_placeholders_files_and_ellipses_are_never_concrete_identifiers() {
        let uppercase_digest = "A".repeat(64);
        for artifact in [
            "<artifact-id>",
            "./firmware.tar.gz",
            "...",
            &uppercase_digest,
        ] {
            assert!(
                validate_against_command_tree(&strings(&[
                    "flash",
                    "assess",
                    "--artifact",
                    artifact,
                    "--profile",
                    "org.openharmony.dayu200@1.0.0",
                    "--device",
                    "OBS-1",
                    "--intent",
                    "full-restore",
                ]))
                .is_err(),
                "artifact value {artifact:?} must not parse"
            );
        }
        assert!(
            validate_against_command_tree(&strings(&[
                "device",
                "show",
                "--device",
                "<observation-id>"
            ]))
            .is_err()
        );
    }

    #[test]
    fn typed_relations_refuse_ambiguous_or_incomplete_effect_inputs() {
        assert!(
            validate_against_command_tree(&strings(&["daemon", "start", "--hdc", "/opt/hdc"]))
                .is_err()
        );
        assert!(
            validate_against_command_tree(&strings(&[
                "rescue",
                "plan",
                "--device",
                "RESCUE-1",
                "--operation",
                "write-partition",
                "--partition",
                "boot_linux"
            ]))
            .is_err()
        );
        assert!(
            validate_against_command_tree(&strings(&[
                "rescue",
                "plan",
                "--device",
                "RESCUE-1",
                "--operation",
                "reset-device",
                "--image",
                "/tmp/image"
            ]))
            .is_err()
        );
        assert!(
            validate_against_command_tree(&strings(&[
                "flash",
                "apply",
                "--plan",
                "PLAN-1",
                "--expect-plan-sha256",
                &"0".repeat(64)
            ]))
            .is_err()
        );
        assert!(
            validate_against_command_tree(&strings(&[
                "rescue",
                "list",
                "--backend",
                "rkdeveloptool"
            ]))
            .is_err()
        );
        assert!(parse_globals(&strings(&["--quiet", "--verbose", "doctor"])).is_err());
    }

    #[test]
    fn jsonl_job_stream_has_metadata_ordered_events_and_terminal_record() {
        let events = vec![
            JobEvent {
                job_id: "JOB-1".into(),
                sequence: 2,
                job_state: "running".into(),
                ..JobEvent::default()
            },
            JobEvent {
                job_id: "JOB-1".into(),
                sequence: 3,
                job_state: "succeeded".into(),
                ..JobEvent::default()
            },
        ];
        let summary = JobSummary {
            job_id: "JOB-1".into(),
            state: "succeeded".into(),
            terminal: true,
            last_sequence: 3,
            ..JobSummary::default()
        };
        let records = render_job_jsonl(&["job", "watch"], 1, 1000, &events, &summary, false);
        assert_eq!(records.len(), 4);
        assert!(records[0].contains("\"record\":\"metadata\""));
        assert!(records[1].contains("\"stream_sequence\":1"));
        assert!(records[2].contains("\"stream_sequence\":2"));
        assert!(records[3].contains("\"record\":\"terminal\""));
        assert!(records.iter().all(|record| !record.contains("\u{1b}[")));
        assert!(records.iter().all(|record| !record.contains("connect_key")));
        assert!(
            records
                .iter()
                .all(|record| !record.contains("pairing_secret"))
        );
    }

    #[test]
    fn signing_requires_explicit_file_and_mode_options() {
        let valid =
            Options::parse(&strings(&["--file", "./arkforged", "--mode", "release"])).unwrap();
        validate_against_command_tree(&strings(&[
            "signing",
            "verify",
            "--file",
            "./arkforged",
            "--mode",
            "release",
        ]))
        .unwrap();
        assert_eq!(valid.one("file").unwrap(), "./arkforged");
        assert_eq!(valid.one("mode").unwrap(), "release");

        assert!(
            validate_against_command_tree(&strings(&["signing", "verify", "--release", "true"]))
                .is_err()
        );
    }

    #[test]
    fn json_escaping_covers_agent_visible_control_characters() {
        assert_eq!(json("a\n\"b\\c\t"), "\"a\\n\\\"b\\\\c\\t\"");
    }

    #[test]
    fn structured_errors_and_retries_do_not_echo_host_paths() {
        let arguments = strings(&[
            "--runtime-dir",
            "/private/secret/runtime",
            "--output",
            "json",
            "flash",
            "apply",
            "--plan",
            "PLAN-1",
            "--expect-plan-sha256",
            &"0".repeat(64),
        ]);
        let error = CliError::new(
            "ACKNOWLEDGEMENT_REQUIRED",
            "Missing exact acknowledgements.",
            4,
            true,
        )
        .with_required_acknowledgements(vec!["data-loss:userdata".into()]);
        let retry = acknowledgement_retry_command(&arguments, &error).unwrap();
        assert!(!retry.contains("/private/secret/runtime"));
        assert!(!retry.contains("--output"));
        assert!(retry.contains("--plan PLAN-1"));
        assert!(retry.contains("--ack data-loss:userdata"));

        let error = CliError::new(
            "PROFILE_FILE_NOT_FOUND",
            "Cannot read profile /private/secret/profile.yaml: missing",
            5,
            false,
        );
        let message = structured_error_message(
            &error,
            &strings(&["--profile-file", "/private/secret/profile.yaml"]),
        );
        assert_eq!(message, "Cannot read profile <redacted-path>: missing");
    }

    #[test]
    fn job_event_json_keeps_sequence_kind_and_typed_children() {
        let event = JobEvent {
            job_id: "JOB-1".into(),
            sequence: 7,
            job_state: "running".into(),
            ..JobEvent::default()
        };
        let rendered = job_event_json(&event);
        assert!(rendered.contains("\"job_id\":\"JOB-1\""));
        assert!(rendered.contains("\"sequence\":7"));
        assert!(rendered.contains("\"kind\":\"stateChanged\""));
        assert!(rendered.contains("\"admission\":null"));
        assert!(rendered.contains("\"control_request\":null"));
        assert!(rendered.contains("\"receipt\":null"));
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn parse_only(arguments: &[String]) -> Result<(), CliError> {
        let (_, command) = parse_globals(arguments)?;
        if command.is_empty() || command == ["--version"] || command == ["-V"] {
            return Ok(());
        }
        if command.first().is_some_and(|value| value == "help") {
            let mut topic = Vec::new();
            let mut index = 1;
            while index < command.len() {
                if command[index] == "--format" {
                    let format = command
                        .get(index + 1)
                        .ok_or_else(|| CliError::invalid("--format requires a value."))?;
                    if !matches!(format.as_str(), "human" | "json") {
                        return Err(CliError::invalid("invalid help format"));
                    }
                    index += 2;
                } else {
                    topic.push(command[index].clone());
                    index += 1;
                }
            }
            help_spec(&topic)?;
            return Ok(());
        }
        validate_against_command_tree(&command)
    }

    fn example_fixture(word: &str) -> String {
        if !word.contains('<') {
            return word.to_string();
        }
        if word.contains("sha256") || word.contains("64-lowercase-hex") {
            return "0".repeat(64);
        }
        if word.contains("u64") {
            return "1".into();
        }
        if word.contains("id@version") {
            return "org.openharmony.dayu200@1.0.0".into();
        }
        if word.contains("file") || word.contains("mach-o") || word.contains("absolute-path") {
            return "/tmp/arkforge-fixture".into();
        }
        match word {
            "<artifact-id>" => "0".repeat(64),
            "<profile-id>" => "org.openharmony.dayu200@1.0.0".into(),
            "<observation-id>" => "OBS-FIXTURE".into(),
            "<device-id>" => "RESCUE-FIXTURE".into(),
            "<job-id>" => "JOB-FIXTURE".into(),
            "<plan-id>" => "PLAN-FIXTURE".into(),
            "<partition>" => "boot_linux".into(),
            "<token>" => "data-loss:userdata".into(),
            _ => word.replace(['<', '>'], "fixture"),
        }
    }
}
