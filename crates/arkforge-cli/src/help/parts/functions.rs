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

