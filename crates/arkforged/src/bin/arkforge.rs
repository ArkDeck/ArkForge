//! Canonical ArkForge command frontend.
//!
//! The first implemented command family is explicit native RockUSB rescue.
//! It deliberately does not connect to the normal-flash authority runtime.

use arkforge_core::Sha256Digest;
use arkforged::dispatch::executable_digest;
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
    code: &'static str,
    message: String,
    exit_code: i32,
    retryable: bool,
}

impl CliError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: "INVALID_ARGUMENT",
            message: message.into(),
            exit_code: 2,
            retryable: false,
        }
    }
}

impl From<RescueError> for CliError {
    fn from(error: RescueError) -> Self {
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
    if command[0] != "rescue" {
        return Err(CliError::invalid(format!(
            "Unknown command {:?}. Run 'arkforge help' for the command tree.",
            command[0]
        )));
    }
    run_rescue(&command[1..], globals)
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
                    "Unexpected positional argument {:?}; all rescue inputs are named.",
                    arguments[index]
                ))
            })?;
            if option.is_empty() {
                return Err(CliError::invalid("'--' is not a rescue option."));
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
    let remediation = remediation(error.code);
    match output {
        Output::Human => {
            eprintln!("arkforge: {}: {}", error.code, error.message);
            if let Some(next) = remediation {
                eprintln!("Next: {next}");
            }
        }
        Output::Json => eprintln!(
            "{{\"schema_version\":\"arkforge.error/v1\",\"code\":{},\"message\":{},\"retryable\":{},\"next_commands\":{}}}",
            json(error.code),
            json(&error.message),
            error.retryable,
            remediation
                .map(|value| format!("[{}]", json(value)))
                .unwrap_or_else(|| "[]".into())
        ),
    }
}

fn remediation(code: &str) -> Option<&'static str> {
    match code {
        "INVALID_ARGUMENT" => Some("arkforge help rescue --format json"),
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
    match output {
        Output::Human => {
            println!("{}", spec.summary);
            println!();
            println!("Usage:\n  {}", spec.usage);
            println!();
            println!("Effect:\n  {}", spec.effect);
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
            println!(
                "{{\"schema_version\":{},\"command\":{},\"summary\":{},\"usage\":{},\"effect\":{},\"requires\":{},\"produces\":{},\"options\":[{}],\"examples\":{},\"next_commands\":{},\"exits\":[{}]}}",
                json(HELP_SCHEMA),
                json(spec.command),
                json(spec.summary),
                json(spec.usage),
                json(spec.effect),
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
            "arkforge help rescue --format json",
        ],
        next: &["arkforge rescue list"],
        exits: &[
            (0, "Help or version produced."),
            (2, "Command or option is invalid."),
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
    fn help_tree_has_every_rescue_leaf_and_agent_fields() {
        for topic in [
            "rescue list",
            "rescue inspect",
            "rescue read",
            "rescue plan",
            "rescue apply",
        ] {
            let topic = strings(&topic.split_whitespace().collect::<Vec<_>>());
            let help = help_spec(&topic).unwrap();
            assert!(!help.effect.is_empty());
            assert!(!help.produces.is_empty());
            assert!(!help.examples.is_empty());
            assert!(!help.next.is_empty());
            assert!(!help.exits.is_empty());
        }
        assert_eq!(HELP_SCHEMA, "arkforge.command-help/v1");
    }

    #[test]
    fn json_escaping_covers_agent_visible_control_characters() {
        assert_eq!(json("a\n\"b\\c\t"), "\"a\\n\\\"b\\\\c\\t\"");
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }
}
