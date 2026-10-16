//! Command help, the typed option tree, and help rendering.

use super::*;

pub(crate) fn validate_against_command_tree(
    arguments: &[String],
    interaction_open: bool,
) -> Result<(), CliError> {
    // The command path is the longest prefix the typed tree still recognizes.
    // Stopping at the first option would swallow a `key=value` operand into the
    // path; stopping at the first unrecognized word keeps both readable.
    let mut path_len = 0;
    while path_len < arguments.len() && !arguments[path_len].starts_with('-') {
        let candidate = arguments[..path_len + 1].join(" ");
        let prefix = format!("{candidate} ");
        if HELP
            .iter()
            .any(|spec| spec.command == candidate || spec.command.starts_with(&prefix))
        {
            path_len += 1;
        } else {
            break;
        }
    }
    let path = &arguments[..path_len];
    let spec = help_spec(path)?;
    let metadata = spec.typed_options();
    let known = metadata
        .iter()
        .map(|option| (option.name.as_str(), option))
        .collect::<BTreeMap<_, _>>();
    let mut supplied: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let operands = spec.operands();
    let mut index = path_len;
    while index < arguments.len() {
        let token = &arguments[index];
        if !token.starts_with("--") {
            // The one positional in the tree, and only where a person can see
            // what it resolved to. A script names its firmware with --file.
            if spec.command == "flash run" && index == path_len && interaction_open {
                index += 1;
                continue;
            }
            if !operands.is_empty() {
                validate_operand(operands, token)?;
                index += 1;
                continue;
            }
            let children = child_specs(spec.command);
            if !children.is_empty() {
                return Err(CliError::invalid(format!(
                    "Unknown subcommand {token:?}; it accepts {}.",
                    children
                        .iter()
                        .map(|child| child.command.rsplit(' ').next().unwrap_or(child.command))
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
            return Err(CliError::invalid(format!(
                "Unexpected positional argument {token:?}; command inputs are named options."
            )));
        }
        let name = token
            .strip_prefix("--")
            .expect("the token was checked to start with the option prefix");
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
    if (spec.command == "flash plan" || (spec.command == "flash run" && !interaction_open))
        && !supplied.contains_key("file")
        && !supplied.contains_key("artifact")
    {
        // Caught here, before any runtime or store is touched: naming no
        // firmware at all is a shape error, not a discovery about the host.
        return Err(CliError::new(
            "CONTENT_REQUIRED",
            "Supply the firmware exactly once as --file <path> or --artifact <artifact-id>.",
            2,
            false,
        ));
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

/// Checks one `key=value` (or bare `key`) operand against the declared set.
fn validate_operand(
    operands: &[(&'static str, &'static str)],
    token: &str,
) -> Result<(), CliError> {
    let (key, value) = match token.split_once('=') {
        Some((key, value)) => (key, Some(value)),
        None => (token, None),
    };
    // A campaign is refused by name rather than by absence, so an operator who
    // tries to make one durable learns why instead of learning "unknown key".
    if key == "campaign" || key.starts_with("campaign.") {
        return Err(CliError::new(
            "CAMPAIGN_NOT_PERSISTABLE",
            "A hardware campaign is named for one call with --hardware-campaign and is never stored; a campaign left switched on would stop meaning that the run was reviewed.",
            2,
            false,
        ));
    }
    let Some((_, value_type)) = operands.iter().find(|(name, _)| *name == key) else {
        return Err(CliError::invalid(format!(
            "Unknown setting {key:?}; this command accepts {}.",
            operands
                .iter()
                .map(|(name, _)| *name)
                .collect::<Vec<_>>()
                .join(", ")
        )));
    };
    match (*value_type, value) {
        ("", Some(_)) => Err(CliError::invalid(format!(
            "{key} names a setting to clear and takes no value."
        ))),
        ("", None) => Ok(()),
        (_, None) => Err(CliError::invalid(format!(
            "{key} requires a {value_type} value, written {key}=<{value_type}>."
        ))),
        ("sha256", Some(value)) => {
            let digest = Sha256Digest::parse_hex(value).map_err(|error| {
                CliError::invalid(format!("{key} requires one SHA-256: {error}"))
            })?;
            if digest.to_hex() != value {
                return Err(CliError::invalid(format!(
                    "{key} requires canonical 64-character lowercase SHA-256 hex."
                )));
            }
            Ok(())
        }
        ("absolute-path", Some(value)) => {
            if !Path::new(value).is_absolute() {
                return Err(CliError::invalid(format!(
                    "{key} requires an absolute path; the working directory is not part of a binding."
                )));
            }
            Ok(())
        }
        ("boolean", Some(value)) => match value {
            "true" | "false" => Ok(()),
            _ => Err(CliError::invalid(format!(
                "{key} accepts exactly 'true' or 'false', not {value:?}."
            ))),
        },
        (_, Some(_)) => Ok(()),
    }
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

pub(crate) fn run_help(arguments: &[String], global_output: Output) -> Result<i32, CliError> {
    let mut topic = Vec::new();
    let mut output = global_output;
    let mut all = false;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--all" => all = true,
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
    if all && !topic.is_empty() {
        return Err(CliError::invalid(
            "--all describes the whole command tree and takes no topic path.",
        ));
    }
    // A structured caller asking for help without a path wants the contract for
    // every command, not the root leaf: one call, whole tree.
    if all || (topic.is_empty() && output == Output::Json) {
        print_help_index(output);
        return Ok(0);
    }
    print_help(help_spec(&topic)?, output);
    Ok(0)
}
pub(crate) fn help_spec(topic: &[String]) -> Result<&'static HelpSpec, CliError> {
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
    if spec.command == "flash plan" {
        constraints.push(
            "{\"kind\":\"exactlyOneOf\",\"options\":[\"--file\",\"--artifact\"],\"required\":true}"
                .into(),
        );
    }
    if spec.command == "flash run" {
        constraints.push(
            "{\"kind\":\"exactlyOneOf\",\"options\":[\"--file\",\"--artifact\"],\"required\":\"unless-interactive\"}"
                .into(),
        );
        constraints.push(
            "{\"kind\":\"exactAcknowledgementSet\",\"tokens\":\"--ack\",\"required\":\"unless-interactive\"}"
                .into(),
        );
    }
    if matches!(spec.command, "apply" | "rescue apply") {
        constraints.push(
            "{\"kind\":\"exactAcknowledgementSet\",\"plan\":\"--plan\",\"digest\":\"--expect-plan-sha256\",\"tokens\":\"--ack\"}"
                .into(),
        );
    }
    constraints
}

pub(crate) fn print_help(spec: &HelpSpec, output: Output) {
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
        Output::Json => println!("{}", help_leaf_json(spec)),
    }
}

/// One `arkforge.command-help/v1` leaf. `help --all` embeds exactly this
/// rendering, so a per-path query and the index can never disagree.
fn help_leaf_json(spec: &HelpSpec) -> String {
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
    let subcommands = child_specs(spec.command)
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
    let facts_projections = spec
        .facts_projections()
        .iter()
        .map(|(name, schema, max_items)| {
            format!(
                "{{\"name\":{},\"schema\":{},\"max_items\":{max_items}}}",
                json(name),
                json(schema)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"schema\":{},\"path\":{},\"command\":{},\"summary\":{},\"usage\":{},\"effect\":{},\"effect_detail\":{},\"runtime_effect\":{},\"interactive\":false,\"availability\":{{\"platforms\":{},\"requires_daemon\":{},\"requires_controller\":{}}},\"subcommands\":[{}],\"requires\":{},\"outputs\":{},\"output_descriptions\":{},\"options\":[{}],\"constraints\":[{}],\"facts_projections\":[{}],\"examples\":{},\"next_commands\":{},\"exit_codes\":[{}]}}",
        json(HELP_SCHEMA),
        json_array(&spec.path()),
        json(spec.command),
        json(spec.summary),
        json(spec.usage),
        json(spec.effect_class()),
        json(spec.effect),
        json(spec.runtime_effect()),
        supported_platforms_json(),
        spec.requires_daemon(),
        spec.requires_controller(),
        subcommands,
        json_array(spec.requires),
        json_strings(&spec.output_schemas()),
        json_array(spec.produces),
        options,
        constraints,
        facts_projections,
        json_array(spec.examples),
        json_array(spec.next),
        exits
    )
}

/// The whole command tree in one document, ordered by path so two builds of the
/// same tree render byte-identically.
fn help_index_specs() -> Vec<&'static HelpSpec> {
    let mut specs = HELP.iter().collect::<Vec<_>>();
    specs.sort_by(|left, right| left.path().cmp(&right.path()));
    specs
}

pub(crate) fn print_help_index(output: Output) {
    let specs = help_index_specs();
    match output {
        Output::Human => {
            println!("ArkForge command tree ({} commands)", specs.len());
            for spec in &specs {
                let path = if spec.command.is_empty() {
                    "arkforge".to_string()
                } else {
                    format!("arkforge {}", spec.command)
                };
                println!("  {path:<28} {}", spec.summary);
            }
            println!();
            println!("Next: arkforge help <command> --format json");
        }
        Output::Json => {
            let commands = specs
                .iter()
                .map(|spec| help_leaf_json(spec))
                .collect::<Vec<_>>()
                .join(",");
            emit_json!(
                "{{\"schema\":{},\"command_count\":{},\"commands\":[{}]}}",
                json(HELP_INDEX_SCHEMA),
                specs.len(),
                commands
            );
        }
    }
}

fn supported_platforms_json() -> &'static str {
    // This field describes where the command surface is supported, not the
    // host that happened to build this binary. Keeping the complete, ordered
    // set also makes the machine-help contract byte-identical across ports.
    "[\"macos\",\"windows\"]"
}

pub(crate) fn child_specs(parent: &str) -> Vec<&'static HelpSpec> {
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
/// The `config set` binding example, written for the host this binary serves.
///
/// Absoluteness is a host judgement, and `validate_operand` applies the
/// host's: `/usr/local/bin/hdc` names a binding on macOS and a rootless guess
/// on Windows, where an absolute path carries a drive. One hardcoded literal
/// would therefore publish, on one of the two supported platforms, advice that
/// this same binary refuses.
#[cfg(target_os = "windows")]
const HDC_BINDING_EXAMPLE: &str =
    r"arkforge config set hdc.path=C:\controlled-tools\hdc.exe hdc.sha256=<64-lowercase-hex>";
#[cfg(not(target_os = "windows"))]
const HDC_BINDING_EXAMPLE: &str =
    "arkforge config set hdc.path=/usr/local/bin/hdc hdc.sha256=<64-lowercase-hex>";

/// The `daemon run` runtime-directory example, written for the host this
/// binary serves.
///
/// `--runtime-dir` carries the `<dir>` shape, so `validate_operand` never
/// judges its absoluteness and nothing refuses `/tmp/arkforge` on Windows.
/// That is the reason to split the literal, not a reason to leave it alone: a
/// refusal is read, while a rooted-but-driveless path is resolved silently
/// against whatever the current drive happens to be, and the operator learns
/// the runtime went to `C:\tmp\arkforge` only by going looking for it. The
/// Windows literal is per-user under LOCALAPPDATA, the location
/// `default_runtime_dir` already chooses there, and names the runtime it
/// creates rather than shadowing the default one.
#[cfg(target_os = "windows")]
const DAEMON_RUN_EXAMPLE: &str =
    r"arkforge --runtime-dir C:\Users\you\AppData\Local\ArkForge-Foreground daemon run";
#[cfg(not(target_os = "windows"))]
const DAEMON_RUN_EXAMPLE: &str = "arkforge --runtime-dir /tmp/arkforge daemon run";

pub(crate) static HELP: &[HelpSpec] = &[
    HelpSpec {
        command: "",
        summary: "ArkForge plans, executes, verifies, and recovers device firmware operations.",
        usage: "arkforge [global options] [<command> [<subcommand>]] [options]",
        effect: "With no command the root reports the aggregate host and runtime status. It never starts a runtime and never accesses a device for mutation.",
        requires: &[],
        produces: &[
            "arkforge.status/v1 for a bare invocation, or human help and arkforge.command-help-index/v1 for help.",
        ],
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
                "--no-auto-start",
                "Refuse instead of starting a runtime that is not already listening.",
            ),
            (
                "--no-input",
                "Never ask; treat every missing decision as a typed refusal.",
            ),
            (
                "--verbose",
                "Include diagnostic evidence; never include secrets.",
            ),
            ("-h, --help", "Show help for the current command."),
            ("-V, --version", "Print this build's ArkForge version."),
        ],
        examples: &[
            "arkforge help --all --format json",
            "arkforge help flash plan --format json",
        ],
        next: &["arkforge status"],
        exits: &[
            (0, "Status, help, or version produced."),
            (2, "Command or option is invalid."),
        ],
    },
    HelpSpec {
        command: "status",
        summary: "Report host, runtime, device, artifact, job, and blocker state in one document.",
        usage: "arkforge status",
        effect: "Read-only. It aggregates every fact this host can currently observe, never starts a runtime, and never opens a device for mutation.",
        requires: &[],
        produces: &[
            "arkforge.status/v1 with per-section availability, so a section that could not be observed is never reported as an empty one.",
        ],
        options: &[],
        examples: &["arkforge --output json status"],
        next: &["arkforge daemon start"],
        exits: &[
            (
                0,
                "A snapshot was produced, including a partial or not-ready one.",
            ),
            (2, "A global option is invalid."),
            (10, "The host root assessment itself could not be produced."),
        ],
    },
    HelpSpec {
        command: "device",
        summary: "Discover, identify, and wait for exact device observations.",
        usage: "arkforge device <list|wait> [options]",
        effect: "Read-only. Device commands cannot select a target, materialize authority, or mutate a device.",
        requires: &["A running ArkForge runtime for the selected --runtime-dir."],
        produces: &["Current observations with their identification evidence."],
        options: &[],
        examples: &[
            "arkforge device list",
            "arkforge help device list --format json",
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
        summary: "List current observations with what this build can prove about each.",
        usage: "arkforge device list [--device <observation-id>] [--deep] [--hardware-campaign <campaign-id>]",
        effect: "Read-only discovery through runtime-dir/public.sock. --deep additionally probes every candidate profile. No observation is selected and no device is mutated.",
        requires: &["A running ArkForge runtime."],
        produces: &[
            "arkforge.device-list/v1 observations, each with an identification block reporting compatible profiles and physical model separately, with evidence and strength.",
        ],
        options: &[
            (
                "--hardware-campaign <campaign-id>",
                "Exact campaign served by the running runtime; required for a campaign runtime and never inherited.",
            ),
            (
                "--device <observation-id>",
                "Report only this exact current observation; optional.",
            ),
            (
                "--deep",
                "Also probe every candidate profile and report the facts it returned.",
            ),
        ],
        examples: &[
            "arkforge --output json device list",
            "arkforge --output json device list --deep",
        ],
        next: &[
            "arkforge flash plan --artifact <artifact-id> --profile <profile-id> --device <observation-id> --intent full-restore",
        ],
        exits: &[
            (0, "Observation list produced, including an empty list."),
            (2, "Command or option is invalid."),
            (3, "Discovery or a candidate probe was refused."),
            (5, "The runtime or requested observation was not found."),
            (10, "Discovery or IPC failed."),
        ],
    },
    HelpSpec {
        command: "device wait",
        summary: "Wait for exactly one observation matching an explicit profile and mode.",
        usage: "arkforge device wait --profile <profile-id> --mode <mode> [--timeout-ms <u64>] [--hardware-campaign <campaign-id>]",
        effect: "Repeated read-only discovery and probing. It never chooses the first match; multiple matches are a typed ambiguity refusal.",
        requires: &["A loaded profile id and an explicit expected mode."],
        produces: &["arkforge.device-wait/v1 with the unique probed observation and facts digest."],
        options: &[
            (
                "--hardware-campaign <campaign-id>",
                "Exact campaign served by the running runtime; required for a campaign runtime and never inherited.",
            ),
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
            "arkforge flash plan --file <firmware-file> --profile <profile-id> --device <observation-id>",
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
        summary: "Import, list, and show content-addressed firmware artifacts.",
        usage: "arkforge artifact <import|list|show> [options]",
        effect: "Import writes only the local content-addressed store; list and show are offline reads of it. No artifact command mutates a device.",
        requires: &["An explicit runtime directory or the per-user default."],
        produces: &["Artifact IDs, stored-object lists, or complete inspected manifests."],
        options: &[],
        examples: &[
            "arkforge artifact import --file <firmware-file>",
            "arkforge help artifact show --format json",
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
        command: "artifact preboot-ramdisk",
        summary: "Derive the pinned temporary DAYU200 acceptance ramdisk on the host.",
        usage: "arkforge artifact preboot-ramdisk --file <original-ramdisk.img> --support-archive <pinned.cpio.gz> --out <new-image> --restore-out <new-image>",
        effect: "Host-write only. Preserves the pinned base, adds the closed pre-mount verifier and a random nonce, and saves a content-addressed derivative. Does not access the device.",
        requires: &[
            "The exact pinned original DAYU200 ramdisk, its closed compressed crypto dependency archive, and new output paths.",
        ],
        produces: &[
            "arkforge.preboot-ramdisk/v1 with derivative digest, original digest, nonce and size.",
        ],
        options: &[
            ("--file <file>", "Required. Pinned original ramdisk."),
            (
                "--support-archive <file>",
                "Required. Exact pinned gzip/newc dependency archive; arbitrary libraries and scripts are refused.",
            ),
            (
                "--restore-out <new-file>",
                "Required. Original ramdisk padded with zeros to its full partition extent, clearing every temporary overlay byte.",
            ),
            (
                "--out <new-file>",
                "Required. New host output; never overwritten.",
            ),
        ],
        examples: &[
            "arkforge artifact preboot-ramdisk --file ramdisk.img --support-archive preboot-support.cpio.gz --out acceptance-ramdisk.img --restore-out restore-ramdisk.img",
        ],
        next: &[
            "Build a derived firmware bundle preserving all other members, then seal an ordinary full-restore plan.",
        ],
        exits: &[
            (0, "Derivative saved; no device accessed."),
            (2, "Invalid arguments."),
            (3, "Pinned input or output refused."),
        ],
    },
    HelpSpec {
        command: "artifact import",
        summary: "Import one firmware file and report what it is and what it fits.",
        usage: "arkforge artifact import --file <firmware-file> [--expect-sha256 <sha256>]",
        effect: "Host write only. It creates or deduplicates one content-addressed object after quota, size, and optional digest checks, then reads the stored bytes and the current observations; no device is mutated.",
        requires: &[
            "One regular input file.",
            "Enough store quota and volume reserve for the complete input.",
        ],
        produces: &[
            "arkforge.artifact-import/v1 with CAS facts, a bounded manifest summary, the profiles that declare the parsed format, and the connected devices those profiles could flash. The present-device section reports availability, so a runtime that is not running is unknown rather than empty.",
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
        next: &["arkforge artifact show --artifact <returned-artifact-id>"],
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
        command: "artifact list",
        summary: "List every stored object with the format and profiles it parses as.",
        usage: "arkforge artifact list",
        effect: "Read-only when a store exists. An absent store is reported as an empty list and is not created. Each stored object is parsed to report its format, so listing costs one container read per object.",
        requires: &[],
        produces: &[
            "arkforge.artifact-list/v1 with artifact ids, byte sizes, the parsed format, and the profiles declaring it; an object that cannot be parsed carries a null format and a typed reason.",
        ],
        options: &[],
        examples: &["arkforge --output json artifact list"],
        next: &["arkforge artifact show --artifact <artifact-id>"],
        exits: &[
            (0, "Artifact list produced, including an empty list."),
            (10, "The content store could not be read."),
        ],
    },
    HelpSpec {
        command: "artifact show",
        summary: "Parse one stored artifact offline and optionally compare profile coverage.",
        usage: "arkforge artifact show --artifact <artifact-id> [--profile-file <file>]",
        effect: "Read-only artifact parsing after opening bytes by content digest. It never reparses a caller path, never contacts the runtime, and never accesses a device.",
        requires: &["One exact artifact id already present in this runtime store."],
        produces: &[
            "arkforge.artifact-inspection/v1 with the complete manifest, the profiles declaring its format, and optional ordered profile target coverage.",
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
            "arkforge --output json artifact show --artifact <64-lowercase-hex> --profile-file profiles/dayu200.yaml",
        ],
        next: &[
            "arkforge flash plan --artifact <artifact-id> --profile <profile-id> --device <observation-id> --intent full-restore",
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
        command: "flash",
        summary: "Stage and seal normal firmware work against exact resources.",
        usage: "arkforge flash <run|plan|verify> [options]",
        effect: "Run performs the whole flash behind one consent gate; plan stops after sealing, and the top-level apply executes what it sealed. Verify independently reads completed image ranges in Normal mode.",
        requires: &[
            "Firmware content, and enough evidence to name exactly one device and profile.",
        ],
        produces: &["One composite staging document."],
        options: &[],
        examples: &["arkforge help flash run --format json"],
        next: &["arkforge flash run --file <firmware-file> --ack <token>"],
        exits: &[
            (0, "Staging document produced."),
            (2, "Command or option is invalid."),
            (3, "Materialization was refused."),
            (5, "A required runtime object was not found."),
            (
                6,
                "The device or profile could not be narrowed to exactly one.",
            ),
            (10, "Assessment or IPC failed."),
        ],
    },
    HelpSpec {
        command: "flash run",
        summary: "Import, identify, assess, seal, accept, and write, in one command.",
        usage: "arkforge flash run [FILE] [--file <firmware-file> | --artifact <artifact-id>] [--device <observation-id> | --target <selector>] [--profile <id@version>] [--intent <full-restore>] [--hardware-campaign <campaign-id>] [--ack <token>]... [--wait-device <u64>] [--detach]",
        effect: "Destructive. Every stage before consent only reads or writes host storage; the write itself passes the same sealed plan, exact digest, and exact acknowledgement set the two-command path does. There is no broad --yes or --force.",
        requires: &[
            "A running paired CLI authority supervisor, started for this call if none is listening.",
            "Firmware content, named or selected.",
            "Consent: a confirmation screen on a terminal, or exactly the sealed tokens as --ack.",
            "A durable arkforge.cli-approval/v1 record; if it cannot be written, nothing is dispatched.",
        ],
        produces: &[
            "arkforge.job-event/v1 and arkforge.command-result/v1 with a durable job id and terminal classification; --detach returns arkforge.flash-run/v1. A missing acknowledgement returns the sealed plan and the exact apply command under error.facts, so the plan is never materialized twice.",
        ],
        options: &[
            (
                "--file <firmware-file>",
                "Firmware container imported into the content store before the plan binds it.",
            ),
            (
                "--artifact <artifact-id>",
                "Exact already-imported content id; conflicts with --file.",
            ),
            (
                "--device <observation-id>",
                "Exact current observation; conflicts with --target.",
            ),
            (
                "--target <selector>",
                "Serial digest, unique identifier prefix of at least four characters, or proven product model; conflicts with --device.",
            ),
            (
                "--profile <id@version>",
                "Exact loaded profile identity; inferred when the compatible set has exactly one member.",
            ),
            (
                "--intent <full-restore>",
                "Semantic intent; defaulted when the profile and format admit exactly one.",
            ),
            (
                "--hardware-campaign <campaign-id>",
                "Named acceptance campaign the running runtime must serve; never inherited.",
            ),
            (
                "--ack <token>",
                "Exact required effect token; repeat exactly as the plan declares. Required when no confirmation screen is available.",
            ),
            (
                "--wait-device <u64>",
                "Bounded wait in milliseconds for a matching device to appear.",
            ),
            (
                "--detach",
                "Return after job creation; does not cancel or transfer authority.",
            ),
        ],
        examples: &[
            "arkforge flash run --file <firmware-file> --profile org.openharmony.dayu200@1.0.0 --device OBS-PREFLIGHT --ack data-loss:userdata",
        ],
        next: &["arkforge watch --job <job-id>"],
        exits: &[
            (0, "The device was written and the job succeeded."),
            (2, "Inputs are invalid, or no firmware was named."),
            (
                3,
                "Mechanics, authority, campaign, or physical identity precondition refused.",
            ),
            (
                4,
                "Consent was declined, or the acknowledgement set is not exact.",
            ),
            (5, "A named resource, device, or runtime is unavailable."),
            (
                6,
                "The device or profile could not be narrowed to exactly one, or an approval conflicts.",
            ),
            (7, "Operation ended with a known non-success outcome."),
            (8, "Outcome is unknown; never retry automatically."),
            (9, "Tracking ended without a terminal result."),
            (10, "Controller, store, approval, or supervisor failed."),
        ],
    },
    HelpSpec {
        command: "flash restore-ramdisk",
        summary: "Seal a separate pinned-original-ramdisk-only DAYU200 restoration plan.",
        usage: "arkforge flash restore-ramdisk --artifact <original-artifact-id> --profile <id@version> --device <observation-id> --hardware-campaign <campaign-id> [--source-job <job-id>] [--assess-only]",
        effect: "Planning only. The sealed plan is destructive for ramdisk, includes mode transitions, verification and postflight, and never writes userdata. Apply still requires exact digest and acknowledgement tokens.",
        requires: &[
            "A named DAYU200 campaign and artifact containing the pinned original ramdisk.",
            "An explicit --profile and exact --device when board identity is unproven.",
        ],
        produces: &[
            "arkforge.flash-plan/v2 with one persistent ramdisk effect and an exact apply command.",
        ],
        options: &[
            (
                "--artifact <artifact-id>",
                "Required. Original firmware artifact.",
            ),
            (
                "--source-job <job-id>",
                "Require the fresh board to match the source job's durable identity and confirmed full-flash reboot receipts, including after Runtime restart.",
            ),
            (
                "--profile <id@version>",
                "Exact loaded DAYU200 profile identity; required when board identity is unproven.",
            ),
            (
                "--device <observation-id>",
                "Required. Exact current observation.",
            ),
            (
                "--hardware-campaign <campaign-id>",
                "Required. Named acceptance campaign.",
            ),
            ("--assess-only", "Return only the assessment."),
        ],
        examples: &[
            "arkforge flash restore-ramdisk --artifact <artifact-id> --profile org.openharmony.dayu200@1.0.0 --device <observation-id> --hardware-campaign <campaign-id>",
        ],
        next: &[
            "Review and execute the returned apply command, then flash verify the restoration job.",
        ],
        exits: &[
            (0, "Plan or assessment returned."),
            (2, "Invalid inputs."),
            (3, "Safety or campaign preconditions refused."),
            (5, "Runtime or target unavailable."),
        ],
    },
    HelpSpec {
        command: "flash verify",
        summary: "Verify a completed DAYU200 job's full image ranges in Normal mode.",
        usage: "arkforge flash verify --job <job-id> [--budget-ms <u64>] [--hardware-campaign <campaign-id>] [--preboot-ramdisk <sha256>]",
        effect: "Read-only device access; persists a separate verification record. Exact ranges and expected digests come only from the completed job's sealed plan. Never rewrites historical job receipts or promotes hardware support.",
        requires: &[
            "A successful complete DAYU200 job owned by this standalone runtime; no active jobs.",
            "The same strongly identified board in Normal mode and a hash-bound HDC executable.",
        ],
        produces: &[
            "arkforge.flash-verification/v1 with fullHash, typedSkip, or failed per image range and an independent durable record id.",
        ],
        options: &[
            (
                "--preboot-ramdisk <sha256>",
                "Use the exact host derivative matching the source job's ramdisk write to validate its current-boot pre-mount userdata report; failures are never skips.",
            ),
            (
                "--job <job-id>",
                "Required. Exact completed job reference; no caller paths, ranges, or expected hashes are accepted.",
            ),
            (
                "--budget-ms <u64>",
                "Total device read budget in milliseconds, 1000..600000; defaults to 300000.",
            ),
            (
                "--hardware-campaign <campaign-id>",
                "Exact named campaign of the original job and current runtime; never inherited.",
            ),
        ],
        examples: &[
            "arkforge --output json flash verify --job <job-id> --hardware-campaign <campaign-id>",
        ],
        next: &["arkforge job show --job <job-id>"],
        exits: &[
            (0, "Every image range matched its complete SHA-256 digest."),
            (2, "The job reference or budget is invalid."),
            (
                3,
                "Source, identity, campaign, or execution state refused verification.",
            ),
            (5, "A required job or runtime resource was not found."),
            (
                7,
                "At least one range failed verification; inspect its explicit reason.",
            ),
            (
                9,
                "Partial verification: no failures, but at least one typed skip, such as writable userdata.",
            ),
            (10, "IPC or durable recording failed."),
        ],
    },
    HelpSpec {
        command: "flash plan",
        summary: "Import, identify, assess, and seal one normal-flash plan in one call.",
        usage: "arkforge flash plan (--file <firmware-file> | --artifact <artifact-id>) [--device <observation-id> | --target <selector>] [--profile <id@version>] [--intent <full-restore>] [--hardware-campaign <campaign-id>] [--wait-device <u64>] [--assess-only]",
        effect: "Imports bytes into the content store, reads the exact device through the paired runtime, and stores a sealed plan. It does not mutate the device. --assess-only stops before sealing and materializes nothing.",
        requires: &[
            "A running paired CLI authority supervisor.",
            "Exact mechanics maturity and independent authority support, for a sealed plan.",
            "An explicit --profile and exact --device when this build cannot prove which physical board the target is.",
        ],
        produces: &[
            "arkforge.flash-plan/v2 carrying the resolved artifact, device identification, profile and intent with how each was decided, the assessment, the sealed plan, and the exact apply command. A refused plan returns the same document under error.facts.flash_plan with plan null.",
        ],
        options: &[
            (
                "--file <firmware-file>",
                "Firmware container imported into the content store before the plan binds it.",
            ),
            (
                "--artifact <artifact-id>",
                "Exact already-imported content id; conflicts with --file.",
            ),
            (
                "--device <observation-id>",
                "Exact current observation; conflicts with --target.",
            ),
            (
                "--target <selector>",
                "Serial digest, unique identifier prefix of at least four characters, or proven product model; conflicts with --device.",
            ),
            (
                "--profile <id@version>",
                "Exact loaded profile identity; inferred when the compatible set has exactly one member.",
            ),
            (
                "--intent <full-restore>",
                "Semantic intent; defaulted when the profile and format admit exactly one.",
            ),
            (
                "--hardware-campaign <campaign-id>",
                "Named acceptance campaign the running runtime must already serve; it is never restarted to match.",
            ),
            (
                "--wait-device <u64>",
                "Bounded wait in milliseconds for a matching device to appear.",
            ),
            (
                "--assess-only",
                "Produce the assessment and stop; no plan is materialized.",
            ),
        ],
        examples: &[
            "arkforge --output json flash plan --file <firmware-file> --profile org.openharmony.dayu200@1.0.0 --device OBS-PREFLIGHT",
            "arkforge --output json flash plan --artifact <artifact-id> --assess-only",
        ],
        next: &[
            "Use the returned apply_command verbatim after reviewing required_acknowledgements.",
        ],
        exits: &[
            (
                0,
                "Plan sealed, or assessment produced under --assess-only.",
            ),
            (2, "Inputs are invalid, or no firmware was named."),
            (
                3,
                "Mechanics or authority support is unavailable, or the physical identity was not established.",
            ),
            (5, "A named resource, device, or runtime is unavailable."),
            (
                6,
                "The device or profile could not be narrowed to exactly one, or the runtime serves another campaign.",
            ),
            (10, "Controller, store, or supervisor failed."),
        ],
    },
    HelpSpec {
        command: "apply",
        summary: "Execute one sealed plan after exact digest and acknowledgement equality.",
        usage: "arkforge apply --plan <plan-id> --expect-plan-sha256 <sha256> --ack <token> [--ack <token>...] [--hardware-campaign <campaign-id>] [--detach]",
        effect: "Destructive. Starts only the exact sealed plan after digest and acknowledgement equality; the supervisor mints one durable single-use permit per admitted step. There is no broad --yes or --force.",
        requires: &[
            "A live paired authority supervisor and fresh exact target continuity.",
            "The exact plan digest and exactly every returned acknowledgement token, with no extras.",
            "The runtime's hardware campaign named for this call, when it serves one.",
        ],
        produces: &[
            "arkforge.job-event/v1 and arkforge.command-result/v1 with a durable job id, ordered events, and terminal classification; --detach returns arkforge.apply/v1 after durable job creation while authority continues.",
        ],
        options: &[
            (
                "--plan <plan-id>",
                "Exact sealed normal-flash or recovery plan; required. A rescue-plan id is refused and directed to rescue apply.",
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
                "--hardware-campaign <campaign-id>",
                "Named acceptance campaign the running runtime already serves; never inherited and never restarted to match.",
            ),
            (
                "--detach",
                "Return after job creation; does not cancel or transfer authority.",
            ),
        ],
        examples: &[
            "arkforge apply --plan PLAN-EXAMPLE --expect-plan-sha256 <64-lowercase-hex> --ack data-loss:userdata",
        ],
        next: &["arkforge watch --job <job-id>"],
        exits: &[
            (0, "Detached job created or watched job succeeded."),
            (
                2,
                "Inputs are invalid, or the plan belongs to the rescue domain.",
            ),
            (
                3,
                "Plan, target, authority, campaign, or freshness precondition refused.",
            ),
            (4, "Plan digest or acknowledgement set is not exact."),
            (5, "Runtime or plan was not found."),
            (6, "The runtime serves another hardware campaign."),
            (7, "Operation ended with a known non-success outcome."),
            (8, "Outcome is unknown; never retry automatically."),
            (9, "Watching ended without a terminal result."),
            (10, "Controller, supervisor, or journal failed."),
        ],
    },
    HelpSpec {
        command: "watch",
        summary: "Follow one job's durable events, defaulting to the job that is running.",
        usage: "arkforge watch [--job <job-id>] [--after-sequence <u64>] [--timeout-ms <u64>] [--hardware-campaign <campaign-id>]",
        effect: "Read-only polling of durable events and point-in-time status. Timeout ends only this observation; it never cancels or changes the job.",
        requires: &[
            "A running ArkForge runtime, and a resume sequence no greater than the durable last_sequence.",
        ],
        produces: &[
            "arkforge.job-watch/v1, arkforge.job-event/v1, and arkforge.command-result/v1 with strictly ordered typed events, terminal/timed-out status, and an exact resume command.",
        ],
        options: &[
            (
                "--hardware-campaign <campaign-id>",
                "Exact campaign served by the running runtime; required for a campaign runtime and never inherited.",
            ),
            (
                "--job <job-id>",
                "Exact durable job; defaults to the single running job, or the most recently active one when none is running.",
            ),
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
            "arkforge --output json watch",
            "arkforge --output json watch --job <job-id> --after-sequence 0 --timeout-ms 30000",
        ],
        next: &[
            "If non-terminal, repeat next_commands[0]; stopping the watch never cancels the job.",
        ],
        exits: &[
            (
                0,
                "Events and status produced, including a timed-out observation.",
            ),
            (2, "Options are invalid."),
            (
                5,
                "The runtime or job was not found, or no job is recorded.",
            ),
            (
                6,
                "Several jobs are running, or the resume cursor is ahead of the durable sequence.",
            ),
            (10, "Job query or IPC failed."),
        ],
    },
    HelpSpec {
        command: "cancel",
        summary: "Ask the authority to stop one job at a safe boundary.",
        usage: "arkforge cancel --job <job-id> --expect-sequence <u64> [--hardware-campaign <campaign-id>]",
        effect: "Mutating control only. It requests a safe stop at an optimistic sequence; it never edits the journal, replays an action, or reclassifies an outcome.",
        requires: &["A durable job and the exact expected last sequence."],
        produces: &[
            "arkforge.job-cancellation/v1 with one of four typed dispositions and no automatic replay.",
        ],
        options: &[
            (
                "--hardware-campaign <campaign-id>",
                "Exact campaign served by the running runtime; required for a campaign runtime and never inherited.",
            ),
            ("--job <job-id>", "Exact durable job; required."),
            (
                "--expect-sequence <u64>",
                "Optimistic concurrency cursor; required.",
            ),
        ],
        examples: &["arkforge --output json cancel --job JOB-EXAMPLE --expect-sequence 4"],
        next: &["arkforge job show --job <job-id>"],
        exits: &[
            (0, "Cancelled safely or already terminal."),
            (2, "Inputs are invalid."),
            (5, "Runtime or job was not found."),
            (6, "Expected sequence is stale."),
            (8, "Outcome is unknown."),
            (9, "Cancellation is queued at a safe boundary."),
            (10, "Controller or supervisor failed."),
        ],
    },
    HelpSpec {
        command: "job",
        summary: "Observe, reconcile, and recover durable jobs.",
        usage: "arkforge job <list|show|reconcile|recover> [options]",
        effect: "Observation and reconciliation are read-only; recover creates a distinct superseding plan and never replays the original job. Following and stopping a job are the top-level watch and cancel commands.",
        requires: &["A running ArkForge runtime."],
        produces: &["Durable point-in-time job status or typed recovery guidance."],
        options: &[],
        examples: &["arkforge job list", "arkforge help job show --format json"],
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
        usage: "arkforge job list [--hardware-campaign <campaign-id>]",
        effect: "Read-only. It does not watch, resume, cancel, reconcile, or mutate a job.",
        requires: &["A running ArkForge runtime."],
        produces: &["arkforge.job-list/v1, including an empty jobs array."],
        options: &[(
            "--hardware-campaign <campaign-id>",
            "Exact campaign served by the running runtime; required for a campaign runtime and never inherited.",
        )],
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
        summary: "Show one durable job with its event tail, receipts, and recovery block.",
        usage: "arkforge job show --job <job-id> [--hardware-campaign <campaign-id>]",
        effect: "Read-only point-in-time status. It neither waits for events nor changes the job.",
        requires: &["One exact job id from job list or a prior command result."],
        produces: &[
            "arkforge.job/v1 with plan binding, state, progress, the last 20 durable events, every action receipt, and the no-replay recovery block. Each embedded section reports its own availability, so an unreadable one is never rendered as an absent one.",
        ],
        options: &[
            (
                "--hardware-campaign <campaign-id>",
                "Exact campaign served by the running runtime; required for a campaign runtime and never inherited.",
            ),
            ("--job <job-id>", "Exact durable job id; required."),
        ],
        examples: &["arkforge --output json job show --job <job-id>"],
        next: &["If state is outcomeUnknown, run 'arkforge job reconcile --job <job-id>'."],
        exits: &[
            (
                0,
                "Job document produced, including a partially observable one.",
            ),
            (2, "The job id is missing."),
            (5, "The runtime or job was not found."),
            (10, "Job query or IPC failed."),
        ],
    },
    HelpSpec {
        command: "job reconcile",
        summary: "Assess possible effects without replaying an unresolved job.",
        usage: "arkforge job reconcile --job <job-id> [--hardware-campaign <campaign-id>]",
        effect: "Read-only controller assessment. It never dispatches, resumes, edits, or reclassifies the original job optimistically.",
        requires: &["A durable job in the selected paired runtime."],
        produces: &[
            "arkforge.job-reconciliation/v1 with immutable original state, possible-effect completeness, effects, and verdict.",
        ],
        options: &[
            (
                "--hardware-campaign <campaign-id>",
                "Exact campaign served by the running runtime; required for a campaign runtime and never inherited.",
            ),
            ("--job <job-id>", "Exact durable job; required."),
        ],
        examples: &["arkforge --output json job reconcile --job JOB-EXAMPLE"],
        next: &["arkforge job show --job <job-id>"],
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
        command: "job recover",
        summary: "Seal a distinct plan covering the complete outstanding unknown set.",
        usage: "arkforge job recover --job <job-id> (--file <firmware-file> | --artifact <artifact-id>) [--device <observation-id> | --target <selector>] [--profile <id@version>] [--intent <full-restore>] [--hardware-campaign <campaign-id>] [--wait-device <u64>]",
        effect: "Reads the exact device through the paired runtime and stores a sealed host object. It never resumes, edits, replays, or reclassifies any original job; their outcomes and journals stay exactly as they are.",
        requires: &[
            "One exact anchor job in the complete outstanding unknown set; every source requires independent coverage, identity proof and unspent recovery allowance.",
            "Firmware content, and enough evidence to name exactly one device and profile.",
        ],
        produces: &[
            "arkforge.flash-plan/v2 whose plan covers the complete outstanding unknown set under a new epoch and intent. The named job anchors the recovery:supersedes-job token; the Runtime seals and rechecks every source. The assessment projects the same target's effects and gate state.",
        ],
        options: &[
            (
                "--job <job-id>",
                "Exact unresolved anchor job; does not narrow the complete outstanding set. Required.",
            ),
            (
                "--file <firmware-file>",
                "Firmware container imported into the content store before the plan binds it.",
            ),
            (
                "--artifact <artifact-id>",
                "Exact already-imported content id; conflicts with --file.",
            ),
            (
                "--device <observation-id>",
                "Exact current observation; conflicts with --target.",
            ),
            (
                "--target <selector>",
                "Serial digest, unique identifier prefix of at least four characters, or proven product model; conflicts with --device.",
            ),
            (
                "--profile <id@version>",
                "Exact loaded profile identity; inferred when the compatible set has exactly one member.",
            ),
            (
                "--intent <full-restore>",
                "Semantic intent; defaulted when the profile and format admit exactly one.",
            ),
            (
                "--hardware-campaign <campaign-id>",
                "Named acceptance campaign the running runtime must serve; never inherited.",
            ),
            (
                "--wait-device <u64>",
                "Bounded wait in milliseconds for a matching device to appear.",
            ),
        ],
        examples: &[
            "arkforge --output json job recover --job <job-id> --artifact <artifact-id> --profile org.openharmony.dayu200@1.0.0 --device OBS-PREFLIGHT",
        ],
        next: &["Use the returned apply_command verbatim after reviewing the superseding effects."],
        exits: &[
            (0, "Superseding plan sealed; the original job is unchanged."),
            (2, "Inputs are invalid, or no firmware was named."),
            (
                3,
                "No superseding plan was created, or a support precondition refused.",
            ),
            (5, "The runtime, job, or a named resource was not found."),
            (
                6,
                "The device or profile could not be narrowed to exactly one, or the runtime serves another campaign.",
            ),
            (10, "Controller, store, or supervisor failed."),
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
        usage: "arkforge daemon <run|start|stop> [options]",
        effect: "Service lifecycle. The supervisor owns pairing authority; lifecycle commands do not flash a device.",
        requires: &[
            "arkforged installed beside the canonical arkforge executable.",
            "The bindings stored by config, which lifecycle commands read and explicit arguments override.",
        ],
        produces: &[
            "Typed two-process runtime status with protocol, authority epoch, readiness, active jobs, and blockers.",
        ],
        options: &[],
        examples: &["arkforge daemon start", "arkforge status"],
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
        examples: &[DAEMON_RUN_EXAMPLE],
        next: &["arkforge status"],
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
        command: "config",
        summary: "Bind reusable local tools and profiles for this runtime directory.",
        usage: "arkforge config <show|set|unset|add|remove> [<key>=<value>...]",
        effect: "Owner-only host state. Configuration names what this host may drive; it never grants consent, never opens a hardware campaign, and never touches a device.",
        requires: &["An explicit runtime directory or the per-user default."],
        produces: &[
            "arkforge.config/v1 with binding state, digests, and counts; structured output never carries a host path.",
        ],
        options: &[],
        examples: &[
            "arkforge config show",
            "arkforge help config set --format json",
        ],
        next: &["arkforge config show"],
        exits: &[
            (0, "Configuration produced or committed."),
            (2, "Command, setting, or value is invalid."),
            (3, "A digest did not match, so nothing was stored."),
            (5, "A named file or binding was not found."),
            (6, "The binding already exists."),
            (10, "The durable configuration boundary failed."),
        ],
    },
    HelpSpec {
        command: "config show",
        summary: "Show which tools and profiles this runtime directory has bound.",
        usage: "arkforge config show",
        effect: "Read-only. It does not create a runtime directory it did not find.",
        requires: &[],
        produces: &[
            "arkforge.config/v1 with HDC binding state and digest, profile digests and count, and the release signing requirement. Host and HDC paths are shown only to the owner in human output.",
        ],
        options: &[],
        examples: &["arkforge --output json config show"],
        next: &["arkforge status"],
        exits: &[
            (0, "Configuration produced, including an empty one."),
            (3, "The stored configuration could not be read."),
            (10, "The configuration store failed."),
        ],
    },
    HelpSpec {
        command: "config set",
        summary: "Bind the exact managed-control executable or the signing requirement.",
        usage: "arkforge config set hdc.path=<absolute-path> hdc.sha256=<sha256> | daemon.require-release-signing=<true|false>",
        effect: "Owner-only host write, committed atomically. A failed write leaves the previous configuration exactly as it was.",
        requires: &[
            "An absolute path whose current bytes have the supplied digest.",
            "hdc.path and hdc.sha256 supplied together; one alone is refused.",
        ],
        produces: &["arkforge.config/v1 after the transaction commits."],
        options: &[],
        examples: &[
            "arkforge config set daemon.require-release-signing=true",
            HDC_BINDING_EXAMPLE,
        ],
        next: &["arkforge config show"],
        exits: &[
            (0, "Binding committed."),
            (
                2,
                "A setting is unknown, malformed, relative, or a campaign key.",
            ),
            (3, "The file does not have the expected digest."),
            (5, "The named file was not found."),
            (10, "The durable configuration boundary failed."),
        ],
    },
    HelpSpec {
        command: "config unset",
        summary: "Clear one binding, leaving every other setting untouched.",
        usage: "arkforge config unset <hdc|daemon.require-release-signing>",
        effect: "Owner-only host write, committed atomically. Clearing the HDC binding clears its path and digest together.",
        requires: &["One setting name to clear."],
        produces: &["arkforge.config/v1 after the transaction commits."],
        options: &[],
        examples: &["arkforge config unset hdc"],
        next: &["arkforge config show"],
        exits: &[
            (0, "Binding cleared."),
            (2, "The setting name is unknown or carries a value."),
            (10, "The durable configuration boundary failed."),
        ],
    },
    HelpSpec {
        command: "config add",
        summary: "Add one additional development profile by absolute path and digest.",
        usage: "arkforge config add profile-file.path=<absolute-path> profile-file.sha256=<sha256>",
        effect: "Owner-only host write, committed atomically. The profile is re-hashed before every runtime start, and byte drift is a typed refusal.",
        requires: &[
            "An absolute path whose current bytes have the supplied digest.",
            "A digest not already configured.",
        ],
        produces: &["arkforge.config/v1 after the transaction commits."],
        examples: &[
            "arkforge config add profile-file.path=<absolute-path> profile-file.sha256=<64-lowercase-hex>",
        ],
        options: &[],
        next: &["arkforge config show"],
        exits: &[
            (0, "Profile added."),
            (2, "A setting is unknown, malformed, or relative."),
            (3, "The file does not have the expected digest."),
            (5, "The named file was not found."),
            (6, "That digest is already configured."),
            (10, "The durable configuration boundary failed."),
        ],
    },
    HelpSpec {
        command: "config remove",
        summary: "Remove one configured profile by its exact digest.",
        usage: "arkforge config remove profile-file.sha256=<sha256>",
        effect: "Owner-only host write, committed atomically. Removal is by digest, so a moved or renamed file cannot be unbound by accident.",
        requires: &["A digest that is currently configured."],
        produces: &["arkforge.config/v1 after the transaction commits."],
        options: &[],
        examples: &["arkforge config remove profile-file.sha256=<64-lowercase-hex>"],
        next: &["arkforge config show"],
        exits: &[
            (0, "Profile removed."),
            (2, "The setting is unknown or malformed."),
            (5, "No configured profile has that digest."),
            (10, "The durable configuration boundary failed."),
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
        usage: "arkforge help [<command> [<subcommand>...]] [--all] [--format <human|json>]",
        effect: "Read-only and offline. Help is generated from the canonical typed command tree.",
        requires: &[],
        produces: &[
            "One arkforge.command-help/v1 leaf for a topic path, or the whole tree as arkforge.command-help-index/v1 for --all and for structured help without a path.",
        ],
        options: &[
            (
                "--all",
                "Describe the whole command tree in one document; takes no topic path.",
            ),
            (
                "--format <human|json>",
                "Help presentation format; default follows --output.",
            ),
        ],
        examples: &[
            "arkforge help --all --format json",
            "arkforge help apply --format json",
        ],
        next: &["Follow one returned next_commands entry with exact identifiers."],
        exits: &[
            (0, "Help produced."),
            (2, "The topic or format is invalid."),
        ],
    },
];
