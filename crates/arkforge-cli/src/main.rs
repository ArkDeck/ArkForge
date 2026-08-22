//! Canonical ArkForge command frontend and local authority process.
//!
//! Explicit native rescue and read-only host diagnostics land here before the
//! normal-flash authority surface. No canonical command is a compatibility
//! wrapper around an older binary.

mod inference;
mod interaction;

use arkforge_artifact::cas::{CasError, CasQuota, ContentAddressedStore, ImportedObject};
use arkforge_client::{
    DeviceObservationView, DeviceProbeView, PublicClient, PublicClientError, RecoveryGuideView,
};
use arkforge_core::profile;
use arkforge_core::{OpaqueId, Sha256Digest, Version};
use arkforge_ipc::messages::{
    ActionReceiptSummary, Assessment, Effect, ExecutablePlan, InspectArtifactResponse, JobEvent,
    JobSummary, KeyValue, MaterializePlanResponse,
};
use arkforged::artifact_ops::{
    ProfileCoverage, inspect_container, manifest_response, profile_coverage,
};
use arkforged::dispatch::executable_digest;
use arkforged::packaging::{self, ContractMode, SignedCode};
use arkforged::rescue::{
    NativeRescueBackend, RescueApplyResult, RescueDevice, RescueError, RescueInspection,
    RescueManager, RescuePlanSummary, RescueReadReceipt, now_epoch_ms,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use arkforge_standalone::StandaloneError;
use arkforge_standalone::approval::{self, ApprovalRecord, Provenance};
use arkforge_standalone::config::{RuntimeConfig, pin};
use arkforge_standalone::supervisor;

/// Emits one structured document through [`emit`].
///
/// Same shape as `println!`, so a renderer reads the same; routing every
/// structured document through one place is what lets the autostart disclosure
/// be appended without every renderer remembering to.
macro_rules! emit_json {
    ($($argument:tt)*) => {
        emit(format!($($argument)*))
    };
}

const HELP_SCHEMA: &str = "arkforge.command-help/v1";
const HELP_INDEX_SCHEMA: &str = "arkforge.command-help-index/v1";
const STATUS_SCHEMA: &str = "arkforge.status/v1";

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
    no_auto_start: bool,
    no_input: bool,
}

#[derive(Debug)]
struct CliError {
    code: String,
    message: String,
    exit_code: i32,
    retryable: bool,
    required_acknowledgements: Vec<String>,
    /// What a few refusals carry beyond the v1 envelope. Boxed because almost
    /// none of them do, and every fallible function in this frontend returns
    /// this type by value.
    extras: Option<Box<ErrorExtras>>,
}

#[derive(Debug, Default)]
struct ErrorExtras {
    /// Composite facts already established before the refusal, rendered as one
    /// canonical JSON object body. It is the additive `facts` member of
    /// `arkforge.command-result/v1.error`; the v1 members keep their meaning.
    facts: Option<String>,
    /// The exact command that continues from here, when the refusal knows one
    /// better than a generic retry — most importantly, executing a plan that is
    /// already sealed rather than sealing a second one.
    retry_command: Option<String>,
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
            extras: None,
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new("INVALID_ARGUMENT", message, 2, false)
    }

    #[cfg(test)]
    fn with_required_acknowledgements(mut self, tokens: Vec<String>) -> Self {
        if !tokens.is_empty() {
            self.retryable = true;
        }
        self.required_acknowledgements = tokens;
        self
    }

    /// Attach the composite facts established before this refusal so a failure
    /// path carries the same information a success path would have returned.
    #[allow(dead_code)]
    fn with_facts(mut self, facts: impl Into<String>) -> Self {
        self.extras.get_or_insert_default().facts = Some(facts.into());
        self
    }

    /// Names the exact next command, and the tokens it will need.
    fn with_retry(
        mut self,
        command: impl Into<String>,
        required_acknowledgements: Vec<String>,
    ) -> Self {
        self.extras.get_or_insert_default().retry_command = Some(command.into());
        self.required_acknowledgements = required_acknowledgements;
        self
    }

    fn facts(&self) -> Option<&str> {
        self.extras.as_ref()?.facts.as_deref()
    }

    fn retry_command(&self) -> Option<&str> {
        self.extras.as_ref()?.retry_command.as_deref()
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
            extras: None,
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
            extras: None,
        }
    }
}

impl From<StandaloneError> for CliError {
    fn from(error: StandaloneError) -> Self {
        Self {
            code: error.code,
            message: error.message,
            exit_code: error.exit_code,
            retryable: error.retryable,
            required_acknowledgements: error.required_acknowledgements,
            extras: None,
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
                    || (name == "ack" && matches!(self.command, "apply" | "rescue apply"));
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
                let mut conflicts = match name.as_str() {
                    "quiet" => vec!["verbose".into()],
                    "verbose" => vec!["quiet".into()],
                    _ => Vec::new(),
                };
                if prose.contains("conflicts with") {
                    for token in description.split_whitespace() {
                        if let Some(conflict) = token.strip_prefix("--") {
                            let conflict = conflict.trim_end_matches(['.', ',', ';']);
                            if conflict != name {
                                conflicts.push(conflict.to_string());
                            }
                        }
                    }
                }
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

    /// Whether running this command may bring a local runtime into existence.
    /// It is deliberately separate from `effect`, which grades the business and
    /// device effect: a read-only query that starts a service is still not a
    /// device write, and reporting it as one would make `effect` useless.
    fn runtime_effect(&self) -> &'static str {
        match self.command {
            "daemon run" | "daemon start" => "may-start-service",
            // Every command that needs a paired runtime will bring one up
            // rather than refuse, unless --no-auto-start says otherwise.
            command
                if command.starts_with("device ")
                    || command.starts_with("flash ")
                    || matches!(command, "apply" | "watch" | "cancel")
                    || command.starts_with("job ") =>
            {
                "may-start-service"
            }
            _ => "none",
        }
    }

    /// The `facts` projections this command may attach to a refusal envelope,
    /// as `(name, schema, max_items)`. No command carries composite refusal
    /// facts yet; each composite surface declares its own as it lands.
    fn facts_projections(&self) -> &'static [(&'static str, &'static str, u64)] {
        match self.command {
            "flash plan" | "flash run" | "job recover" => &[
                ("flash_plan", "arkforge.flash-plan/v2", 1),
                ("device_candidates", "arkforge.resolved-device/v1", 32),
            ],
            _ => &[],
        }
    }

    /// Named operands this command accepts beside its options, as
    /// `(key, value-type)`. An empty value type means the operand is a bare key.
    ///
    /// A config assignment carries its own name, so accepting `hdc.path=/x` is
    /// not the parser guessing what an unnamed token means — the rule that
    /// nothing positional is inferred still holds, and an undeclared key is
    /// refused with the whole declared set named.
    fn operands(&self) -> &'static [(&'static str, &'static str)] {
        match self.command {
            "config set" => &[
                ("hdc.path", "absolute-path"),
                ("hdc.sha256", "sha256"),
                ("daemon.require-release-signing", "boolean"),
            ],
            "config unset" => &[("hdc", ""), ("daemon.require-release-signing", "")],
            "config add" => &[
                ("profile-file.path", "absolute-path"),
                ("profile-file.sha256", "sha256"),
            ],
            "config remove" => &[("profile-file.sha256", "sha256")],
            _ => &[],
        }
    }

    fn effect_class(&self) -> &'static str {
        match self.command {
            "apply" | "flash run" | "rescue apply" => "destructive",
            "cancel" => "mutating-control",
            "artifact import" | "rescue read" => "host-write",
            "flash plan" | "job recover" | "rescue plan" => "read-device-and-host-write",
            command if command.starts_with("config ") && command != "config show" => "host-write",
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
            "flash plan"
                | "flash run"
                | "apply"
                | "cancel"
                | "job reconcile"
                | "job recover"
                | "daemon stop"
        )
    }

    fn requires_daemon(&self) -> bool {
        self.command.starts_with("device ")
            || self.command.starts_with("flash ")
            || self.command.starts_with("job ")
            || matches!(self.command, "apply" | "watch" | "cancel" | "daemon stop")
    }
}

fn validate_against_command_tree(
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
    // A bare invocation answers "what is going on here", not "what could I
    // type": `arkforge help` is one keystroke away and still prints the tree.
    if command.is_empty() {
        return run_status(globals);
    }
    if command == ["--version"] || command == ["-V"] {
        match globals.output {
            Output::Human => println!("arkforge {}", env!("CARGO_PKG_VERSION")),
            Output::Json => emit_json!(
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
        // One rule for structured callers: help without a path is the whole
        // tree, whether it was asked for as `help` or as `--help`.
        if topic.is_empty() && globals.output == Output::Json {
            print_help_index(globals.output);
        } else {
            print_help(help_spec(&topic)?, globals.output);
        }
        return Ok(0);
    }
    validate_against_command_tree(
        &command,
        interaction::open_for(globals.output == Output::Human, globals.no_input),
    )?;
    match command[0].as_str() {
        "status" => {
            reject_extra(&command[1..], "status")?;
            run_status(globals)
        }
        "device" => run_device(&command[1..], globals),
        "artifact" => run_artifact(&command[1..], globals),
        "flash" => run_flash(&command[1..], globals),
        "apply" => run_apply(&command[1..], globals),
        "watch" => run_watch(&command[1..], globals),
        "cancel" => run_cancel(&command[1..], globals),
        "job" => run_job(&command[1..], globals),
        "rescue" => run_rescue(&command[1..], globals),
        "daemon" => run_daemon(&command[1..], globals),
        "config" => run_config(&command[1..], globals),
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
            let options = daemon_options(&runtime_dir, &arguments[1..])?;
            supervisor::run(
                runtime_dir,
                options,
                globals.output == Output::Human && !globals.quiet,
            )?;
            Ok(0)
        }
        "start" => {
            let options = daemon_options(&runtime_dir, &arguments[1..])?;
            let status = supervisor::start(runtime_dir, options)?;
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
                Output::Json => emit_json!(
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

/// The lifecycle options for this run: stored configuration, with explicit
/// arguments overriding it wherever they give a complete binding.
fn daemon_options(
    runtime_dir: &Path,
    arguments: &[String],
) -> Result<supervisor::DaemonOptions, CliError> {
    let config = RuntimeConfig::load(runtime_dir)?;
    config.verify_pins()?;
    Ok(supervisor::DaemonOptions::from_config(&config, None)
        .overridden_by(supervisor::DaemonOptions::parse(arguments)?))
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
        Output::Json => emit_json!(
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
        vec!["arkforge status".into()]
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
    let mut no_auto_start = false;
    let mut no_input = false;
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
            "--no-auto-start" => no_auto_start = true,
            "--no-input" => no_input = true,
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
            no_auto_start,
            no_input,
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

/// One aggregate host and runtime snapshot.
///
/// It deliberately never starts a runtime: a caller asking "what is going on"
/// must be able to learn "nothing is running" without that question changing
/// the answer. Every section separates three states that a bare array cannot —
/// not observable, observed and empty, observed and populated.
fn run_status(globals: Globals) -> Result<i32, CliError> {
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
        daemon_next_commands,
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
struct StatusSection {
    available: bool,
    reason: Option<String>,
    items: Option<Vec<String>>,
    lines: Vec<String>,
}

impl StatusSection {
    fn enumerated(items: Vec<String>, lines: Vec<String>) -> Self {
        Self {
            available: true,
            reason: None,
            items: Some(items),
            lines,
        }
    }

    fn unobservable(reason: impl Into<String>) -> Self {
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

    fn json(&self) -> String {
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

    fn print_human(&self, title: &str, empty: &str) {
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

fn run_completion(arguments: &[String], output: Output) -> Result<i32, CliError> {
    let options = Options::parse(arguments)?;
    let shell = options.one("shell")?;
    let script = completion_script(shell)?;
    match output {
        Output::Human => print!("{script}"),
        Output::Json => emit_json!(
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
            let selected = options.optional_one("device")?.map(str::to_string);
            let deep = options.flag("deep");
            ensure_runtime(&globals, None)?;
            let registry = profile_registry()?;
            let mut client = public_client(&globals)?;
            let mut observations = client.device_list()?;
            if let Some(selected) = &selected {
                observations.retain(|observation| observation.observation_id == *selected);
                if observations.is_empty() {
                    return Err(CliError::new(
                        "OBSERVATION_NOT_FOUND",
                        format!("No current observation has id {selected}."),
                        5,
                        false,
                    ));
                }
            }
            let mut entries = Vec::with_capacity(observations.len());
            for observation in &observations {
                entries.push(describe_device(&mut client, &registry, observation, deep)?);
            }
            print_device_list(globals.output, deep, selected.as_deref(), &entries);
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
            ensure_runtime(&globals, None)?;
            let probe = wait_for_device(&globals, profile, mode, timeout_ms)?;
            print_device_wait(globals.output, profile, mode, timeout_ms, &probe);
            Ok(0)
        }
        other => Err(CliError::invalid(format!(
            "Unknown device command {other:?}. Run 'arkforge help device'."
        ))),
    }
}

/// The profiles this build can reason about.
fn profile_registry() -> Result<inference::ProfileRegistry, CliError> {
    inference::ProfileRegistry::load()
        .map_err(|message| CliError::new("PROFILE_REJECTED", message, 10, false))
}

/// One observation with everything this build can say about it.
struct DeviceEntry {
    observation: DeviceObservationView,
    identification: inference::Identification,
    /// Facts an active probe returned, or `None` when no probe ran. An empty
    /// vector would claim a probe answered with nothing, which is a different
    /// fact from not having asked.
    probe_facts: Option<Vec<(String, String)>>,
    probe_refusals: Vec<(String, String)>,
}

/// Identifies one observation, optionally confirming it on the wire.
///
/// A deep pass probes every profile the passive evidence says the device could
/// be — not one guessed profile — so an ambiguous device produces evidence for
/// each candidate instead of a conclusion for one.
fn describe_device(
    client: &mut PublicClient,
    registry: &inference::ProfileRegistry,
    observation: &DeviceObservationView,
    deep: bool,
) -> Result<DeviceEntry, CliError> {
    let passive = inference::identify(registry, observation, None);
    if !deep {
        return Ok(DeviceEntry {
            observation: observation.clone(),
            identification: passive,
            probe_facts: None,
            probe_refusals: Vec::new(),
        });
    }

    let mut facts: Vec<(String, String)> = Vec::new();
    let mut refusals = Vec::new();
    let mut answered = false;
    for profile in &passive.compatible_profiles {
        match client.device_probe(&observation.observation_id, profile) {
            Ok(probe) => {
                answered = true;
                facts.extend(
                    probe
                        .protocol_facts
                        .iter()
                        .map(|fact| (fact.key.clone(), fact.value.clone())),
                );
            }
            Err(error)
                if matches!(
                    error.code.as_str(),
                    "PROBE_REFUSED" | "OBSERVATION_NOT_FOUND" | "NO_PROVIDER_FOR_PROFILE"
                ) =>
            {
                refusals.push((profile.clone(), error.code.clone()));
            }
            Err(error) => return Err(error.into()),
        }
    }
    facts.sort();
    facts.dedup();
    let identification = if answered {
        let borrowed = facts
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect::<Vec<_>>();
        inference::identify(registry, observation, Some(&borrowed))
    } else {
        passive
    };
    Ok(DeviceEntry {
        observation: observation.clone(),
        identification,
        probe_facts: answered.then_some(facts),
        probe_refusals: refusals,
    })
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
            // Importing is the moment the caller learns what they have. Making
            // them run three more queries to find out whether it matches the
            // board on their desk is the pipework this change exists to remove.
            let registry = profile_registry()?;
            let manifest = stored_manifest(&store, &imported.digest)?;
            let response = manifest_response(&manifest);
            let compatible = registry.compatible_with_format(&response.format_id);
            let present = present_matching_devices(&globals, &registry, &compatible)?;
            print_artifact_import(globals.output, &imported, &response, &compatible, &present);
            Ok(0)
        }
        "list" => {
            let registry = profile_registry()?;
            let store_root_exists = artifact_store_root(&globals)?.exists();
            let objects = list_artifacts(&globals)?;
            let mut items = Vec::with_capacity(objects.len());
            if store_root_exists && !objects.is_empty() {
                let store = open_existing_artifact_store(&globals)?;
                for (digest, size_bytes) in &objects {
                    items.push(match stored_manifest(&store, digest) {
                        Ok(manifest) => {
                            let response = manifest_response(&manifest);
                            StoredArtifact {
                                digest: *digest,
                                size_bytes: *size_bytes,
                                compatible_profiles: registry
                                    .compatible_with_format(&response.format_id),
                                format: Some(response.format_id),
                                unreadable_reason: None,
                            }
                        }
                        Err(error) => StoredArtifact {
                            digest: *digest,
                            size_bytes: *size_bytes,
                            format: None,
                            compatible_profiles: Vec::new(),
                            unreadable_reason: Some(error.code),
                        },
                    });
                }
            }
            print_artifact_list(globals.output, &items);
            Ok(0)
        }
        "show" => {
            let artifact_id = options.one("artifact")?;
            let digest = parse_digest("--artifact", artifact_id)?;
            let registry = profile_registry()?;
            let store = open_existing_artifact_store(&globals)?;
            let manifest = stored_manifest(&store, &digest)?;
            let response = manifest_response(&manifest);
            let coverage = options
                .optional_one("profile-file")?
                .map(|path| load_profile_coverage(Path::new(path), &manifest))
                .transpose()?;
            let compatible = registry.compatible_with_format(&response.format_id);
            print_artifact_inspection(
                globals.output,
                artifact_id,
                &response,
                coverage.as_ref(),
                &compatible,
            );
            Ok(0)
        }
        other => Err(CliError::invalid(format!(
            "Unknown artifact command {other:?}. Run 'arkforge help artifact'."
        ))),
    }
}

/// One stored object as the artifact list reports it. An object whose bytes do
/// not parse keeps its place with a null format and a typed reason rather than
/// disappearing from the listing.
struct StoredArtifact {
    digest: Sha256Digest,
    size_bytes: u64,
    format: Option<String>,
    compatible_profiles: Vec<String>,
    unreadable_reason: Option<String>,
}

/// Parses one stored object with the parser its framing selects.
fn stored_manifest(
    store: &ContentAddressedStore,
    digest: &Sha256Digest,
) -> Result<arkforge_artifact::manifest::ArtifactManifest, CliError> {
    let object = store.open_object(digest).map_err(artifact_store_error)?;
    inspect_container(object)
        .map_err(|message| CliError::new("ARTIFACT_REJECTED", message, 3, false))
}

/// Devices currently on this host that the given profiles could flash.
///
/// A runtime that is not running makes this unknown, not empty: reporting "no
/// matching device" when nothing was enumerated would be a lie with a plausible
/// shape.
fn present_matching_devices(
    globals: &Globals,
    registry: &inference::ProfileRegistry,
    artifact_profiles: &[String],
) -> Result<StatusSection, CliError> {
    if artifact_profiles.is_empty() {
        return Ok(StatusSection::enumerated(Vec::new(), Vec::new()));
    }
    let mut client = match public_client(globals) {
        Ok(client) => client,
        Err(error) => return Ok(StatusSection::unobservable(error.code)),
    };
    let observations = match client.device_list() {
        Ok(observations) => observations,
        Err(error) => return Ok(StatusSection::unobservable(error.code)),
    };
    let mut items = Vec::new();
    let mut lines = Vec::new();
    for observation in &observations {
        let identification = inference::identify(registry, observation, None);
        if !identification
            .compatible_profiles
            .iter()
            .any(|profile| artifact_profiles.contains(profile))
        {
            continue;
        }
        items.push(format!(
            "{{\"observation_id\":{},\"mode\":{},\"identification\":{}}}",
            json(&observation.observation_id),
            json(&observation.mode),
            identification.to_json(json)
        ));
        lines.push(format!(
            "{}  mode={}  profiles={}",
            observation.observation_id,
            observation.mode,
            identification.compatible_profiles.join(", ")
        ));
    }
    Ok(StatusSection::enumerated(items, lines))
}

fn run_flash(arguments: &[String], globals: Globals) -> Result<i32, CliError> {
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
        other => Err(CliError::invalid(format!(
            "Unknown flash command {other:?}. Run 'arkforge help flash'."
        ))),
    }
}

/// Reusable local bindings: what this host may drive, not what it may do.
fn run_config(arguments: &[String], globals: Globals) -> Result<i32, CliError> {
    let Some(subcommand) = arguments.first() else {
        print_help(help_spec(&["config".into()])?, globals.output);
        return Ok(0);
    };
    let runtime_dir = command_runtime_dir(&globals)?;
    let settings = Settings::parse(&arguments[1..])?;
    let mut config = RuntimeConfig::load(&runtime_dir)?;
    match subcommand.as_str() {
        "show" => {
            settings.reject_any("config show")?;
            print_config(globals.output, &config, &runtime_dir);
            return Ok(0);
        }
        "set" => {
            // A path and the digest of its bytes are one decision, so they are
            // one transaction: a config can never name a tool it has not pinned.
            match (settings.value("hdc.path"), settings.value("hdc.sha256")) {
                (Some(path), Some(expected)) => {
                    let pinned = pin(Path::new(path))?;
                    if pinned.sha256 != expected {
                        return Err(CliError::new(
                            "CONFIG_DIGEST_MISMATCH",
                            "The file at the configured path does not have the expected digest; nothing was stored."
                                .to_string(),
                            3,
                            false,
                        ));
                    }
                    config.hdc = Some(pinned);
                }
                (None, None) => {}
                _ => {
                    return Err(CliError::invalid(
                        "hdc.path and hdc.sha256 bind one tool together; supply both or neither.",
                    ));
                }
            }
            if let Some(value) = settings.value("daemon.require-release-signing") {
                config.require_release_signing = value == "true";
            }
            settings.require_any()?;
        }
        "unset" => {
            for key in settings.keys() {
                match key.as_str() {
                    "hdc" => config.hdc = None,
                    "daemon.require-release-signing" => config.require_release_signing = false,
                    other => {
                        return Err(CliError::invalid(format!(
                            "Unknown setting {other:?} to clear."
                        )));
                    }
                }
            }
            settings.require_any()?;
        }
        "add" => {
            let (Some(path), Some(expected)) = (
                settings.value("profile-file.path"),
                settings.value("profile-file.sha256"),
            ) else {
                return Err(CliError::invalid(
                    "profile-file.path and profile-file.sha256 bind one profile together; supply both.",
                ));
            };
            let pinned = pin(Path::new(path))?;
            if pinned.sha256 != expected {
                return Err(CliError::new(
                    "CONFIG_DIGEST_MISMATCH",
                    "The file at the configured path does not have the expected digest; nothing was stored.".to_string(),
                    3,
                    false,
                ));
            }
            if config
                .profile_files
                .iter()
                .any(|existing| existing.sha256 == pinned.sha256)
            {
                return Err(CliError::new(
                    "CONFIG_ALREADY_BOUND",
                    format!(
                        "A profile with digest {} is already configured.",
                        pinned.sha256
                    ),
                    6,
                    false,
                ));
            }
            config.profile_files.push(pinned);
            config
                .profile_files
                .sort_by(|left, right| left.sha256.cmp(&right.sha256));
        }
        "remove" => {
            let Some(expected) = settings.value("profile-file.sha256") else {
                return Err(CliError::invalid(
                    "Name the profile to remove by digest: profile-file.sha256=<sha256>.",
                ));
            };
            let before = config.profile_files.len();
            config
                .profile_files
                .retain(|profile| profile.sha256 != expected);
            if config.profile_files.len() == before {
                return Err(CliError::new(
                    "CONFIG_NOT_BOUND",
                    format!("No configured profile has digest {expected}."),
                    5,
                    false,
                ));
            }
        }
        other => {
            return Err(CliError::invalid(format!(
                "Unknown config command {other:?}. Run 'arkforge help config'."
            )));
        }
    }
    config.store(&runtime_dir)?;
    print_config(globals.output, &config, &runtime_dir);
    Ok(0)
}

/// The `key=value` operands one config command was given.
struct Settings {
    values: Vec<(String, Option<String>)>,
}

impl Settings {
    fn parse(arguments: &[String]) -> Result<Self, CliError> {
        let mut values = Vec::new();
        for argument in arguments {
            let (key, value) = match argument.split_once('=') {
                Some((key, value)) => (key.to_string(), Some(value.to_string())),
                None => (argument.clone(), None),
            };
            if values.iter().any(|(existing, _)| *existing == key) {
                return Err(CliError::invalid(format!(
                    "{key} may be supplied only once."
                )));
            }
            values.push((key, value));
        }
        Ok(Self { values })
    }

    fn value(&self, key: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(name, _)| name == key)
            .and_then(|(_, value)| value.as_deref())
    }

    fn keys(&self) -> Vec<String> {
        self.values.iter().map(|(key, _)| key.clone()).collect()
    }

    fn require_any(&self) -> Result<(), CliError> {
        if self.values.is_empty() {
            return Err(CliError::invalid(
                "Name at least one setting; run 'arkforge help config --format json' for the exact keys.",
            ));
        }
        Ok(())
    }

    fn reject_any(&self, command: &str) -> Result<(), CliError> {
        if self.values.is_empty() {
            return Ok(());
        }
        Err(CliError::invalid(format!(
            "{command} accepts no settings; unexpected {:?}.",
            self.values[0].0
        )))
    }
}

/// Reports what is bound without reporting where it lives.
///
/// Structured output carries binding state, digests and counts only. A host
/// path is not a secret to the owner reading their own terminal, but it is not
/// something an Agent transcript or a CI log should carry, and CLI-AC-04 draws
/// that line at the structured surface.
fn print_config(output: Output, config: &RuntimeConfig, runtime_dir: &Path) {
    let next = if config.hdc.is_some() {
        "arkforge status".to_string()
    } else {
        "arkforge config set hdc.path=<absolute-path> hdc.sha256=<sha256>".to_string()
    };
    match output {
        Output::Human => {
            println!("ArkForge configuration ({})", runtime_dir.display());
            match &config.hdc {
                Some(hdc) => {
                    println!("hdc");
                    println!("  path    {}", hdc.path.display());
                    println!("  sha256  {}", hdc.sha256);
                }
                None => println!("hdc      not bound"),
            }
            println!("profile files ({})", config.profile_files.len());
            for profile in &config.profile_files {
                println!("  {}  {}", profile.sha256, profile.path.display());
            }
            println!(
                "daemon.require-release-signing  {}",
                config.require_release_signing
            );
            println!("Campaigns are never stored; name one per call with --hardware-campaign.");
            println!("Next: {next}");
        }
        Output::Json => emit_json!(
            "{{\"schema\":\"arkforge.config/v1\",\"hdc\":{},\"profile_files\":[{}],\"profile_file_count\":{},\"daemon\":{{\"require_release_signing\":{}}},\"campaign_persistable\":false,\"next_commands\":[{}]}}",
            config
                .hdc
                .as_ref()
                .map(|hdc| format!("{{\"bound\":true,\"sha256\":{}}}", json(&hdc.sha256)))
                .unwrap_or_else(|| "{\"bound\":false,\"sha256\":null}".to_string()),
            config
                .profile_files
                .iter()
                .map(|profile| format!("{{\"sha256\":{}}}", json(&profile.sha256)))
                .collect::<Vec<_>>()
                .join(","),
            config.profile_files.len(),
            config.require_release_signing,
            json(&next)
        ),
    }
}

/// The general consent verb.
///
/// One command for every plan an authority sealed — a normal flash plan and a
/// superseding recovery plan alike — because what the caller is doing is the
/// same act: accepting a named set of destructive effects. Nothing here relaxes
/// the gate; the exact digest and the exact acknowledgement set are still the
/// only way through, and no broad `--yes` exists to add.
fn run_apply(arguments: &[String], globals: Globals) -> Result<i32, CliError> {
    let options = Options::parse(arguments)?;
    let plan_id = options.one("plan")?;
    // Rescue keeps its own consent surface on purpose. The canonical shape is
    // recognized here, before any authority store is read, so a rescue id can
    // never be resolved against the wrong domain even momentarily.
    if plan_id.starts_with(RESCUE_PLAN_PREFIX) {
        return Err(CliError::new(
            "RESCUE_PLAN_DOMAIN",
            format!(
                "{plan_id} is a rescue plan. Rescue keeps a separate plan, receipt, and evidence domain; apply it with 'arkforge rescue apply'."
            ),
            2,
            false,
        ));
    }
    let expected = options.one("expect-plan-sha256")?;
    parse_digest("--expect-plan-sha256", expected)?;
    let acknowledgements = options.many_required("ack")?.to_vec();
    let detach = options.flag("detach");
    let runtime_dir = command_runtime_dir(&globals)?;
    // Before dispatch, never after: a campaign mismatch must cost nothing.
    ensure_runtime(&globals, options.optional_one("hardware-campaign")?)?;
    let job_id =
        supervisor::apply_plan(&runtime_dir, plan_id, expected, &acknowledgements, detach)?;
    if detach {
        match globals.output {
            Output::Human => {
                println!("Started durable job {job_id}.");
                println!("The authority supervisor continues to drive it.");
                println!("Next: arkforge watch --job {job_id}");
            }
            Output::Json => emit_json!(
                "{{\"schema\":\"arkforge.apply/v1\",\"job_id\":{},\"detached\":true,\"authority_continues\":true,\"next_commands\":[{}]}}",
                json(&job_id),
                json(&format!("arkforge watch --job {job_id}")),
            ),
        }
        return Ok(0);
    }
    let (events, summary, _) = watch_job(&globals, &job_id, 0, u64::MAX)?;
    print_job_watch(&globals, &["apply"], 0, u64::MAX, &events, &summary, false);
    Ok(match summary.state.as_str() {
        "succeeded" => 0,
        "outcomeUnknown" => 8,
        "cancelledSafe" => 7,
        _ if summary.terminal => 7,
        _ => 9,
    })
}

/// The canonical rescue plan identifier shape.
const RESCUE_PLAN_PREFIX: &str = "rescue-plan:";

/// Follows one job's durable events, defaulting to the one that is running.
fn run_watch(arguments: &[String], globals: Globals) -> Result<i32, CliError> {
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
    ensure_runtime(&globals, None)?;
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
fn run_cancel(arguments: &[String], globals: Globals) -> Result<i32, CliError> {
    let options = Options::parse(arguments)?;
    let job_id = options.one("job")?;
    let expected_sequence = parse_u64("--expect-sequence", options.one("expect-sequence")?)?;
    ensure_runtime(&globals, None)?;
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
        hardware_campaign: partial.sealed_campaign.clone(),
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
    let (events, summary, _) = watch_job(&globals, &job_id, 0, u64::MAX)?;
    print_job_watch(
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
    let assess_only = options.flag("assess-only");
    let registry = profile_registry()?;
    let runtime_dir = command_runtime_dir(&globals)?;
    ensure_runtime(&globals, options.optional_one("hardware-campaign")?)?;
    let resolved = resolve_plan_inputs(&globals, &registry, options, !assess_only, None)?;
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
    let materialized = supervisor::materialize_plan(
        &runtime_dir,
        &resolved.artifact.artifact_id,
        &resolved.profile.reference,
        &resolved.device.observation.observation_id,
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
fn sealed_campaign(plan: &ExecutablePlan) -> Option<String> {
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
fn require_campaign_acknowledgement(
    runtime_dir: &Path,
    given: Option<&str>,
) -> Result<(), CliError> {
    let running = supervisor::status(runtime_dir)
        .ok()
        .map(|status| status.hardware_campaign)
        .filter(|campaign| !campaign.is_empty());
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

struct ResolvedArtifact {
    artifact_id: String,
    manifest: InspectArtifactResponse,
    imported: bool,
    compatible_profiles: Vec<String>,
}

struct ResolvedProfile {
    reference: String,
    resolution: &'static str,
}

struct ResolvedIntent {
    value: String,
    resolution: &'static str,
}

struct Resolved {
    artifact: ResolvedArtifact,
    device: inference::Candidate,
    profile: ResolvedProfile,
    intent: ResolvedIntent,
}

/// What resolution had established when it finished — or when it gave up.
///
/// A refusal carries this so the failure path returns the same facts the
/// success path would have, and the caller does not have to re-query to learn
/// how far the command got.
#[derive(Default)]
struct PartialResolution {
    artifact: Option<String>,
    device: Option<String>,
    profile: Option<String>,
    intent: Option<String>,
    candidates: Vec<String>,
    sealed_campaign: Option<String>,
    /// Tokens the plan's origin requires beyond its own effects.
    extra_acknowledgements: Vec<String>,
}

impl PartialResolution {
    fn from_resolved(resolved: &Resolved) -> Self {
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

    fn refuse(
        &self,
        code: &str,
        message: impl Into<String>,
        exit_code: i32,
        retryable: bool,
    ) -> CliError {
        self.refuse_with(code, message, exit_code, retryable, None, None)
    }

    fn refuse_with(
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
        manifest_summary_json(&artifact.manifest),
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

fn resolve_plan_inputs(
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
    let manifest = manifest_response(&stored_manifest(&store, &digest)?);
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
            steps_json(&assessment.would_be_steps),
            effects_json(&assessment.known_persistent_effects),
            key_values_json(&assessment.data_impact),
            key_values_json(&assessment.unknowns),
            key_values_json(&assessment.evidence_requirements),
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
                steps_json(&plan.public_steps),
                effects_json(&plan.persistent_effects),
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

fn print_flash_plan_v2(
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
                print_effects_human(
                    "known persistent effects",
                    &assessment.known_persistent_effects,
                );
                print_key_values_human("data impact", &assessment.data_impact);
                print_key_values_human("unknowns", &assessment.unknowns);
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

fn run_job(arguments: &[String], globals: Globals) -> Result<i32, CliError> {
    let Some(subcommand) = arguments.first() else {
        print_help(help_spec(&["job".into()])?, globals.output);
        return Ok(0);
    };
    ensure_runtime(&globals, None)?;
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

    let resolved = resolve_plan_inputs(&globals, &registry, &options, true, None)?;
    let mut partial = PartialResolution::from_resolved(&resolved);
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
        MaterializePlanResponse::Assessment(assessment) => Err(partial.refuse_with(
            "RECOVERY_PLAN_UNAVAILABLE",
            format!(
                "No superseding plan was created for {job_id}: {}",
                if assessment.unavailable_reason.is_empty() {
                    assessment.availability.clone()
                } else {
                    assessment.unavailable_reason.clone()
                }
            ),
            3,
            false,
            Some(&assessment),
            None,
        )),
    }
}

fn public_client(globals: &Globals) -> Result<PublicClient, CliError> {
    let runtime_dir = command_runtime_dir(globals)?;
    PublicClient::connect(&runtime_dir).map_err(Into::into)
}

/// Whether this process had to bring the runtime up to answer.
///
/// Recorded once here rather than threaded through every renderer, so no
/// command can answer without disclosing that it started a service.
static RUNTIME_AUTOSTARTED: AtomicBool = AtomicBool::new(false);

/// Makes a runtime exist, or explains why this call will not make one.
///
/// Starting a service to answer a question is a real effect, so it is opt-out
/// (`--no-auto-start`) and disclosed in the result. Serializing concurrent
/// starters and refusing to take over a paired supervisor belong to the
/// lifecycle layer, which owns both.
fn ensure_runtime(globals: &Globals, hardware_campaign: Option<&str>) -> Result<(), CliError> {
    let runtime_dir = command_runtime_dir(globals)?;
    if supervisor::status(&runtime_dir).is_ok() {
        return require_campaign_acknowledgement(&runtime_dir, hardware_campaign);
    }
    if globals.no_auto_start {
        return Err(CliError::new(
            "DAEMON_UNAVAILABLE",
            "No CLI authority supervisor is listening and --no-auto-start forbids starting one.",
            5,
            true,
        ));
    }
    let config = RuntimeConfig::load(&runtime_dir)?;
    config.verify_pins()?;
    let options = supervisor::DaemonOptions::from_config(&config, hardware_campaign);
    if supervisor::ensure_started(&runtime_dir, options)? {
        RUNTIME_AUTOSTARTED.store(true, Ordering::Relaxed);
        if globals.output == Output::Human && !globals.quiet {
            println!("Started the ArkForge runtime for this command.");
        }
        return Ok(());
    }
    // Another command created it first; it still has to be the campaign runtime
    // this call named.
    require_campaign_acknowledgement(&runtime_dir, hardware_campaign)
}

/// Emits one structured document on stdout.
///
/// The autostart disclosure is appended here, at the single outermost boundary,
/// to a JSON object this process just built.
fn emit(document: String) {
    if RUNTIME_AUTOSTARTED.load(Ordering::Relaxed)
        && let Some(body) = document.strip_suffix('}')
    {
        println!("{body},\"runtime_autostarted\":true}}");
        return;
    }
    println!("{document}");
}

fn artifact_store_root(globals: &Globals) -> Result<PathBuf, CliError> {
    let runtime_dir = match &globals.runtime_dir {
        Some(path) => path.clone(),
        None => default_runtime_dir()?,
    };
    Ok(runtime_dir.join("store"))
}

fn open_artifact_store(globals: &Globals) -> Result<ContentAddressedStore, CliError> {
    let runtime_dir = command_runtime_dir(globals)?;
    supervisor::prepare_storage(&runtime_dir)?;
    ContentAddressedStore::open(runtime_dir.join("store"), CasQuota::dayu200_default())
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
            emit_json!(
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

/// The one device query surface: enumeration, single-device filtering, and
/// active confirmation are the same answer at three depths, not three commands
/// whose outputs a caller has to reconcile.
fn print_device_list(output: Output, deep: bool, selected: Option<&str>, entries: &[DeviceEntry]) {
    let next = entries
        .iter()
        .find(|entry| entry.identification.profile.is_some())
        .map(|entry| {
            format!(
                "arkforge flash plan --artifact <artifact-id> --device {} --intent full-restore",
                entry.observation.observation_id
            )
        })
        .unwrap_or_else(|| {
            if deep {
                "arkforge status".to_string()
            } else {
                "arkforge device list --deep".to_string()
            }
        });
    match output {
        Output::Human => {
            if entries.is_empty() {
                println!("No device observations are available.");
                println!(
                    "Connect a supported device and make sure the ArkForge runtime is running."
                );
                println!("Next: arkforge device list");
                return;
            }
            println!("Device observations ({})", entries.len());
            for entry in entries {
                let identification = &entry.identification;
                println!(
                    "{}  mode={}  identity={}",
                    entry.observation.observation_id,
                    entry.observation.mode,
                    entry.observation.identity_strength
                );
                println!(
                    "  model                {}",
                    identification.model.as_deref().unwrap_or("unproven")
                );
                println!(
                    "  compatible profiles  {}",
                    if identification.compatible_profiles.is_empty() {
                        "none".to_string()
                    } else {
                        identification.compatible_profiles.join(", ")
                    }
                );
                println!(
                    "  resolution           {}",
                    identification.profile_resolution
                );
                println!(
                    "  strength             {}",
                    identification.strength.as_str()
                );
                println!(
                    "  evidence             {}",
                    identification.evidence.join(", ")
                );
                if entry.observation.malformed_descriptor {
                    println!("  descriptor           malformed");
                }
                if let Some(facts) = &entry.probe_facts {
                    println!("  probe facts ({})", facts.len());
                    for (key, value) in facts {
                        println!("    {key} = {value}");
                    }
                }
                for (profile, code) in &entry.probe_refusals {
                    println!("  probe refused        {profile}: {code}");
                }
            }
            println!("Next: {next}");
        }
        Output::Json => {
            let values = entries
                .iter()
                .map(device_entry_json)
                .collect::<Vec<_>>()
                .join(",");
            emit_json!(
                "{{\"schema\":\"arkforge.device-list/v1\",\"deep\":{deep},\"filtered_to\":{},\"observations\":[{values}],\"next_commands\":[{}]}}",
                optional_json(selected),
                json(&next)
            );
        }
    }
}

fn device_entry_json(entry: &DeviceEntry) -> String {
    let probe = match &entry.probe_facts {
        None => "null".to_string(),
        Some(facts) => {
            let facts = facts
                .iter()
                .map(|(key, value)| format!("{{\"key\":{},\"value\":{}}}", json(key), json(value)))
                .collect::<Vec<_>>()
                .join(",");
            let refusals = entry
                .probe_refusals
                .iter()
                .map(|(profile, code)| {
                    format!("{{\"profile\":{},\"code\":{}}}", json(profile), json(code))
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{\"facts\":[{facts}],\"refusals\":[{refusals}]}}")
        }
    };
    format!(
        "{{{},\"identification\":{},\"probe\":{}}}",
        observation_fields_json(&entry.observation),
        entry.identification.to_json(json),
        probe
    )
}

fn print_device_wait(
    output: Output,
    requested_profile: &str,
    requested_mode: &str,
    timeout_ms: u64,
    probe: &DeviceProbeView,
) {
    let next = format!(
        "arkforge flash plan --file <firmware-file> --profile {} --device {}",
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
        Output::Json => emit_json!(
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

/// One import answers the whole staging question: what landed in the store,
/// what it parses as, which profiles declare that format, and which of the
/// devices on this host could take it.
fn print_artifact_import(
    output: Output,
    imported: &ImportedObject,
    manifest: &InspectArtifactResponse,
    compatible: &[String],
    present: &StatusSection,
) {
    let artifact_id = imported.digest.to_hex();
    let next = match (compatible.first(), present.items.as_ref()) {
        (Some(profile), Some(items)) if !items.is_empty() => format!(
            "arkforge flash plan --artifact {artifact_id} --profile {profile} --device <observation-id> --intent full-restore"
        ),
        _ => format!("arkforge artifact show --artifact {artifact_id}"),
    };
    match output {
        Output::Human => {
            println!("Artifact imported into the content-addressed store.");
            println!("artifact_id   {artifact_id}");
            println!("sha256        {artifact_id}");
            println!("size_bytes    {}", imported.size_bytes);
            println!("deduplicated  {}", imported.deduplicated);
            println!("format        {}", manifest.format_id);
            println!("confidence    {}", manifest.confidence);
            println!("members       {}", manifest.members.len());
            println!("partitions    {}", manifest.partitions.len());
            println!(
                "compatible profiles  {}",
                if compatible.is_empty() {
                    "none".to_string()
                } else {
                    compatible.join(", ")
                }
            );
            present.print_human(
                "matching devices present",
                "None of the connected devices declare a compatible profile.",
            );
            println!("No device was accessed or mutated.");
            println!("Next: {next}");
        }
        Output::Json => emit_json!(
            "{{\"schema\":\"arkforge.artifact-import/v1\",\"artifact_id\":{},\"sha256\":{},\"size_bytes\":{},\"deduplicated\":{},\"host_store_mutated\":{},\"device_accessed\":false,\"manifest_summary\":{},\"compatible_profiles\":{},\"present_devices\":{},\"next_commands\":[{}]}}",
            json(&artifact_id),
            json(&artifact_id),
            imported.size_bytes,
            imported.deduplicated,
            !imported.deduplicated,
            manifest_summary_json(manifest),
            json_strings(compatible),
            present.json(),
            json(&next)
        ),
    }
}

/// A bounded manifest projection. The full member and partition tables belong
/// to `artifact show`; repeating them in every composite document would make
/// the composites grow without bound.
fn manifest_summary_json(manifest: &InspectArtifactResponse) -> String {
    format!(
        "{{\"format_id\":{},\"content_sha256\":{},\"manifest_sha256\":{},\"size_bytes\":{},\"confidence\":{},\"member_count\":{},\"partition_count\":{},\"unclassified_member_count\":{},\"execution_relevant_unknown_count\":{}}}",
        json(&manifest.format_id),
        json(&manifest.content_sha256),
        json(&manifest.manifest_sha256),
        manifest.size_bytes,
        json(&manifest.confidence),
        manifest.members.len(),
        manifest.partitions.len(),
        manifest.unclassified_members.len(),
        manifest.execution_relevant_unknowns.len(),
    )
}

fn print_artifact_list(output: Output, items: &[StoredArtifact]) {
    let next = if items.is_empty() {
        "arkforge artifact import --file <firmware-file>".to_string()
    } else {
        "arkforge artifact show --artifact <artifact-id>".to_string()
    };
    match output {
        Output::Human => {
            if items.is_empty() {
                println!("No artifacts are stored in this runtime.");
                println!("Next: {next}");
                return;
            }
            println!("Stored artifacts ({})", items.len());
            for item in items {
                println!("{}  size_bytes={}", item.digest.to_hex(), item.size_bytes);
                match (&item.format, &item.unreadable_reason) {
                    (Some(format), _) => {
                        println!("  format               {format}");
                        println!(
                            "  compatible profiles  {}",
                            if item.compatible_profiles.is_empty() {
                                "none".to_string()
                            } else {
                                item.compatible_profiles.join(", ")
                            }
                        );
                    }
                    (None, Some(reason)) => {
                        println!("  format               unreadable ({reason})")
                    }
                    (None, None) => println!("  format               unreadable"),
                }
            }
            println!("Next: {next}");
        }
        Output::Json => {
            let artifacts = items
                .iter()
                .map(|item| {
                    format!(
                        "{{\"artifact_id\":{},\"sha256\":{},\"size_bytes\":{},\"format\":{},\"compatible_profiles\":{},\"unreadable_reason\":{}}}",
                        json(&item.digest.to_hex()),
                        json(&item.digest.to_hex()),
                        item.size_bytes,
                        optional_json(item.format.as_deref()),
                        json_strings(&item.compatible_profiles),
                        optional_json(item.unreadable_reason.as_deref())
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            emit_json!(
                "{{\"schema\":\"arkforge.artifact-list/v1\",\"artifacts\":[{artifacts}],\"next_commands\":[{}]}}",
                json(&next)
            );
        }
    }
}

fn print_artifact_inspection(
    output: Output,
    artifact_id: &str,
    manifest: &InspectArtifactResponse,
    coverage: Option<&ProfileCoverage>,
    compatible: &[String],
) {
    let next = match compatible.first() {
        Some(profile) => format!(
            "arkforge flash plan --artifact {artifact_id} --profile {profile} --device <observation-id> --intent full-restore"
        ),
        None => format!(
            "arkforge flash plan --artifact {artifact_id} --profile <profile-id> --device <observation-id> --intent full-restore"
        ),
    };
    match output {
        Output::Human => {
            print_artifact_human(artifact_id, manifest);
            println!(
                "compatible profiles  {}",
                if compatible.is_empty() {
                    "none".to_string()
                } else {
                    compatible.join(", ")
                }
            );
            if let Some(coverage) = coverage {
                print_profile_coverage_human(coverage);
            }
            println!("No device was accessed or mutated.");
            println!("Next: {next}");
        }
        Output::Json => emit_json!(
            "{{\"schema\":\"arkforge.artifact-inspection/v1\",{},\"compatible_profiles\":{},\"profile_coverage\":{},\"device_accessed\":false,\"next_commands\":[{}]}}",
            artifact_fields_json(artifact_id, manifest),
            json_strings(compatible),
            coverage
                .map(profile_coverage_json)
                .unwrap_or_else(|| "null".into()),
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
    format!("{{{}}}", observation_fields_json(observation))
}

/// The observation members without their enclosing braces, so a composite
/// document can inline them beside its own members instead of restating the
/// field list and letting the two drift.
fn observation_fields_json(observation: &DeviceObservationView) -> String {
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
            if is_boolean_option(option) {
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

    /// Whether a value-less flag was supplied.
    fn flag(&self, name: &str) -> bool {
        self.values.contains_key(name)
    }

    /// Every value supplied for one option.
    fn many(&self, name: &str) -> &[String] {
        self.values.get(name).map_or(&[], Vec::as_slice)
    }

    /// Records a value the frontend resolved rather than the caller typed.
    fn insert(&mut self, name: &str, value: String) {
        self.values.insert(name.to_string(), vec![value]);
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

/// Whether this option takes no value, decided by the same typed tree that
/// generates help and completion. Asking the tree rather than keeping a second
/// list is what keeps the parser and the published contract from drifting.
fn is_boolean_option(name: &str) -> bool {
    HELP.iter()
        .flat_map(HelpSpec::typed_options)
        .any(|option| option.name == name && option.value_type == "boolean")
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
    #[cfg(target_os = "windows")]
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        return Ok(PathBuf::from(local_app_data).join("ArkForge"));
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
            emit_json!(
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
            emit_json!(
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
        Output::Json => emit_json!(
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
            emit_json!(
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
        Output::Json => emit_json!(
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
    let exact_retry = error
        .retry_command()
        .map(str::to_string)
        .or_else(|| acknowledgement_retry_command(arguments, error));
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
        Output::Json => emit_json!(
            "{{\"schema\":\"arkforge.command-result/v1\",\"ok\":false,\"command\":{},\"error\":{{\"code\":{},\"message\":{},\"remediation\":{},\"retryable\":{},\"required_acknowledgements\":{},\"next_commands\":{},\"facts\":{}}}}}",
            json_strings(command),
            json(&error.code),
            json(&structured_message),
            json(remediation_text),
            error.retryable,
            json_strings(&error.required_acknowledgements),
            json_strings(&next_commands),
            error.facts().unwrap_or("null"),
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
            "--no-color" | "--quiet" | "--verbose" | "--no-auto-start" | "--no-input" => index += 1,
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
            "--no-color" | "--quiet" | "--verbose" | "--no-auto-start" | "--no-input" => index += 1,
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
        "ARTIFACT_NOT_FOUND" | "ARTIFACT_NOT_INSPECTED" => Some("arkforge artifact list"),
        "ARTIFACT_FILE_NOT_FOUND" => Some("arkforge help artifact import --format json"),
        "ARTIFACT_IMPORT_REFUSED" | "ARTIFACT_STORE_FAILED" => Some("arkforge artifact list"),
        "ARTIFACT_REJECTED" => Some("arkforge help artifact show --format json"),
        "PROFILE_FILE_NOT_FOUND" | "PROFILE_REJECTED" => {
            Some("arkforge help artifact show --format json")
        }
        "OBSERVATION_NOT_FOUND" => Some("arkforge device list"),
        "CONTENT_REQUIRED" => Some("arkforge artifact list"),
        "DEVICE_AMBIGUOUS" | "IDENTITY_CONFIRMATION_REQUIRED" => {
            Some("arkforge device list --deep")
        }
        "PROFILE_AMBIGUOUS" | "PROFILE_INCOMPATIBLE" => Some("arkforge device list --deep"),
        "INTENT_REQUIRED" | "INTENT_UNAVAILABLE" => Some("arkforge help flash plan --format json"),
        "RUNTIME_CAMPAIGN_MISMATCH" => Some("arkforge status"),
        "DEVICE_WAIT_TIMEOUT" | "AMBIGUOUS_DEVICE" => Some("arkforge device list"),
        "PROFILE_NOT_FOUND" | "NO_PROVIDER_FOR_PROFILE" => Some("arkforge device list --deep"),
        "PLAN_UNAVAILABLE"
        | "RECOVERY_PLAN_UNAVAILABLE"
        | "AUTHORITY_SUPPORT_UNAVAILABLE"
        | "AUTHORITY_SUPPORT_SEAL_MISMATCH"
        | "MECHANICS_MATURITY_KEY_INVALID"
        | "HDC_BINDING_REFUSED"
        | "HDC_DIGEST_MISMATCH" => Some("arkforge status"),
        "MECHANICS_RUNTIME_CHANGED" => Some("arkforge help flash plan --format json"),
        "PLAN_DIGEST_MISMATCH" | "UNEXPECTED_ACKNOWLEDGEMENT" => {
            Some("arkforge help apply --format json")
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

fn print_help_index(output: Output) {
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
    if cfg!(target_os = "windows") {
        "[\"windows\"]"
    } else {
        "[\"macos\"]"
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

static HELP: &[HelpSpec] = &[
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
        usage: "arkforge device list [--device <observation-id>] [--deep]",
        effect: "Read-only discovery through runtime-dir/public.sock. --deep additionally probes every candidate profile. No observation is selected and no device is mutated.",
        requires: &["A running ArkForge runtime."],
        produces: &[
            "arkforge.device-list/v1 observations, each with an identification block reporting compatible profiles and physical model separately, with evidence and strength.",
        ],
        options: &[
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
        usage: "arkforge flash <run|plan> [options]",
        effect: "Run performs the whole flash behind one consent gate; plan stops after sealing, and the top-level apply executes what it sealed.",
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
        usage: "arkforge watch [--job <job-id>] [--after-sequence <u64>] [--timeout-ms <u64>]",
        effect: "Read-only polling of durable events and point-in-time status. Timeout ends only this observation; it never cancels or changes the job.",
        requires: &[
            "A running ArkForge runtime, and a resume sequence no greater than the durable last_sequence.",
        ],
        produces: &[
            "arkforge.job-watch/v1, arkforge.job-event/v1, and arkforge.command-result/v1 with strictly ordered typed events, terminal/timed-out status, and an exact resume command.",
        ],
        options: &[
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
        usage: "arkforge cancel --job <job-id> --expect-sequence <u64>",
        effect: "Mutating control only. It requests a safe stop at an optimistic sequence; it never edits the journal, replays an action, or reclassifies an outcome.",
        requires: &["A durable job and the exact expected last sequence."],
        produces: &[
            "arkforge.job-cancellation/v1 with one of four typed dispositions and no automatic replay.",
        ],
        options: &[
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
        summary: "Show one durable job with its event tail, receipts, and recovery block.",
        usage: "arkforge job show --job <job-id>",
        effect: "Read-only point-in-time status. It neither waits for events nor changes the job.",
        requires: &["One exact job id from job list or a prior command result."],
        produces: &[
            "arkforge.job/v1 with plan binding, state, progress, the last 20 durable events, every action receipt, and the no-replay recovery block. Each embedded section reports its own availability, so an unreadable one is never rendered as an absent one.",
        ],
        options: &[("--job <job-id>", "Exact durable job id; required.")],
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
        usage: "arkforge job reconcile --job <job-id>",
        effect: "Read-only controller assessment. It never dispatches, resumes, edits, or reclassifies the original job optimistically.",
        requires: &["A durable job in the selected paired runtime."],
        produces: &[
            "arkforge.job-reconciliation/v1 with immutable original state, possible-effect completeness, effects, and verdict.",
        ],
        options: &[("--job <job-id>", "Exact durable job; required.")],
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
        summary: "Seal a distinct plan that supersedes one unresolved job.",
        usage: "arkforge job recover --job <job-id> (--file <firmware-file> | --artifact <artifact-id>) [--device <observation-id> | --target <selector>] [--profile <id@version>] [--intent <full-restore>] [--hardware-campaign <campaign-id>] [--wait-device <u64>]",
        effect: "Reads the exact device through the paired runtime and stores a sealed host object. It never resumes, edits, replays, or reclassifies the original job, whose outcome and journal stay exactly as they are.",
        requires: &[
            "One exact durable job whose recovery contract covers every possible effect.",
            "Firmware content, and enough evidence to name exactly one device and profile.",
        ],
        produces: &[
            "arkforge.flash-plan/v2 whose plan supersedes the named job under a new epoch and intent, and whose apply_command carries the recovery:supersedes-job token as well as the effect tokens. The assessment section projects the same target's effects and gate state.",
        ],
        options: &[
            ("--job <job-id>", "Exact unresolved durable job; required."),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_next_commands_do_not_loop_on_authority_blockers() {
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
            validate_against_command_tree_for_test(&strings(&[
                "rescue", "inspect", "--device", "a", "--device", "b"
            ]))
            .is_err()
        );
        assert!(
            validate_against_command_tree_for_test(&strings(&[
                "rescue",
                "list",
                "--backend",
                "external"
            ]))
            .is_err()
        );
    }

    #[test]
    fn help_tree_has_every_implemented_leaf_and_agent_fields() {
        for topic in [
            "status",
            "device list",
            "device wait",
            "artifact import",
            "artifact list",
            "artifact show",
            "flash run",
            "flash plan",
            "apply",
            "watch",
            "cancel",
            "job list",
            "job show",
            "job reconcile",
            "job recover",
            "rescue list",
            "rescue inspect",
            "rescue read",
            "rescue plan",
            "rescue apply",
            "daemon run",
            "daemon start",
            "daemon stop",
            "config show",
            "config set",
            "config unset",
            "config add",
            "config remove",
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
                "status",
                "device",
                "artifact",
                "flash",
                "apply",
                "watch",
                "cancel",
                "job",
                "rescue",
                "daemon",
                "config",
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
            vec!["job list", "job show", "job reconcile", "job recover"]
        );

        assert_eq!(child_specs("signing")[0].command, "signing verify");
        assert_eq!(HELP_SCHEMA, "arkforge.command-help/v1");
    }

    #[test]
    fn every_example_parses_from_the_same_typed_tree_without_io() {
        // Every refusal is collected rather than the first one panicking: a
        // host-sensitive example is rarely alone, and one CI round on the other
        // platform should name all of them at once.
        let mut refused = Vec::new();
        for spec in HELP {
            for example in spec.examples {
                let words = example
                    .split_whitespace()
                    .map(example_fixture)
                    .collect::<Vec<_>>();
                assert_eq!(words.first().map(String::as_str), Some("arkforge"));
                if let Err(error) = parse_only(&words[1..]) {
                    refused.push(format!(
                        "example for {:?} did not parse: {example:?}: {}: {}",
                        spec.command, error.code, error.message
                    ));
                }
            }
        }
        assert!(
            refused.is_empty(),
            "{} published example(s) do not parse on this host:\n{}",
            refused.len(),
            refused.join("\n")
        );
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
                validate_against_command_tree_for_test(&strings(&[
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
            validate_against_command_tree_for_test(&strings(&[
                "device",
                "list",
                "--device",
                "<observation-id>"
            ]))
            .is_err()
        );
    }

    #[test]
    fn typed_relations_refuse_ambiguous_or_incomplete_effect_inputs() {
        assert!(
            validate_against_command_tree_for_test(&strings(&[
                "daemon", "start", "--hdc", "/opt/hdc"
            ]))
            .is_err()
        );
        assert!(
            validate_against_command_tree_for_test(&strings(&[
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
            validate_against_command_tree_for_test(&strings(&[
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
            validate_against_command_tree_for_test(&strings(&[
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
            validate_against_command_tree_for_test(&strings(&[
                "rescue",
                "list",
                "--backend",
                "rkdeveloptool"
            ]))
            .is_err()
        );
        assert!(parse_globals(&strings(&["--quiet", "--verbose", "status"])).is_err());
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
        validate_against_command_tree_for_test(&strings(&[
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
            validate_against_command_tree_for_test(&strings(&[
                "signing",
                "verify",
                "--release",
                "true"
            ]))
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

    /// The tree check as a script sees it: no terminal, so no positionals.
    fn validate_against_command_tree_for_test(arguments: &[String]) -> Result<(), CliError> {
        validate_against_command_tree(arguments, false)
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
                if command[index] == "--all" {
                    index += 1;
                } else if command[index] == "--format" {
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
        validate_against_command_tree(&command, false)
    }

    fn example_fixture(word: &str) -> String {
        if !word.contains('<') {
            return word.to_string();
        }
        // A `key=<placeholder>` operand keeps its key; only the placeholder is
        // a stand-in for a real value.
        if let Some((key, placeholder)) = word.split_once('=') {
            return format!("{key}={}", example_fixture(placeholder));
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
            // A fixture rooted at `/` stands in for a binding on macOS and for a
            // refusal on Windows, so the stand-in follows the host too.
            return if cfg!(target_os = "windows") {
                r"C:\arkforge-fixture".into()
            } else {
                "/tmp/arkforge-fixture".into()
            };
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
