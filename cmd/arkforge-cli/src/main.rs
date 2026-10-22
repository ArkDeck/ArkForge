//! Canonical ArkForge command frontend and local authority process.
//!
//! Explicit native rescue and read-only host diagnostics land here before the
//! normal-flash authority surface. No canonical command is a compatibility
//! wrapper around an older binary.

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

mod apply;
mod artifact;
mod completion;
mod config;
mod daemon;
mod device;
mod flash;
mod help;
mod inference;
mod interaction;
mod job;
mod rescue;
mod signing;
mod status;

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
            "artifact import" | "artifact preboot-ramdisk" | "rescue read" => "host-write",
            "flash plan"
            | "flash restore-ramdisk"
            | "flash verify"
            | "job recover"
            | "rescue plan" => "read-device-and-host-write",
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
                | "flash restore-ramdisk"
                | "flash run"
                | "flash verify"
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
        return status::run_status(globals);
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
        return help::run_help(&command[1..], globals.output);
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
            help::print_help_index(globals.output);
        } else {
            help::print_help(help::help_spec(&topic)?, globals.output);
        }
        return Ok(0);
    }
    help::validate_against_command_tree(
        &command,
        interaction::open_for(globals.output == Output::Human, globals.no_input),
    )?;
    match command[0].as_str() {
        "status" => {
            reject_extra(&command[1..], "status")?;
            status::run_status(globals)
        }
        "device" => device::run_device(&command[1..], globals),
        "artifact" => artifact::run_artifact(&command[1..], globals),
        "flash" => flash::run_flash(&command[1..], globals),
        "apply" => apply::run_apply(&command[1..], globals),
        "watch" => job::run_watch(&command[1..], globals),
        "cancel" => job::run_cancel(&command[1..], globals),
        "job" => job::run_job(&command[1..], globals),
        "rescue" => rescue::run_rescue(&command[1..], globals),
        "daemon" => daemon::run_daemon(&command[1..], globals),
        "config" => config::run_config(&command[1..], globals),
        "signing" => signing::run_signing(&command[1..], globals.output),
        "completion" => completion::run_completion(&command[1..], globals.output),
        other => Err(CliError::invalid(format!(
            "Unknown command {other:?}. Run 'arkforge help' for the command tree."
        ))),
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

/// The profiles this build can reason about.
fn profile_registry() -> Result<inference::ProfileRegistry, CliError> {
    inference::ProfileRegistry::load()
        .map_err(|message| CliError::new("PROFILE_REJECTED", message, 10, false))
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
        return flash::require_campaign_acknowledgement(&runtime_dir, hardware_campaign);
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
    flash::require_campaign_acknowledgement(&runtime_dir, hardware_campaign)
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
    help::HELP
        .iter()
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
        help::HELP.iter().any(|spec| spec.command == key)
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

#[cfg(test)]
mod tests {
    use super::daemon::daemon_next_commands;
    use super::help::{HELP, child_specs, help_spec, validate_against_command_tree};
    use super::job::{job_event_json, render_job_jsonl};
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
    fn restoration_accepts_the_explicit_profile_required_by_identity_admission() {
        let arguments = strings(&[
            "flash",
            "restore-ramdisk",
            "--artifact",
            "1111111111111111111111111111111111111111111111111111111111111111",
            "--profile",
            "org.openharmony.dayu200@1.0.0",
            "--device",
            "OBS-PREFLIGHT",
            "--hardware-campaign",
            "preboot-acceptance",
            "--source-job",
            "JOB-SOURCE",
        ]);
        assert!(validate_against_command_tree_for_test(&arguments).is_ok());
        let mut invalid = arguments;
        invalid[5] = "not-a-versioned-profile".into();
        assert!(validate_against_command_tree_for_test(&invalid).is_err());
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
            "flash verify",
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
        let validate_artifact = |artifact: &str| {
            validate_against_command_tree_for_test(&strings(&[
                "artifact",
                "show",
                "--artifact",
                artifact,
            ]))
        };
        assert!(validate_artifact(&"a".repeat(64)).is_ok());
        for artifact in ["<artifact-id>", "./firmware.tar.gz", "...", &"A".repeat(64)] {
            assert!(
                validate_artifact(artifact).is_err(),
                "artifact value {artifact:?} must not parse"
            );
        }
        let validate_device = |device: &str| {
            validate_against_command_tree_for_test(&strings(&[
                "device", "list", "--device", device,
            ]))
        };
        assert!(validate_device("OBS-1").is_ok());
        assert!(validate_device("<observation-id>").is_err());
    }

    #[test]
    fn typed_relations_refuse_ambiguous_or_incomplete_effect_inputs() {
        assert!(
            validate_against_command_tree_for_test(&strings(&[
                "apply",
                "--plan",
                "PLAN-1",
                "--expect-plan-sha256",
                &"0".repeat(64),
                "--ack",
                "data-loss:userdata",
            ]))
            .is_ok()
        );
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
