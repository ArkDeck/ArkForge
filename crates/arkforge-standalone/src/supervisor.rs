//! Persistent local CLI authority supervisor.
//!
//! The supervisor, not a short-lived command, owns the pairing secret. It
//! passes the secret to `arkforged` over an anonymous stdin pipe and exposes an
//! owner-only local control socket containing no secret-bearing operation.

use crate::StandaloneError;
use crate::authority_support::{self, AuthoritySupportKey};
use crate::hdc_control::{ControlContext, HdcControlPort};
use arkforge_authority_api::authority_side::mint_integrity_tag;
use arkforge_authority_api::{
    ControllerPairingSecret, PairingEpoch, PermitIntegrityTag, StepPermit,
};
use arkforge_client::{ControllerClient, MaterializeInput};
use arkforge_client::{DeviceObservationView, DeviceProbeView, PublicClient, PublicRuntimeInfo};
use arkforge_core::Sha256Digest;
use arkforge_core::authority::{AuthorityBindingRef, AuthorityNamespace, AuthoritySupportState};
use arkforge_core::digest::sha256;
use arkforge_core::ids::{
    AttemptId, ControllerSessionId, JobId, OpaqueId, PermitId, PlanId, StepId,
};
use arkforge_ipc::framing::{read_frame, write_frame};
use arkforge_ipc::messages::{
    ExecutablePlan, JobEventKind, MaterializePlanResponse, StepAdmissionSnapshot,
    SubmitManagedControlReceiptRequest, SubmitStepPermitRequest,
};
use arkforge_ipc::wire;
use arkforge_platform::{
    LocalChannel, LocalEndpoint, LocalListener, LocalStream, fill_random, protect_path,
    replace_file, sync_directory, unix_socket_path,
};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const READY_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Default)]
pub struct DaemonOptions {
    profile_files: Vec<PathBuf>,
    hdc: Option<PathBuf>,
    expect_hdc_sha256: Option<String>,
    hardware_campaign: Option<String>,
    require_release_signing: bool,
}

impl DaemonOptions {
    /// The lifecycle options a stored configuration implies.
    ///
    /// The campaign is not among them. It comes from the call that needed the
    /// runtime, because a named acceptance run is opened deliberately or not at
    /// all (design.md 1.2).
    pub fn from_config(
        config: &crate::config::RuntimeConfig,
        hardware_campaign: Option<&str>,
    ) -> Self {
        Self {
            profile_files: config
                .profile_files
                .iter()
                .map(|profile| profile.path.clone())
                .collect(),
            hdc: config.hdc.as_ref().map(|hdc| hdc.path.clone()),
            expect_hdc_sha256: config.hdc.as_ref().map(|hdc| hdc.sha256.clone()),
            hardware_campaign: hardware_campaign.map(str::to_string),
            require_release_signing: config.require_release_signing,
        }
    }

    /// Applies explicit call-site arguments over stored configuration.
    ///
    /// A binding is overridden only where the call gives a complete one: half a
    /// pair would silently mix a configured digest with a call-site path, which
    /// is exactly the pairing the config format exists to prevent.
    pub fn overridden_by(mut self, explicit: Self) -> Self {
        if explicit.hdc.is_some() && explicit.expect_hdc_sha256.is_some() {
            self.hdc = explicit.hdc;
            self.expect_hdc_sha256 = explicit.expect_hdc_sha256;
        }
        if !explicit.profile_files.is_empty() {
            self.profile_files = explicit.profile_files;
        }
        if explicit.hardware_campaign.is_some() {
            self.hardware_campaign = explicit.hardware_campaign;
        }
        if explicit.require_release_signing {
            self.require_release_signing = true;
        }
        self
    }

    pub fn parse(arguments: &[String]) -> Result<Self, StandaloneError> {
        let mut options = Self::default();
        let mut index = 0;
        while index < arguments.len() {
            match arguments[index].as_str() {
                "--profile-file" => {
                    index += 1;
                    options
                        .profile_files
                        .push(PathBuf::from(arguments.get(index).ok_or_else(|| {
                            StandaloneError::invalid("--profile-file requires a file path.")
                        })?));
                }
                "--hdc" => {
                    index += 1;
                    let path = PathBuf::from(arguments.get(index).ok_or_else(|| {
                        StandaloneError::invalid("--hdc requires an absolute path.")
                    })?);
                    if !path.is_absolute() {
                        return Err(StandaloneError::invalid("--hdc requires an absolute path."));
                    }
                    if options.hdc.replace(path).is_some() {
                        return Err(StandaloneError::invalid("--hdc may be supplied only once."));
                    }
                }
                "--expect-hdc-sha256" => {
                    index += 1;
                    let digest = arguments.get(index).ok_or_else(|| {
                        StandaloneError::invalid(
                            "--expect-hdc-sha256 requires 64 lowercase hex digits.",
                        )
                    })?;
                    if digest.len() != 64
                        || !digest
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    {
                        return Err(StandaloneError::invalid(
                            "--expect-hdc-sha256 requires 64 lowercase hex digits.",
                        ));
                    }
                    if options.expect_hdc_sha256.replace(digest.clone()).is_some() {
                        return Err(StandaloneError::invalid(
                            "--expect-hdc-sha256 may be supplied only once.",
                        ));
                    }
                }
                "--require-release-signing" => {
                    if options.require_release_signing {
                        return Err(StandaloneError::invalid(
                            "--require-release-signing may be supplied only once.",
                        ));
                    }
                    options.require_release_signing = true;
                }
                "--hardware-campaign" => {
                    index += 1;
                    let campaign = arguments.get(index).ok_or_else(|| {
                        StandaloneError::invalid(
                            "--hardware-campaign requires a non-empty campaign id.",
                        )
                    })?;
                    if campaign.trim().is_empty() {
                        return Err(StandaloneError::invalid(
                            "--hardware-campaign requires a non-empty campaign id.",
                        ));
                    }
                    if options
                        .hardware_campaign
                        .replace(campaign.clone())
                        .is_some()
                    {
                        return Err(StandaloneError::invalid(
                            "--hardware-campaign may be supplied only once.",
                        ));
                    }
                }
                argument => {
                    return Err(StandaloneError::invalid(format!(
                        "Unknown daemon option {argument:?}."
                    )));
                }
            }
            index += 1;
        }
        if options.hdc.is_some() != options.expect_hdc_sha256.is_some() {
            return Err(StandaloneError::invalid(
                "--hdc and --expect-hdc-sha256 are required together.",
            ));
        }
        Ok(options)
    }

    fn append_public_arguments(&self, command: &mut Command, runtime_dir: &Path) {
        command
            .arg("--runtime-dir")
            .arg(runtime_dir)
            .arg("daemon")
            .arg("run");
        for profile in &self.profile_files {
            command.arg("--profile-file").arg(profile);
        }
        if let (Some(hdc), Some(digest)) = (&self.hdc, &self.expect_hdc_sha256) {
            command
                .arg("--hdc")
                .arg(hdc)
                .arg("--expect-hdc-sha256")
                .arg(digest);
        }
        if self.require_release_signing {
            command.arg("--require-release-signing");
        }
        if let Some(campaign) = &self.hardware_campaign {
            command.arg("--hardware-campaign").arg(campaign);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonStatus {
    pub supervisor_pid: u32,
    pub daemon_pid: u32,
    pub epoch: u64,
    pub protocol_major: u32,
    pub protocol_minor: u32,
    pub daemon_version: String,
    pub mechanics_ready: bool,
    pub authority_support_available: bool,
    pub hdc_bound: bool,
    pub hdc_sha256: String,
    pub hardware_campaign: String,
    pub active_jobs: usize,
    pub blockers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileStatus {
    pub job_id: String,
    pub verdict: String,
    pub detail: String,
    pub completeness: String,
    pub possible_effects: Vec<String>,
    pub original_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AuthorityPlanRecord {
    plan: ExecutablePlan,
    binding_id: String,
    stable_identity_sha256: Vec<u8>,
    device_id: String,
    profile_id: String,
    supersedes_job_id: String,
    topology_sha256: String,
    toolchain_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ActiveTargetLineage {
    plan_id: String,
    current_device_id: String,
    current_stable_identity_sha256: Vec<u8>,
    topology_sha256: String,
    revision: u64,
}

/// How long to wait for a competing process to finish starting the runtime.
const STARTUP_LOCK_TIMEOUT: Duration = Duration::from_secs(20);

/// Makes a runtime exist, reporting whether this call is the one that made it.
///
/// Concurrent callers serialize on an owner-only lock so exactly one of them
/// creates the runtime and the rest attach to what it created. It is never a
/// takeover: an already-paired supervisor is found by [`status`] and returned
/// as-is, and a mechanics daemon paired with another authority still refuses
/// through [`start`].
pub fn ensure_started(runtime_dir: &Path, options: DaemonOptions) -> Result<bool, StandaloneError> {
    if status(runtime_dir).is_ok() {
        return Ok(false);
    }
    let _lock = StartupLock::acquire(runtime_dir)?;
    // The competitor may have finished while this call waited for the lock.
    if status(runtime_dir).is_ok() {
        return Ok(false);
    }
    start(runtime_dir.to_path_buf(), options)?;
    Ok(true)
}

/// An owner-only exclusive lock held while one process starts the runtime.
struct StartupLock {
    path: PathBuf,
}

impl StartupLock {
    fn acquire(runtime_dir: &Path) -> Result<Self, StandaloneError> {
        std::fs::create_dir_all(runtime_dir)
            .map_err(|error| internal("create the runtime directory", error))?;
        let path = runtime_dir.join("startup.lock");
        let deadline = Instant::now() + STARTUP_LOCK_TIMEOUT;
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => {
                    let _ = protect_path(&path, false);
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if status(runtime_dir).is_ok() {
                        // The holder succeeded; there is nothing left to lock.
                        return Ok(Self {
                            path: PathBuf::new(),
                        });
                    }
                    if Instant::now() >= deadline {
                        // The holder died without releasing. Taking the lock
                        // over is safe because the caller re-checks whether a
                        // runtime exists before creating one.
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(error) => {
                    return Err(internal("take the runtime startup lock", error));
                }
            }
        }
    }
}

impl Drop for StartupLock {
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub fn start(
    runtime_dir: PathBuf,
    options: DaemonOptions,
) -> Result<DaemonStatus, StandaloneError> {
    let executable =
        std::env::current_exe().map_err(|error| internal("identify arkforge", error))?;
    start_with_launcher(runtime_dir, options, executable)
}

/// Starts the background authority through a packaged ArkForge launcher.
///
/// Desktop applications use this entry point because their current executable
/// is a UI process and cannot service the supervisor mode. The CLI continues
/// to use [`start`] and therefore preserves its existing behavior.
pub fn start_with_launcher(
    runtime_dir: PathBuf,
    options: DaemonOptions,
    executable: PathBuf,
) -> Result<DaemonStatus, StandaloneError> {
    if status(&runtime_dir).is_ok() {
        return Err(StandaloneError::new(
            "RUNTIME_ALREADY_RUNNING",
            "This ArkForge runtime already has a live CLI authority supervisor.",
            6,
            false,
        ));
    }
    let mut command = Command::new(executable);
    options.append_public_arguments(&mut command, &runtime_dir);
    command
        .env("ARKFORGE_INTERNAL_SUPERVISOR_MODE", "background")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
        .spawn()
        .map_err(|error| internal("start the authority supervisor", error))?;

    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        match status(&runtime_dir) {
            Ok(status) => return Ok(status),
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            Err(error) => return Err(error),
        }
    }
}

pub fn run(
    runtime_dir: PathBuf,
    options: DaemonOptions,
    foreground_output: bool,
) -> Result<(), StandaloneError> {
    prepare_runtime(&runtime_dir)?;
    let endpoint = LocalEndpoint::for_runtime(&runtime_dir, LocalChannel::Supervisor);
    if LocalStream::connect(&endpoint).is_ok() {
        return Err(StandaloneError::new(
            "RUNTIME_ALREADY_RUNNING",
            "This runtime is already owned by a live CLI authority supervisor.",
            6,
            false,
        ));
    }
    let mut listener = LocalListener::bind(&endpoint)
        .map_err(|error| internal("bind the authority supervisor socket", error))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| internal("configure the authority supervisor socket", error))?;

    let (epoch, secret) = fresh_pairing(&runtime_dir)?;
    validate_tool_bindings(&options)?;
    let background = std::env::var_os("ARKFORGE_INTERNAL_SUPERVISOR_MODE")
        .is_some_and(|value| value == "background");
    let mut daemon = spawn_daemon(
        &runtime_dir,
        &options,
        epoch,
        &secret,
        foreground_output && !background,
    )?;
    let _ = wait_for_daemon(&runtime_dir, &mut daemon)?;
    if foreground_output && !background {
        println!(
            "arkforge supervisor {} paired arkforged {} at epoch {}",
            std::process::id(),
            daemon.id(),
            epoch
        );
    }
    let pairing = ControllerPairingSecret::new(PairingEpoch(epoch), secret);
    let hdc_digest = options
        .expect_hdc_sha256
        .as_deref()
        .map(Sha256Digest::parse_hex)
        .transpose()
        .map_err(|error| internal("decode the validated HDC digest", error))?;
    let mut hdc = options
        .hdc
        .clone()
        .zip(hdc_digest)
        .map(|(executable, digest)| HdcControlPort::new(executable, runtime_dir.clone(), digest));
    let result = serve(
        listener,
        &runtime_dir,
        &mut daemon,
        SupervisorSession {
            epoch,
            pairing: &pairing,
            hdc: hdc.as_mut(),
            hdc_digest,
            hardware_campaign: options.hardware_campaign.as_deref(),
        },
    );
    let _ = daemon.kill();
    let _ = daemon.wait();
    for channel in [
        LocalChannel::Supervisor,
        LocalChannel::Public,
        LocalChannel::Controller,
    ] {
        if let Some(path) = unix_socket_path(&runtime_dir, channel) {
            let _ = std::fs::remove_file(path);
        }
    }
    result
}

pub fn status(runtime_dir: &Path) -> Result<DaemonStatus, StandaloneError> {
    let payload = request(runtime_dir, "status", &[])?;
    decode_status(&payload)
}

pub fn stop(runtime_dir: &Path) -> Result<DaemonStatus, StandaloneError> {
    let payload = request(runtime_dir, "stop", &[])?;
    decode_status(&payload)
}

pub fn materialize_plan(
    runtime_dir: &Path,
    artifact: &str,
    profile: &str,
    device: &str,
) -> Result<MaterializePlanResponse, StandaloneError> {
    let payload = request(
        runtime_dir,
        "materialize-plan",
        &[artifact, profile, device],
    )?;
    MaterializePlanResponse::decode(&payload).map_err(|error| {
        StandaloneError::new(
            "SUPERVISOR_RESPONSE_INVALID",
            format!("The supervisor returned an invalid plan response: {error}"),
            10,
            false,
        )
    })
}

pub fn assess_plan(
    runtime_dir: &Path,
    artifact: &str,
    profile: &str,
    device: &str,
) -> Result<arkforge_ipc::messages::Assessment, StandaloneError> {
    let payload = request(runtime_dir, "assess-plan", &[artifact, profile, device])?;
    match MaterializePlanResponse::decode(&payload).map_err(|error| {
        StandaloneError::new(
            "SUPERVISOR_RESPONSE_INVALID",
            format!("The supervisor assessment response is invalid: {error}"),
            10,
            false,
        )
    })? {
        MaterializePlanResponse::Assessment(assessment) => Ok(assessment),
        MaterializePlanResponse::Plan(_) => Err(StandaloneError::new(
            "ASSESSMENT_BECAME_EXECUTABLE_PLAN",
            "A read-only assessment returned an executable plan.",
            10,
            false,
        )),
    }
}

pub fn cancel_job(
    runtime_dir: &Path,
    job_id: &str,
    expected_sequence: u64,
) -> Result<String, StandaloneError> {
    let sequence = expected_sequence.to_string();
    let payload = request(runtime_dir, "cancel-job", &[job_id, &sequence])?;
    decode_single_string(&payload, 1, "cancel disposition")
}

pub fn reconcile_job(runtime_dir: &Path, job_id: &str) -> Result<ReconcileStatus, StandaloneError> {
    let payload = request(runtime_dir, "reconcile-job", &[job_id])?;
    decode_reconcile(&payload)
}

pub fn apply_plan(
    runtime_dir: &Path,
    plan_id: &str,
    expected_plan_sha256: &str,
    acknowledgements: &[String],
    detach: bool,
) -> Result<String, StandaloneError> {
    let mut arguments = vec![
        plan_id,
        expected_plan_sha256,
        if detach { "true" } else { "false" },
    ];
    arguments.extend(acknowledgements.iter().map(String::as_str));
    let payload = request(runtime_dir, "apply-plan", &arguments)?;
    decode_single_string(&payload, 1, "durable job id")
}

pub fn materialize_recovery_plan(
    runtime_dir: &Path,
    job_id: &str,
    artifact: &str,
    profile: &str,
    device: &str,
) -> Result<MaterializePlanResponse, StandaloneError> {
    let payload = request(
        runtime_dir,
        "materialize-recovery-plan",
        &[job_id, artifact, profile, device],
    )?;
    MaterializePlanResponse::decode(&payload).map_err(|error| {
        StandaloneError::new(
            "SUPERVISOR_RESPONSE_INVALID",
            format!("The supervisor returned an invalid recovery plan response: {error}"),
            10,
            false,
        )
    })
}

fn request(runtime_dir: &Path, verb: &str, arguments: &[&str]) -> Result<Vec<u8>, StandaloneError> {
    let endpoint = LocalEndpoint::for_runtime(runtime_dir, LocalChannel::Supervisor);
    let mut stream = LocalStream::connect(&endpoint).map_err(|error| {
        StandaloneError::new(
            "DAEMON_UNAVAILABLE",
            format!(
                "No CLI authority supervisor is listening at {}: {error}",
                endpoint.display()
            ),
            5,
            true,
        )
    })?;
    let mut payload = Vec::new();
    wire::write_string(&mut payload, 1, verb);
    for argument in arguments {
        wire::write_string(&mut payload, 2, argument);
    }
    write_frame(&mut stream, &payload)
        .map_err(|error| internal("write supervisor request", error))?;
    let response = read_frame(&mut stream)
        .map_err(|error| internal("read supervisor response", error))?
        .ok_or_else(|| internal("read supervisor response", "connection closed"))?;
    decode_reply(&response)
}

struct SupervisorSession<'a> {
    epoch: u64,
    pairing: &'a ControllerPairingSecret,
    hdc: Option<&'a mut HdcControlPort>,
    hdc_digest: Option<Sha256Digest>,
    hardware_campaign: Option<&'a str>,
}

fn serve(
    mut listener: LocalListener,
    runtime_dir: &Path,
    daemon: &mut Child,
    mut session: SupervisorSession<'_>,
) -> Result<(), StandaloneError> {
    let mut job_cursors = BTreeMap::new();
    let mut target_lineages = BTreeMap::new();
    let mut last_drive = Instant::now() - Duration::from_secs(1);
    loop {
        if let Some(exit) = daemon
            .try_wait()
            .map_err(|error| internal("observe arkforged", error))?
        {
            return Err(StandaloneError::new(
                "MECHANICS_DAEMON_EXITED",
                format!("arkforged exited unexpectedly with {exit}."),
                10,
                true,
            ));
        }
        if last_drive.elapsed() >= Duration::from_millis(100) {
            if let Err(error) = drive_active_jobs(
                runtime_dir,
                session.epoch,
                session.pairing,
                session.hdc.as_deref_mut(),
                &mut job_cursors,
                &mut target_lineages,
            ) {
                eprintln!("arkforge supervisor: {}: {}", error.code, error.message);
            }
            last_drive = Instant::now();
        }
        match listener.accept() {
            Ok(mut stream) => {
                let Some(request) = read_frame(&mut stream)
                    .map_err(|error| internal("read supervisor request", error))?
                else {
                    continue;
                };
                let (command, arguments) = decode_request(&request)?;
                let mut public = PublicClient::connect(runtime_dir)?;
                let runtime_info = public.runtime_info().clone();
                let active_jobs = public
                    .job_list()?
                    .into_iter()
                    .filter(|job| !job.terminal)
                    .count();
                let mut blockers = runtime_info.execution_blockers.clone();
                if session.hdc_digest.is_none() {
                    blockers.push("AUTHORITY_HDC_UNBOUND".into());
                }
                if session.hardware_campaign.is_none()
                    && !authority_support::has_reviewed_support_records()
                {
                    blockers.push("AUTHORITY_SUPPORT_UNPUBLISHED".into());
                }
                let authority_support_available = session.hdc_digest.is_some()
                    && (session.hardware_campaign.is_some()
                        || authority_support::has_reviewed_support_records());
                let status = DaemonStatus {
                    supervisor_pid: std::process::id(),
                    daemon_pid: daemon.id(),
                    epoch: session.epoch,
                    protocol_major: runtime_info.protocol_major,
                    protocol_minor: runtime_info.protocol_minor,
                    daemon_version: runtime_info.daemon_version.clone(),
                    mechanics_ready: runtime_info.execution_ready,
                    authority_support_available,
                    hdc_bound: session.hdc_digest.is_some(),
                    hdc_sha256: session
                        .hdc_digest
                        .map_or_else(String::new, |digest| digest.to_hex()),
                    hardware_campaign: session.hardware_campaign.unwrap_or_default().to_string(),
                    active_jobs,
                    blockers,
                };
                match command.as_str() {
                    "status" => write_reply(&mut stream, Ok(encode_status(&status)))?,
                    "stop" if active_jobs == 0 => {
                        write_reply(&mut stream, Ok(encode_status(&status)))?;
                        return Ok(());
                    }
                    "stop" => {
                        write_reply(
                            &mut stream,
                            Err(StandaloneError::new(
                                "ACTIVE_JOBS",
                                format!(
                                    "The runtime has {active_jobs} active job(s); request cancellation and wait for a terminal state before stopping."
                                ),
                                6,
                                true,
                            )),
                        )?;
                    }
                    "materialize-plan" => {
                        let result = handle_materialize(
                            runtime_dir,
                            &mut public,
                            &arguments,
                            session.hdc_digest,
                            session.hardware_campaign,
                        )
                        .map(|response| response.encode());
                        write_reply(&mut stream, result)?;
                    }
                    "assess-plan" => {
                        let result = handle_assess(
                            runtime_dir,
                            &mut public,
                            &arguments,
                            session.hdc_digest,
                            session.hardware_campaign,
                        )
                        .map(|response| response.encode());
                        write_reply(&mut stream, result)?;
                    }
                    "apply-plan" => {
                        let result = handle_apply(
                            runtime_dir,
                            &public,
                            session.epoch,
                            session.hdc_digest,
                            session.hardware_campaign,
                            &arguments,
                        );
                        write_reply(&mut stream, result)?;
                    }
                    "materialize-recovery-plan" => {
                        let result = handle_recovery_plan(
                            runtime_dir,
                            &mut public,
                            &arguments,
                            session.hdc_digest,
                            session.hardware_campaign,
                        )
                        .map(|response| response.encode());
                        write_reply(&mut stream, result)?;
                    }
                    "cancel-job" => {
                        let result = handle_cancel(runtime_dir, &arguments);
                        write_reply(&mut stream, result)?;
                    }
                    "reconcile-job" => {
                        let result = handle_reconcile(runtime_dir, &arguments);
                        write_reply(&mut stream, result)?;
                    }
                    _ => {
                        write_reply(
                            &mut stream,
                            Err(StandaloneError::new(
                                "SUPERVISOR_REQUEST_INVALID",
                                "The authority supervisor does not recognize this request.",
                                2,
                                false,
                            )),
                        )?;
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(internal("accept a supervisor client", error)),
        }
    }
}

fn handle_cancel(runtime_dir: &Path, arguments: &[String]) -> Result<Vec<u8>, StandaloneError> {
    let [job_id, expected] = arguments else {
        return Err(StandaloneError::new(
            "SUPERVISOR_REQUEST_INVALID",
            "cancel-job requires a job id and expected journal sequence.",
            2,
            false,
        ));
    };
    JobId::new(job_id).map_err(|error| {
        StandaloneError::new(
            "SUPERVISOR_REQUEST_INVALID",
            format!("cancel-job requires a canonical job id: {error}"),
            2,
            false,
        )
    })?;
    let expected = expected.parse::<u64>().map_err(|_| {
        StandaloneError::new(
            "SUPERVISOR_REQUEST_INVALID",
            "cancel-job expected sequence is not an unsigned integer.",
            2,
            false,
        )
    })?;
    let mut controller = ControllerClient::connect(runtime_dir)?;
    let state = controller.cancel(job_id, expected)?;
    let mut out = Vec::new();
    wire::write_string(&mut out, 1, &state);
    Ok(out)
}

fn handle_reconcile(runtime_dir: &Path, arguments: &[String]) -> Result<Vec<u8>, StandaloneError> {
    let [job_id] = arguments else {
        return Err(StandaloneError::new(
            "SUPERVISOR_REQUEST_INVALID",
            "reconcile-job requires exactly one job id.",
            2,
            false,
        ));
    };
    JobId::new(job_id).map_err(|error| {
        StandaloneError::new(
            "SUPERVISOR_REQUEST_INVALID",
            format!("reconcile-job requires a canonical job id: {error}"),
            2,
            false,
        )
    })?;
    let mut controller = ControllerClient::connect(runtime_dir)?;
    Ok(controller.reconcile(job_id)?)
}

fn decode_single_string(
    input: &[u8],
    wanted: u32,
    context: &str,
) -> Result<String, StandaloneError> {
    let mut reader = wire::Reader::new(input);
    while let Some((field, value)) = reader
        .next_field()
        .map_err(|error| internal(&format!("decode {context}"), error))?
    {
        if field == wanted {
            return Ok(value.as_str(wanted).map_err(status_decode)?.to_string());
        }
    }
    Err(StandaloneError::new(
        "SUPERVISOR_RESPONSE_INVALID",
        format!("The supervisor returned no {context}."),
        10,
        false,
    ))
}

fn decode_reconcile(input: &[u8]) -> Result<ReconcileStatus, StandaloneError> {
    let mut status = ReconcileStatus {
        job_id: String::new(),
        verdict: String::new(),
        detail: String::new(),
        completeness: String::new(),
        possible_effects: Vec::new(),
        original_state: String::new(),
    };
    let mut reader = wire::Reader::new(input);
    while let Some((field, value)) = reader
        .next_field()
        .map_err(|error| internal("decode reconciliation", error))?
    {
        match field {
            1 => status.job_id = value.as_str(1).map_err(status_decode)?.to_string(),
            2 => status.verdict = value.as_str(2).map_err(status_decode)?.to_string(),
            3 => status.detail = value.as_str(3).map_err(status_decode)?.to_string(),
            4 => status.completeness = value.as_str(4).map_err(status_decode)?.to_string(),
            5 => status
                .possible_effects
                .push(value.as_str(5).map_err(status_decode)?.to_string()),
            6 => status.original_state = value.as_str(6).map_err(status_decode)?.to_string(),
            _ => {}
        }
    }
    if status.job_id.is_empty() || status.verdict.is_empty() {
        return Err(StandaloneError::new(
            "SUPERVISOR_RESPONSE_INVALID",
            "The supervisor returned an incomplete reconciliation.",
            10,
            false,
        ));
    }
    Ok(status)
}

fn handle_materialize(
    runtime_dir: &Path,
    public: &mut PublicClient,
    arguments: &[String],
    hdc_digest: Option<Sha256Digest>,
    hardware_campaign: Option<&str>,
) -> Result<MaterializePlanResponse, StandaloneError> {
    handle_materialize_for(
        runtime_dir,
        public,
        arguments,
        MaterializationKind {
            execution_purpose: "primaryFlash",
            supersedes_job_id: "",
            assessment_only: false,
        },
        hdc_digest,
        hardware_campaign,
    )
}

fn handle_assess(
    runtime_dir: &Path,
    public: &mut PublicClient,
    arguments: &[String],
    hdc_digest: Option<Sha256Digest>,
    hardware_campaign: Option<&str>,
) -> Result<MaterializePlanResponse, StandaloneError> {
    handle_materialize_for(
        runtime_dir,
        public,
        arguments,
        MaterializationKind {
            execution_purpose: "primaryFlash",
            supersedes_job_id: "",
            assessment_only: true,
        },
        hdc_digest,
        hardware_campaign,
    )
}

fn handle_recovery_plan(
    runtime_dir: &Path,
    public: &mut PublicClient,
    arguments: &[String],
    hdc_digest: Option<Sha256Digest>,
    hardware_campaign: Option<&str>,
) -> Result<MaterializePlanResponse, StandaloneError> {
    let [job_id, artifact, profile, device] = arguments else {
        return Err(StandaloneError::new(
            "SUPERVISOR_REQUEST_INVALID",
            "materialize-recovery-plan requires job, artifact, profile, and device.",
            2,
            false,
        ));
    };
    JobId::new(job_id).map_err(|error| {
        StandaloneError::new(
            "SUPERVISOR_REQUEST_INVALID",
            format!("materialize-recovery-plan requires a canonical job id: {error}"),
            2,
            false,
        )
    })?;
    let mut controller = ControllerClient::connect(runtime_dir)?;
    let assessment = controller.plan_superseding_recovery(job_id)?;
    let mut eligible = false;
    let mut blocker = String::new();
    let mut detail = String::new();
    let mut reader = wire::Reader::new(&assessment);
    while let Some((field, value)) = reader
        .next_field()
        .map_err(|error| internal("decode superseding recovery assessment", error))?
    {
        match field {
            2 => eligible = value.as_bool().map_err(status_decode)?,
            3 => blocker = value.as_str(3).map_err(status_decode)?.to_string(),
            4 => detail = value.as_str(4).map_err(status_decode)?.to_string(),
            _ => {}
        }
    }
    if !eligible {
        return Err(StandaloneError::new(
            if blocker.is_empty() {
                "RECOVERY_NOT_ELIGIBLE".into()
            } else {
                blocker
            },
            if detail.is_empty() {
                format!("Job {job_id} is not eligible for a superseding recovery plan.")
            } else {
                detail
            },
            3,
            false,
        ));
    }
    handle_materialize_for(
        runtime_dir,
        public,
        &[artifact.clone(), profile.clone(), device.clone()],
        MaterializationKind {
            execution_purpose: "supersedingRecovery",
            supersedes_job_id: job_id,
            assessment_only: false,
        },
        hdc_digest,
        hardware_campaign,
    )
}

struct MaterializationKind<'a> {
    execution_purpose: &'a str,
    supersedes_job_id: &'a str,
    assessment_only: bool,
}

fn handle_materialize_for(
    runtime_dir: &Path,
    public: &mut PublicClient,
    arguments: &[String],
    kind: MaterializationKind<'_>,
    hdc_digest: Option<Sha256Digest>,
    hardware_campaign: Option<&str>,
) -> Result<MaterializePlanResponse, StandaloneError> {
    let [artifact, profile, device] = arguments else {
        return Err(StandaloneError::new(
            "SUPERVISOR_REQUEST_INVALID",
            "materialize-plan requires artifact, profile, and exact device observation.",
            2,
            false,
        ));
    };
    let observations = public.device_list()?;
    let observation = observations
        .iter()
        .find(|candidate| candidate.observation_id == *device)
        .ok_or_else(|| {
            StandaloneError::new(
                "OBSERVATION_NOT_FOUND",
                format!("No current observation exactly matches {device}."),
                5,
                true,
            )
        })?;
    let probe = public.device_probe(device, profile)?;
    let stable_identity = stable_identity_digest(observation, &probe);
    let binding_id = format!("CLI-BIND-{}", &stable_identity.to_hex()[..24]);
    if !kind.assessment_only {
        persist_binding(
            runtime_dir,
            &binding_id,
            device,
            profile,
            &stable_identity.to_hex(),
        )?;
    }
    let toolchain_id = public.runtime_info().toolchain_id.clone();
    let mut controller = ControllerClient::connect(runtime_dir)?;
    let pending_support_key = sha256(b"arkforge.cli-authority-support-pending");
    let first = controller.materialize_plan(&MaterializeInput {
        artifact_id: artifact,
        profile_id: profile,
        device_id: device,
        toolchain_id: &toolchain_id,
        authority_namespace: "arkforge.cli",
        binding_id: &binding_id,
        binding_revision: 1,
        stable_identity_sha256: stable_identity.as_bytes(),
        execution_purpose: kind.execution_purpose,
        authority_support_key_sha256: pending_support_key.as_bytes(),
        authority_support_state: "hardwareGated",
        authority_support_detail: "the exact mechanics maturity key has not been returned yet",
    })?;
    let mechanics_key = match &first {
        MaterializePlanResponse::Assessment(assessment) => {
            parse_mechanics_key(&assessment.mechanics_maturity_key_sha256)?
        }
        MaterializePlanResponse::Plan(_) => {
            return Err(StandaloneError::new(
                "AUTHORITY_GATE_BYPASSED",
                "The daemon produced an executable plan for a hardware-gated authority binding.",
                10,
                false,
            ));
        }
    };
    let (support_key, support_state) =
        current_authority_support(mechanics_key, hdc_digest, hardware_campaign)?;
    let MaterializePlanResponse::Assessment(mut first_assessment) = first else {
        unreachable!("the pending support gate above requires an assessment")
    };
    first_assessment.authority_support_key_sha256 = support_key.to_hex();
    first_assessment.authority_support_state = support_state.as_str().to_string();
    if !support_state.permits_execution() {
        first_assessment.unavailable_reason = format!(
            "authority support is {} for exact key {}: {} Mechanics state {} does not bypass this independent gate.",
            support_state.as_str(),
            support_key,
            support_state
                .blocker()
                .unwrap_or("no reviewed support record"),
            first_assessment.mechanics_maturity_state,
        );
        return Ok(MaterializePlanResponse::Assessment(first_assessment));
    }
    if kind.assessment_only {
        close_resolved_authority_blocker(&mut first_assessment);
        return Ok(MaterializePlanResponse::Assessment(first_assessment));
    }
    let support_detail = support_state
        .campaign()
        .or_else(|| support_state.blocker())
        .unwrap_or_default();
    let response = controller.materialize_plan(&MaterializeInput {
        artifact_id: artifact,
        profile_id: profile,
        device_id: device,
        toolchain_id: &toolchain_id,
        authority_namespace: "arkforge.cli",
        binding_id: &binding_id,
        binding_revision: 1,
        stable_identity_sha256: stable_identity.as_bytes(),
        execution_purpose: kind.execution_purpose,
        authority_support_key_sha256: support_key.as_bytes(),
        authority_support_state: support_state.as_str(),
        authority_support_detail: support_detail,
    })?;
    if let MaterializePlanResponse::Plan(plan) = &response {
        require_authority_support(plan, support_key, &support_state)?;
        persist_authority_plan(
            runtime_dir,
            &AuthorityPlanRecord {
                plan: plan.clone(),
                binding_id,
                stable_identity_sha256: stable_identity.as_bytes().to_vec(),
                device_id: device.clone(),
                profile_id: profile.clone(),
                supersedes_job_id: kind.supersedes_job_id.to_string(),
                topology_sha256: observation.topology_sha256.clone(),
                toolchain_id,
            },
        )?;
    }
    Ok(response)
}

fn close_resolved_authority_blocker(assessment: &mut arkforge_ipc::messages::Assessment) {
    assessment
        .unknowns
        .retain(|unknown| unknown.key != "RK-A01");
    assessment
        .evidence_requirements
        .retain(|requirement| requirement.key != "EVR-RK-A01");
    if assessment.unknowns.is_empty()
        && matches!(
            assessment.mechanics_maturity_state.as_str(),
            "productionVerified" | "hardwareCampaign"
        )
    {
        assessment.availability = "available".into();
        assessment.unavailable_reason.clear();
    }
}

fn parse_mechanics_key(value: &str) -> Result<Sha256Digest, StandaloneError> {
    Sha256Digest::parse_hex(value).map_err(|error| {
        StandaloneError::new(
            "MECHANICS_MATURITY_KEY_INVALID",
            format!("The materialization carries no usable mechanics maturity key: {error}"),
            10,
            false,
        )
    })
}

fn current_authority_support(
    mechanics: Sha256Digest,
    hdc_digest: Option<Sha256Digest>,
    hardware_campaign: Option<&str>,
) -> Result<(Sha256Digest, AuthoritySupportState), StandaloneError> {
    let executable =
        std::env::current_exe().map_err(|error| internal("identify arkforge", error))?;
    let implementation = arkforged::dispatch::executable_digest(&executable)
        .map_err(|error| internal("digest the CLI authority build", error))?;
    let key = AuthoritySupportKey::for_running_build(
        implementation,
        mechanics,
        hdc_digest.unwrap_or_else(|| sha256(b"arkforge.cli-hdc-unbound")),
    );
    let key_digest = key
        .digest()
        .map_err(|error| StandaloneError::new("AUTHORITY_SUPPORT_KEY_INVALID", error, 10, false))?;
    let state = match hdc_digest {
        Some(_) => authority_support::classify(&key, hardware_campaign),
        None => AuthoritySupportState::HardwareGated {
            blocker: "No exact HDC executable and digest are bound to this authority runtime."
                .into(),
        },
    };
    Ok((key_digest, state))
}

fn require_authority_support(
    plan: &ExecutablePlan,
    expected_key: Sha256Digest,
    expected_state: &AuthoritySupportState,
) -> Result<(), StandaloneError> {
    let actual_key =
        Sha256Digest::parse_hex(&plan.authority_support_key_sha256).map_err(|error| {
            StandaloneError::new(
                "AUTHORITY_SUPPORT_KEY_INVALID",
                format!("The executable plan carries no usable authority support key: {error}"),
                10,
                false,
            )
        })?;
    let expected_campaign = expected_state.campaign().unwrap_or_default();
    if actual_key != expected_key
        || plan.authority_support_state != expected_state.as_str()
        || plan.authority_support_campaign != expected_campaign
    {
        return Err(StandaloneError::new(
            "AUTHORITY_SUPPORT_SEAL_MISMATCH",
            "The daemon did not seal the exact authority support key and state supplied by this supervisor.",
            10,
            false,
        ));
    }
    Ok(())
}

fn require_current_authority_support(
    plan: &ExecutablePlan,
    hdc_digest: Option<Sha256Digest>,
    hardware_campaign: Option<&str>,
) -> Result<(), StandaloneError> {
    let mechanics_key = parse_mechanics_key(&plan.mechanics_maturity_key_sha256)?;
    let (support_key, support_state) =
        current_authority_support(mechanics_key, hdc_digest, hardware_campaign)?;
    if !support_state.permits_execution() {
        return Err(StandaloneError::new(
            "AUTHORITY_SUPPORT_UNAVAILABLE",
            support_state
                .blocker()
                .unwrap_or("The current CLI authority combination is not executable."),
            3,
            false,
        ));
    }
    require_authority_support(plan, support_key, &support_state)
}

fn handle_apply(
    runtime_dir: &Path,
    public: &PublicClient,
    epoch: u64,
    hdc_digest: Option<Sha256Digest>,
    hardware_campaign: Option<&str>,
    arguments: &[String],
) -> Result<Vec<u8>, StandaloneError> {
    if arguments.len() < 3 {
        return Err(StandaloneError::new(
            "SUPERVISOR_REQUEST_INVALID",
            "apply-plan requires plan id, expected digest, detach disposition, and acknowledgements.",
            2,
            false,
        ));
    }
    let plan_id = &arguments[0];
    PlanId::new(plan_id).map_err(|error| {
        StandaloneError::new(
            "SUPERVISOR_REQUEST_INVALID",
            format!("apply-plan requires a canonical plan id: {error}"),
            2,
            false,
        )
    })?;
    let expected_digest = &arguments[1];
    let detach = match arguments[2].as_str() {
        "true" => true,
        "false" => false,
        _ => {
            return Err(StandaloneError::new(
                "SUPERVISOR_REQUEST_INVALID",
                "apply-plan detach disposition must be true or false.",
                2,
                false,
            ));
        }
    };
    let record = load_authority_plan(runtime_dir, plan_id)?;
    if record.plan.plan_sha256 != *expected_digest {
        return Err(StandaloneError::new(
            "PLAN_DIGEST_MISMATCH",
            format!(
                "Plan {} is sealed as {}, not caller expectation {}.",
                record.plan.plan_id, record.plan.plan_sha256, expected_digest
            ),
            4,
            false,
        ));
    }
    let supplied: std::collections::BTreeSet<String> = arguments[3..].iter().cloned().collect();
    if supplied.len() != arguments[3..].len() {
        return Err(StandaloneError::new(
            "UNEXPECTED_ACKNOWLEDGEMENT",
            "Each acknowledgement token must be supplied exactly once.",
            4,
            false,
        ));
    }
    let required: std::collections::BTreeSet<String> =
        record_acknowledgements(&record).into_iter().collect();
    if supplied != required {
        let missing = required.difference(&supplied).cloned().collect::<Vec<_>>();
        let unexpected = supplied.difference(&required).cloned().collect::<Vec<_>>();
        return Err(StandaloneError::new(
            if !missing.is_empty() {
                "ACKNOWLEDGEMENT_REQUIRED"
            } else {
                "UNEXPECTED_ACKNOWLEDGEMENT"
            },
            format!(
                "Acknowledgement set is not exact; missing=[{}], unexpected=[{}].",
                missing.join(","),
                unexpected.join(",")
            ),
            4,
            !missing.is_empty(),
        )
        .with_required_acknowledgements(missing));
    }
    require_current_authority_support(&record.plan, hdc_digest, hardware_campaign)?;
    if public.runtime_info().toolchain_id != record.toolchain_id {
        return Err(StandaloneError::new(
            "MECHANICS_RUNTIME_CHANGED",
            "The running mechanics toolchain differs from the one sealed by this authority plan; materialize a new plan.",
            3,
            false,
        ));
    }
    let now = arkforged::rescue::now_epoch_ms()?;
    if now >= record.plan.expires_at_epoch_ms {
        return Err(StandaloneError::new(
            "PLAN_EXPIRED",
            format!("Plan {plan_id} expired before apply."),
            3,
            false,
        ));
    }
    let mut controller = ControllerClient::connect(runtime_dir)?;
    let session = format!("CLI-SESSION-{epoch}");
    let job_id = controller.start_execution(
        plan_id,
        expected_digest,
        &record.plan.execution_purpose,
        &session,
    )?;
    persist_job_lineage(
        runtime_dir,
        &job_id,
        &ActiveTargetLineage {
            plan_id: record.plan.plan_id.clone(),
            current_device_id: record.device_id.clone(),
            current_stable_identity_sha256: record.stable_identity_sha256.clone(),
            topology_sha256: record.topology_sha256.clone(),
            revision: 1,
        },
    )?;
    let mut out = Vec::new();
    wire::write_string(&mut out, 1, &job_id);
    wire::write_bool(&mut out, 2, detach);
    Ok(out)
}

/// Stable acknowledgement tokens required before an application may submit
/// this plan. Presentation layers render the tokens but never derive them.
pub fn required_acknowledgements(plan: &ExecutablePlan) -> Vec<String> {
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
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn record_acknowledgements(record: &AuthorityPlanRecord) -> Vec<String> {
    let mut tokens = required_acknowledgements(&record.plan);
    if !record.supersedes_job_id.is_empty() {
        tokens.push(format!(
            "recovery:supersedes-job={}",
            record.supersedes_job_id
        ));
    }
    tokens.sort();
    tokens
}

fn authority_plan_path(runtime_dir: &Path, plan_id: &str) -> PathBuf {
    runtime_dir
        .join("authority")
        .join("plans")
        .join(format!("{}.plan", sha256(plan_id.as_bytes())))
}

fn job_lineage_path(runtime_dir: &Path, job_id: &str) -> PathBuf {
    runtime_dir
        .join("authority")
        .join("jobs")
        .join(format!("{}.binding", sha256(job_id.as_bytes())))
}

fn encode_job_lineage(lineage: &ActiveTargetLineage) -> Vec<u8> {
    let mut out = Vec::new();
    wire::write_string(&mut out, 1, &lineage.plan_id);
    wire::write_string(&mut out, 2, &lineage.current_device_id);
    wire::write_bytes(&mut out, 3, &lineage.current_stable_identity_sha256);
    wire::write_string(&mut out, 4, &lineage.topology_sha256);
    wire::write_uint64(&mut out, 5, lineage.revision);
    out
}

fn persist_job_lineage(
    runtime_dir: &Path,
    job_id: &str,
    lineage: &ActiveTargetLineage,
) -> Result<(), StandaloneError> {
    let path = job_lineage_path(runtime_dir, job_id);
    let root = path.parent().expect("job lineage path has parent");
    std::fs::create_dir_all(root)
        .map_err(|error| internal("create the authority job journal", error))?;
    protect_path(root, true)
        .map_err(|error| internal("protect the authority job journal", error))?;
    let encoded = encode_job_lineage(lineage);
    if path.exists() {
        let existing = load_job_lineage(runtime_dir, job_id)?;
        if existing.revision > lineage.revision {
            return Err(StandaloneError::new(
                "TARGET_LINEAGE_STALE",
                "A stale target-lineage revision cannot replace newer durable evidence.",
                6,
                false,
            ));
        }
        if existing.revision == lineage.revision {
            if existing == *lineage {
                return Ok(());
            }
            return Err(StandaloneError::new(
                "TARGET_LINEAGE_CONFLICT",
                "The same target-lineage revision already names different facts.",
                6,
                false,
            ));
        }
    }
    let temporary = root.join(format!(
        ".{}.{}.{}.tmp",
        sha256(job_id.as_bytes()),
        lineage.revision,
        std::process::id(),
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| internal("create a target-lineage transaction", error))?;
    protect_path(&temporary, false)
        .map_err(|error| internal("protect a target-lineage transaction", error))?;
    file.write_all(&encoded)
        .and_then(|_| file.sync_all())
        .map_err(|error| internal("commit target-lineage bytes", error))?;
    replace_file(&temporary, &path)
        .map_err(|error| internal("publish target-lineage revision", error))
}

fn load_job_lineage(
    runtime_dir: &Path,
    job_id: &str,
) -> Result<ActiveTargetLineage, StandaloneError> {
    let encoded = std::fs::read(job_lineage_path(runtime_dir, job_id))
        .map_err(|error| internal("read the authority job lineage", error))?;
    let mut lineage = ActiveTargetLineage {
        plan_id: String::new(),
        current_device_id: String::new(),
        current_stable_identity_sha256: Vec::new(),
        topology_sha256: String::new(),
        revision: 0,
    };
    let mut reader = wire::Reader::new(&encoded);
    while let Some((field, value)) = reader
        .next_field()
        .map_err(|error| internal("decode the authority job lineage", error))?
    {
        match field {
            1 => lineage.plan_id = value.as_str(1).map_err(status_decode)?.to_string(),
            2 => lineage.current_device_id = value.as_str(2).map_err(status_decode)?.to_string(),
            3 => {
                lineage.current_stable_identity_sha256 =
                    value.as_bytes().map_err(status_decode)?.to_vec()
            }
            4 => lineage.topology_sha256 = value.as_str(4).map_err(status_decode)?.to_string(),
            5 => lineage.revision = value.as_u64().map_err(status_decode)?,
            _ => {}
        }
    }
    if lineage.plan_id.is_empty()
        || lineage.current_device_id.is_empty()
        || lineage.current_stable_identity_sha256.len() != 32
        || lineage.topology_sha256.is_empty()
        || lineage.revision == 0
    {
        return Err(StandaloneError::new(
            "TARGET_LINEAGE_INVALID",
            "The durable target lineage is incomplete.",
            10,
            false,
        ));
    }
    Ok(lineage)
}

fn encode_authority_plan(record: &AuthorityPlanRecord) -> Vec<u8> {
    let mut out = Vec::new();
    wire::write_message(&mut out, 1, &record.plan.encode());
    wire::write_string(&mut out, 2, &record.binding_id);
    wire::write_bytes(&mut out, 3, &record.stable_identity_sha256);
    wire::write_string(&mut out, 4, &record.device_id);
    wire::write_string(&mut out, 5, &record.profile_id);
    wire::write_string(&mut out, 6, &record.supersedes_job_id);
    wire::write_string(&mut out, 7, &record.topology_sha256);
    wire::write_string(&mut out, 8, &record.toolchain_id);
    out
}

fn persist_authority_plan(
    runtime_dir: &Path,
    record: &AuthorityPlanRecord,
) -> Result<(), StandaloneError> {
    let path = authority_plan_path(runtime_dir, &record.plan.plan_id);
    let root = path.parent().expect("plan path has parent");
    std::fs::create_dir_all(root)
        .map_err(|error| internal("create the authority plan journal", error))?;
    protect_path(root, true)
        .map_err(|error| internal("protect the authority plan journal", error))?;
    let encoded = encode_authority_plan(record);
    if path.exists() {
        let existing = std::fs::read(&path)
            .map_err(|error| internal("read the authority plan journal", error))?;
        if existing != encoded {
            return Err(StandaloneError::new(
                "PLAN_STATE_CONFLICT",
                format!(
                    "Authority plan {} already exists with different bytes.",
                    record.plan.plan_id
                ),
                6,
                false,
            ));
        }
        return Ok(());
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| internal("create the authority plan journal", error))?;
    protect_path(&path, false)
        .map_err(|error| internal("protect the authority plan journal", error))?;
    file.write_all(&encoded)
        .and_then(|_| file.sync_all())
        .map_err(|error| internal("commit the authority plan journal", error))
}

fn load_authority_plan(
    runtime_dir: &Path,
    plan_id: &str,
) -> Result<AuthorityPlanRecord, StandaloneError> {
    let path = authority_plan_path(runtime_dir, plan_id);
    let encoded = std::fs::read(&path).map_err(|error| {
        StandaloneError::new(
            "PLAN_NOT_FOUND",
            format!(
                "Cannot read authority plan {plan_id} at {}: {error}",
                path.display()
            ),
            5,
            false,
        )
    })?;
    let mut plan = None;
    let mut binding_id = String::new();
    let mut stable_identity_sha256 = Vec::new();
    let mut device_id = String::new();
    let mut profile_id = String::new();
    let mut supersedes_job_id = String::new();
    let mut topology_sha256 = String::new();
    let mut toolchain_id = String::new();
    let mut reader = wire::Reader::new(&encoded);
    while let Some((field, value)) = reader
        .next_field()
        .map_err(|error| internal("decode the authority plan journal", error))?
    {
        match field {
            1 => {
                plan = Some(
                    ExecutablePlan::decode(value.as_bytes().map_err(status_decode)?)
                        .map_err(status_decode)?,
                )
            }
            2 => binding_id = value.as_str(2).map_err(status_decode)?.to_string(),
            3 => stable_identity_sha256 = value.as_bytes().map_err(status_decode)?.to_vec(),
            4 => device_id = value.as_str(4).map_err(status_decode)?.to_string(),
            5 => profile_id = value.as_str(5).map_err(status_decode)?.to_string(),
            6 => supersedes_job_id = value.as_str(6).map_err(status_decode)?.to_string(),
            7 => topology_sha256 = value.as_str(7).map_err(status_decode)?.to_string(),
            8 => toolchain_id = value.as_str(8).map_err(status_decode)?.to_string(),
            _ => {}
        }
    }
    let record = AuthorityPlanRecord {
        plan: plan.ok_or_else(|| internal("decode the authority plan journal", "missing plan"))?,
        binding_id,
        stable_identity_sha256,
        device_id,
        profile_id,
        supersedes_job_id,
        topology_sha256,
        toolchain_id,
    };
    if record.plan.plan_id != plan_id
        || record.binding_id.is_empty()
        || record.stable_identity_sha256.len() != 32
        || record.topology_sha256.is_empty()
        || record.toolchain_id.is_empty()
    {
        return Err(StandaloneError::new(
            "PLAN_STATE_INVALID",
            format!("Authority plan journal for {plan_id} is incomplete or mismatched."),
            10,
            false,
        ));
    }
    Ok(record)
}

fn drive_active_jobs(
    runtime_dir: &Path,
    epoch: u64,
    pairing: &ControllerPairingSecret,
    mut hdc: Option<&mut HdcControlPort>,
    cursors: &mut BTreeMap<String, u64>,
    target_lineages: &mut BTreeMap<String, ActiveTargetLineage>,
) -> Result<(), StandaloneError> {
    let mut public = PublicClient::connect(runtime_dir)?;
    let active = public
        .job_list()?
        .into_iter()
        .filter(|job| !job.terminal)
        .collect::<Vec<_>>();
    if active.is_empty() {
        return Ok(());
    }
    let mut controller = ControllerClient::connect(runtime_dir)?;
    for job in active {
        let mut cursor = *cursors.get(&job.job_id).unwrap_or(&0);
        let events = controller.job_events(&job.job_id, cursor)?;
        for event in events {
            match event.kind {
                JobEventKind::StepAdmissionRequested => {
                    let snapshot = event.admission.as_ref().ok_or_else(|| {
                        StandaloneError::new(
                            "ADMISSION_EVENT_INVALID",
                            "A stepAdmissionRequested event contains no admission snapshot.",
                            10,
                            false,
                        )
                    })?;
                    answer_admission(
                        runtime_dir,
                        epoch,
                        pairing,
                        &mut public,
                        &mut controller,
                        snapshot,
                        target_lineages,
                    )?;
                }
                JobEventKind::ManagedControlRequested => {
                    let request = event.control_request.as_ref().ok_or_else(|| {
                        StandaloneError::new(
                            "CONTROL_EVENT_INVALID",
                            "A managedControlRequested event contains no typed request.",
                            10,
                            false,
                        )
                    })?;
                    if let Some(port) = hdc.as_deref_mut() {
                        let record = load_authority_plan(runtime_dir, &job.plan_id)?;
                        let lineage = match target_lineages.get(&job.job_id) {
                            Some(lineage) => lineage.clone(),
                            None => {
                                let loaded = load_job_lineage(runtime_dir, &job.job_id)?;
                                target_lineages.insert(job.job_id.clone(), loaded.clone());
                                loaded
                            }
                        };
                        if lineage.plan_id != record.plan.plan_id
                            || lineage.topology_sha256 != record.topology_sha256
                        {
                            return Err(StandaloneError::new(
                                "TARGET_LINEAGE_CONFLICT",
                                "The active target lineage does not belong to the job's sealed plan.",
                                6,
                                false,
                            ));
                        }
                        let result = port.perform(
                            &mut public,
                            request,
                            &ControlContext {
                                current_device_id: lineage.current_device_id.clone(),
                                profile_id: record.profile_id.clone(),
                                authority_stable_identity_sha256: digest_bytes(
                                    &record.stable_identity_sha256,
                                    "authority stable identity",
                                )?
                                .to_hex(),
                                topology_sha256: lineage.topology_sha256.clone(),
                            },
                        );
                        if result.receipt.accepted
                            && let Some(rebound) = result.rebound_observation
                        {
                            let current_stable = current_stable_identity(
                                &mut public,
                                &rebound.observation_id,
                                &record.profile_id,
                            )?;
                            let advanced = ActiveTargetLineage {
                                plan_id: lineage.plan_id,
                                current_device_id: rebound.observation_id,
                                current_stable_identity_sha256: current_stable.as_bytes().to_vec(),
                                topology_sha256: lineage.topology_sha256,
                                revision: lineage.revision.saturating_add(1),
                            };
                            persist_job_lineage(runtime_dir, &job.job_id, &advanced)?;
                            target_lineages.insert(job.job_id.clone(), advanced);
                        }
                        submit_control_receipt(&mut controller, &result.receipt)?;
                    } else {
                        submit_control_refusal(
                            &mut controller,
                            request,
                            "No exact HDC executable is bound to this authority runtime.",
                        )?;
                    }
                }
                _ => {}
            }
            cursor = event.sequence;
            cursors.insert(job.job_id.clone(), cursor);
        }
    }
    Ok(())
}

fn submit_control_receipt(
    controller: &mut ControllerClient,
    receipt: &SubmitManagedControlReceiptRequest,
) -> Result<(), StandaloneError> {
    let outcome = controller.submit_control_receipt(receipt)?;
    if !outcome.accepted {
        return Err(StandaloneError::new(
            outcome.rejection_code,
            outcome.rejection_message,
            3,
            false,
        ));
    }
    Ok(())
}

fn submit_control_refusal(
    controller: &mut ControllerClient,
    request: &arkforge_ipc::messages::ManagedControlRequest,
    reason: &str,
) -> Result<(), StandaloneError> {
    let receipt = SubmitManagedControlReceiptRequest {
        job_id: request.job_id.clone(),
        request_id: request.request_id.clone(),
        action: request.action,
        accepted: false,
        facts: Vec::new(),
        evidence_sha256: sha256(reason.as_bytes()).as_bytes().to_vec(),
        failure_reason: reason.to_string(),
    };
    submit_control_receipt(controller, &receipt)
}

fn answer_admission(
    runtime_dir: &Path,
    epoch: u64,
    pairing: &ControllerPairingSecret,
    public: &mut PublicClient,
    controller: &mut ControllerClient,
    snapshot: &StepAdmissionSnapshot,
    target_lineages: &mut BTreeMap<String, ActiveTargetLineage>,
) -> Result<(), StandaloneError> {
    let record = load_authority_plan(runtime_dir, &snapshot.plan_id)?;
    let lineage = match target_lineages.get(&snapshot.job_id) {
        Some(lineage) => lineage.clone(),
        None => {
            let loaded = load_job_lineage(runtime_dir, &snapshot.job_id)?;
            target_lineages.insert(snapshot.job_id.clone(), loaded.clone());
            loaded
        }
    };
    if lineage.plan_id != snapshot.plan_id || lineage.topology_sha256 != record.topology_sha256 {
        return submit_permit_refusal(
            controller,
            snapshot,
            epoch,
            "The durable target lineage does not belong to this sealed plan.",
        );
    }
    let admitted_device = recompute_admitted_device_digest(snapshot)?;
    if admitted_device.as_bytes() != snapshot.admitted_device_facts_sha256.as_slice() {
        return submit_permit_refusal(
            controller,
            snapshot,
            epoch,
            "The admission device digest does not match the raw typed identity snapshot.",
        );
    }
    let now = arkforged::rescue::now_epoch_ms()?;
    let expires = snapshot
        .observed_at_epoch_ms
        .saturating_add(snapshot.snapshot_lifetime_ms);
    if now >= expires {
        return submit_permit_refusal(
            controller,
            snapshot,
            epoch,
            "The admission snapshot expired before the authority could permit it.",
        );
    }
    let Some(lineage) =
        resolve_lineage_for_admission(runtime_dir, public, snapshot, &record, &lineage)?
    else {
        return submit_permit_refusal(
            controller,
            snapshot,
            epoch,
            "The exact target lineage does not match one unique current observation.",
        );
    };
    target_lineages.insert(snapshot.job_id.clone(), lineage.clone());
    let current_stable =
        current_stable_identity(public, &lineage.current_device_id, &record.profile_id)?;
    if current_stable.as_bytes() != lineage.current_stable_identity_sha256.as_slice() {
        return submit_permit_refusal(
            controller,
            snapshot,
            epoch,
            "The exact target binding no longer matches the current observation and probe facts.",
        );
    }
    let submission = load_or_create_permit(
        runtime_dir,
        epoch,
        pairing,
        snapshot,
        &record,
        admitted_device,
        now,
        expires,
    )?;
    let outcome = controller.submit_permit(&submission)?;
    if !outcome.accepted {
        return Err(StandaloneError::new(
            outcome.rejection_code,
            outcome.rejection_message,
            3,
            false,
        ));
    }
    Ok(())
}

fn resolve_lineage_for_admission(
    runtime_dir: &Path,
    public: &mut PublicClient,
    snapshot: &StepAdmissionSnapshot,
    record: &AuthorityPlanRecord,
    lineage: &ActiveTargetLineage,
) -> Result<Option<ActiveTargetLineage>, StandaloneError> {
    let observations = public.device_list()?;
    if let Some(current) = observations
        .iter()
        .find(|observation| observation.observation_id == lineage.current_device_id)
    {
        return Ok(observation_matches_admission(current, snapshot).then(|| lineage.clone()));
    }

    // A transport-dispatched reset does not return a managed-control rebound
    // observation to the supervisor. Advance the authority's durable lineage
    // only across the exact mode edge sealed immediately before this step and
    // only to the unique live observation that reproduces every typed identity
    // field in the daemon's admission snapshot.
    if !prior_reboot_authorizes_rebind(&record.plan, snapshot) {
        return Ok(None);
    }
    let mut matches = observations
        .iter()
        .filter(|observation| observation_matches_admission(observation, snapshot));
    let Some(rebound) = matches.next() else {
        return Ok(None);
    };
    if matches.next().is_some() {
        return Ok(None);
    }
    let probe = public.device_probe(&rebound.observation_id, &record.profile_id)?;
    if !observation_matches_admission(&probe.observation, snapshot) {
        return Ok(None);
    }
    let advanced = ActiveTargetLineage {
        plan_id: lineage.plan_id.clone(),
        current_device_id: rebound.observation_id.clone(),
        current_stable_identity_sha256: stable_identity_digest(rebound, &probe).as_bytes().to_vec(),
        topology_sha256: lineage.topology_sha256.clone(),
        revision: lineage.revision.saturating_add(1),
    };
    persist_job_lineage(runtime_dir, &snapshot.job_id, &advanced)?;
    Ok(Some(advanced))
}

fn prior_reboot_authorizes_rebind(plan: &ExecutablePlan, snapshot: &StepAdmissionSnapshot) -> bool {
    let Some(index) = plan
        .public_steps
        .iter()
        .position(|step| step.step_id == snapshot.step_id)
    else {
        return false;
    };
    let Some(previous) = index
        .checked_sub(1)
        .and_then(|previous| plan.public_steps.get(previous))
    else {
        return false;
    };
    let current = &plan.public_steps[index];
    previous.kind == "reboot"
        && !previous.expected_mode_before.is_empty()
        && previous.expected_mode_before != previous.expected_mode_after
        && previous.expected_mode_after == snapshot.observed_mode
        && current.expected_mode_before == snapshot.observed_mode
        && current.expected_mode_after == snapshot.observed_mode
        && digest_bytes(&snapshot.private_action_sha256, "private action digest")
            .is_ok_and(|digest| digest.to_hex() == current.private_action_sha256)
}

fn observation_matches_admission(
    observation: &DeviceObservationView,
    snapshot: &StepAdmissionSnapshot,
) -> bool {
    observation.mode == snapshot.observed_mode
        && digest_bytes(&snapshot.topology_sha256, "topology digest")
            .is_ok_and(|digest| digest.to_hex() == observation.topology_sha256)
        && digest_bytes(&snapshot.descriptor_sha256, "descriptor digest")
            .is_ok_and(|digest| digest.to_hex() == observation.descriptor_sha256)
        && digest_bytes(&snapshot.serial_sha256, "serial digest")
            .is_ok_and(|digest| digest.to_hex() == observation.serial_sha256)
        && observation.serial_evidence_kind == snapshot.serial_evidence_kind
        && observation.protocol_identity == snapshot.protocol_identity
        && observation.identity_strength == snapshot.identity_strength
        && observation.malformed_descriptor == snapshot.malformed_descriptor
}

fn submit_permit_refusal(
    controller: &mut ControllerClient,
    snapshot: &StepAdmissionSnapshot,
    epoch: u64,
    reason: &str,
) -> Result<(), StandaloneError> {
    let outcome = controller.submit_permit(&SubmitStepPermitRequest {
        job_id: snapshot.job_id.clone(),
        request_id: snapshot.request_id.clone(),
        permit_cbor: Vec::new(),
        integrity_tag: Vec::new(),
        pairing_epoch: epoch,
        refusal: reason.to_string(),
    })?;
    if !outcome.accepted {
        return Err(StandaloneError::new(
            outcome.rejection_code,
            outcome.rejection_message,
            3,
            false,
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn load_or_create_permit(
    runtime_dir: &Path,
    epoch: u64,
    pairing: &ControllerPairingSecret,
    snapshot: &StepAdmissionSnapshot,
    record: &AuthorityPlanRecord,
    admitted_device: Sha256Digest,
    issued_at: u64,
    expires_at: u64,
) -> Result<SubmitStepPermitRequest, StandaloneError> {
    let root = runtime_dir
        .join("authority")
        .join("permits")
        .join(epoch.to_string());
    std::fs::create_dir_all(&root).map_err(|error| internal("create the permit journal", error))?;
    protect_path(&root, true).map_err(|error| internal("protect the permit journal", error))?;
    let path = root.join(format!("{}.permit", sha256(snapshot.request_id.as_bytes())));
    if path.exists() {
        let persisted =
            std::fs::read(&path).map_err(|error| internal("read a durable permit", error))?;
        return decode_persisted_permit(snapshot, epoch, &persisted);
    }

    let plan_digest = digest_bytes(&snapshot.plan_sha256, "plan digest")?;
    if plan_digest.to_hex() != record.plan.plan_sha256 {
        return Err(permit_invalid(
            "admission plan digest differs from authority plan",
        ));
    }
    let permit_id = PermitId::new(format!(
        "CLI-PERMIT-{}",
        &sha256(format!("{epoch}:{}", snapshot.request_id).as_bytes()).to_hex()[..24]
    ))
    .map_err(permit_invalid)?;
    let binding = AuthorityBindingRef {
        authority_namespace: AuthorityNamespace::new("arkforge.cli").map_err(permit_invalid)?,
        binding_id: OpaqueId::new(&record.binding_id).map_err(permit_invalid)?,
        binding_revision: 1,
        stable_identity_digest: digest_bytes(
            &record.stable_identity_sha256,
            "stable identity digest",
        )?,
    };
    let permit = StepPermit {
        permit_id,
        authority_namespace: AuthorityNamespace::new("arkforge.cli").map_err(permit_invalid)?,
        controller_session_id: ControllerSessionId::new(format!("CLI-SESSION-{epoch}"))
            .map_err(permit_invalid)?,
        job_id: JobId::new(&snapshot.job_id).map_err(permit_invalid)?,
        plan_id: PlanId::new(&snapshot.plan_id).map_err(permit_invalid)?,
        plan_digest,
        step_id: StepId::new(&snapshot.step_id).map_err(permit_invalid)?,
        attempt_id: AttemptId::new(&snapshot.attempt_id).map_err(permit_invalid)?,
        public_step_digest: digest_bytes(&snapshot.public_step_sha256, "public step digest")?,
        private_action_digest: digest_bytes(
            &snapshot.private_action_sha256,
            "private action digest",
        )?,
        effect_set_digest: digest_bytes(&snapshot.effect_set_sha256, "effect set digest")?,
        authority_binding: binding,
        admitted_device_facts_digest: admitted_device,
        issued_at_epoch_ms: issued_at,
        expires_at_epoch_ms: expires_at,
        single_use: true,
        integrity_tag: PermitIntegrityTag {
            epoch: PairingEpoch(epoch),
            tag: Sha256Digest::from_bytes([0; 32]),
        },
    };
    let permit_cbor = permit.signing_body().map_err(permit_invalid)?;
    let tag = mint_integrity_tag(&permit, pairing).map_err(permit_invalid)?;
    let submission = SubmitStepPermitRequest {
        job_id: snapshot.job_id.clone(),
        request_id: snapshot.request_id.clone(),
        permit_cbor,
        integrity_tag: tag.tag.as_bytes().to_vec(),
        pairing_epoch: epoch,
        refusal: String::new(),
    };
    let persisted = encode_persisted_permit(&submission);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| internal("create a durable permit", error))?;
    protect_path(&path, false).map_err(|error| internal("protect a durable permit", error))?;
    file.write_all(&persisted)
        .and_then(|_| file.sync_all())
        .map_err(|error| internal("commit a durable permit before submission", error))?;
    Ok(submission)
}

fn encode_persisted_permit(submission: &SubmitStepPermitRequest) -> Vec<u8> {
    let mut out = Vec::new();
    wire::write_bytes(&mut out, 1, &submission.permit_cbor);
    wire::write_bytes(&mut out, 2, &submission.integrity_tag);
    wire::write_uint64(&mut out, 3, submission.pairing_epoch);
    out
}

fn decode_persisted_permit(
    snapshot: &StepAdmissionSnapshot,
    epoch: u64,
    input: &[u8],
) -> Result<SubmitStepPermitRequest, StandaloneError> {
    let mut permit_cbor = Vec::new();
    let mut integrity_tag = Vec::new();
    let mut stored_epoch = 0;
    let mut reader = wire::Reader::new(input);
    while let Some((field, value)) = reader
        .next_field()
        .map_err(|error| internal("decode a durable permit", error))?
    {
        match field {
            1 => permit_cbor = value.as_bytes().map_err(status_decode)?.to_vec(),
            2 => integrity_tag = value.as_bytes().map_err(status_decode)?.to_vec(),
            3 => stored_epoch = value.as_u64().map_err(status_decode)?,
            _ => {}
        }
    }
    if permit_cbor.is_empty() || integrity_tag.len() != 32 || stored_epoch != epoch {
        return Err(StandaloneError::new(
            "PERMIT_JOURNAL_INVALID",
            "A durable permit is incomplete or belongs to another pairing epoch.",
            10,
            false,
        ));
    }
    Ok(SubmitStepPermitRequest {
        job_id: snapshot.job_id.clone(),
        request_id: snapshot.request_id.clone(),
        permit_cbor,
        integrity_tag,
        pairing_epoch: epoch,
        refusal: String::new(),
    })
}

fn current_stable_identity(
    public: &mut PublicClient,
    device: &str,
    profile: &str,
) -> Result<Sha256Digest, StandaloneError> {
    let observations = public.device_list()?;
    let observation = observations
        .iter()
        .find(|candidate| candidate.observation_id == device)
        .ok_or_else(|| {
            StandaloneError::new(
                "OBSERVATION_NOT_FOUND",
                format!("No current observation exactly matches {device}."),
                5,
                true,
            )
        })?;
    let probe = public.device_probe(device, profile)?;
    Ok(stable_identity_digest(observation, &probe))
}

fn stable_identity_digest(
    observation: &DeviceObservationView,
    probe: &DeviceProbeView,
) -> Sha256Digest {
    let mut stable = Vec::new();
    stable.extend_from_slice(b"arkforge.cli-stable-identity/v2");
    append_stable_observation(&mut stable, "discovery", observation);
    append_stable_observation(&mut stable, "same-handle-probe", &probe.observation);
    append_stable_field(&mut stable, "probe.profile", probe.profile_id.as_bytes());
    for fact in &probe.protocol_facts {
        append_stable_field(&mut stable, "probe.fact.key", fact.key.as_bytes());
        append_stable_field(&mut stable, "probe.fact.value", fact.value.as_bytes());
    }
    sha256(&stable)
}

fn append_stable_observation(
    stable: &mut Vec<u8>,
    source: &str,
    observation: &DeviceObservationView,
) {
    append_stable_field(stable, "observation.source", source.as_bytes());
    append_stable_field(
        stable,
        "observation.id",
        observation.observation_id.as_bytes(),
    );
    append_stable_field(stable, "observation.mode", observation.mode.as_bytes());
    append_stable_field(
        stable,
        "observation.topology",
        observation.topology_sha256.as_bytes(),
    );
    append_stable_field(
        stable,
        "observation.descriptor",
        observation.descriptor_sha256.as_bytes(),
    );
    append_stable_field(
        stable,
        "observation.serial",
        observation.serial_sha256.as_bytes(),
    );
    append_stable_field(
        stable,
        "observation.serial-evidence-kind",
        observation.serial_evidence_kind.as_bytes(),
    );
    append_stable_field(
        stable,
        "observation.identity-strength",
        observation.identity_strength.as_bytes(),
    );
    append_stable_field(
        stable,
        "observation.malformed-descriptor",
        &[u8::from(observation.malformed_descriptor)],
    );
    for fact in &observation.protocol_identity {
        append_stable_field(stable, "observation.fact.key", fact.key.as_bytes());
        append_stable_field(stable, "observation.fact.value", fact.value.as_bytes());
    }
}

fn append_stable_field(stable: &mut Vec<u8>, label: &str, value: &[u8]) {
    stable.extend_from_slice(&(label.len() as u64).to_be_bytes());
    stable.extend_from_slice(label.as_bytes());
    stable.extend_from_slice(&(value.len() as u64).to_be_bytes());
    stable.extend_from_slice(value);
}

fn recompute_admitted_device_digest(
    snapshot: &StepAdmissionSnapshot,
) -> Result<Sha256Digest, StandaloneError> {
    use arkforge_transport::{
        DeviceObservation, IdentityEvidenceStrength, ProtocolIdentityFact, SerialEvidence,
    };
    let serial_evidence = match snapshot.serial_evidence_kind.as_str() {
        "absent" if snapshot.serial_sha256.is_empty() => SerialEvidence::Absent,
        "descriptor" => SerialEvidence::Descriptor {
            digest: digest_bytes(&snapshot.serial_sha256, "serial digest")?,
        },
        "protocolUnique" => SerialEvidence::ProtocolUnique {
            digest: digest_bytes(&snapshot.serial_sha256, "serial digest")?,
        },
        _ => return Err(permit_invalid("admission serial evidence is malformed")),
    };
    let observation = DeviceObservation {
        observation_id: arkforge_core::ids::ObservationId::new("CLI-ADMISSION-SNAPSHOT")
            .map_err(permit_invalid)?,
        observed_at_epoch_ms: snapshot.observed_at_epoch_ms,
        mode: arkforge_core::DeviceMode::new(&snapshot.observed_mode).map_err(permit_invalid)?,
        topology_digest: digest_bytes(&snapshot.topology_sha256, "topology digest")?,
        descriptor_digest: digest_bytes(&snapshot.descriptor_sha256, "descriptor digest")?,
        serial_evidence,
        protocol_identity: snapshot
            .protocol_identity
            .iter()
            .map(|fact| {
                Ok(ProtocolIdentityFact {
                    key: OpaqueId::new(&fact.key).map_err(permit_invalid)?,
                    value: fact.value.clone(),
                })
            })
            .collect::<Result<Vec<_>, StandaloneError>>()?,
        provider_candidates: Vec::new(),
        identity_strength: IdentityEvidenceStrength::parse(&snapshot.identity_strength)
            .ok_or_else(|| permit_invalid("admission identity strength is unknown"))?,
        malformed_descriptor: snapshot.malformed_descriptor,
    };
    observation.admission_facts_digest().map_err(permit_invalid)
}

fn digest_bytes(bytes: &[u8], name: &str) -> Result<Sha256Digest, StandaloneError> {
    if bytes.len() != 32 {
        return Err(permit_invalid(format!("{name} is not 32 bytes")));
    }
    let mut digest = [0u8; 32];
    digest.copy_from_slice(bytes);
    Ok(Sha256Digest::from_bytes(digest))
}

fn permit_invalid(error: impl std::fmt::Display) -> StandaloneError {
    StandaloneError::new(
        "PERMIT_INPUT_INVALID",
        format!("Cannot construct an exact step permit: {error}"),
        10,
        false,
    )
}

fn persist_binding(
    runtime_dir: &Path,
    binding_id: &str,
    device: &str,
    profile: &str,
    stable_identity: &str,
) -> Result<(), StandaloneError> {
    let root = runtime_dir.join("authority").join("bindings");
    std::fs::create_dir_all(&root)
        .map_err(|error| internal("create the authority binding journal", error))?;
    protect_path(&root, true)
        .map_err(|error| internal("protect the authority binding journal", error))?;
    let path = root.join(format!("{binding_id}.binding"));
    let record = format!(
        "schema=arkforge.cli-target-binding/v1\nbinding_id={binding_id}\nrevision=1\ndevice={device}\nprofile={profile}\nstable_identity_sha256={stable_identity}\n"
    );
    if path.exists() {
        let existing = std::fs::read_to_string(&path)
            .map_err(|error| internal("read the durable target binding", error))?;
        if existing != record {
            return Err(StandaloneError::new(
                "TARGET_BINDING_CONFLICT",
                format!("Durable binding {binding_id} disagrees with the current target facts."),
                6,
                false,
            ));
        }
        return Ok(());
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| internal("create the durable target binding", error))?;
    protect_path(&path, false)
        .map_err(|error| internal("protect the durable target binding", error))?;
    file.write_all(record.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|error| internal("commit the durable target binding", error))
}

fn encode_status(status: &DaemonStatus) -> Vec<u8> {
    let mut out = Vec::new();
    wire::write_uint32(&mut out, 1, status.supervisor_pid);
    wire::write_uint32(&mut out, 2, status.daemon_pid);
    wire::write_uint64(&mut out, 3, status.epoch);
    wire::write_uint32(&mut out, 4, status.protocol_major);
    wire::write_uint32(&mut out, 5, status.protocol_minor);
    wire::write_string(&mut out, 6, &status.daemon_version);
    wire::write_bool(&mut out, 7, status.mechanics_ready);
    wire::write_uint64(&mut out, 8, status.active_jobs as u64);
    for blocker in &status.blockers {
        wire::write_string(&mut out, 9, blocker);
    }
    wire::write_bool(&mut out, 10, status.authority_support_available);
    wire::write_bool(&mut out, 11, status.hdc_bound);
    wire::write_string(&mut out, 12, &status.hdc_sha256);
    wire::write_string(&mut out, 13, &status.hardware_campaign);
    out
}

fn decode_status(input: &[u8]) -> Result<DaemonStatus, StandaloneError> {
    let mut status = DaemonStatus {
        supervisor_pid: 0,
        daemon_pid: 0,
        epoch: 0,
        protocol_major: 0,
        protocol_minor: 0,
        daemon_version: String::new(),
        mechanics_ready: false,
        authority_support_available: false,
        hdc_bound: false,
        hdc_sha256: String::new(),
        hardware_campaign: String::new(),
        active_jobs: 0,
        blockers: Vec::new(),
    };
    let mut reader = wire::Reader::new(input);
    while let Some((field, value)) = reader
        .next_field()
        .map_err(|error| internal("decode supervisor status", error))?
    {
        match field {
            1 => status.supervisor_pid = value.as_u64().map_err(status_decode)? as u32,
            2 => status.daemon_pid = value.as_u64().map_err(status_decode)? as u32,
            3 => status.epoch = value.as_u64().map_err(status_decode)?,
            4 => status.protocol_major = value.as_u64().map_err(status_decode)? as u32,
            5 => status.protocol_minor = value.as_u64().map_err(status_decode)? as u32,
            6 => status.daemon_version = value.as_str(6).map_err(status_decode)?.to_string(),
            7 => status.mechanics_ready = value.as_bool().map_err(status_decode)?,
            8 => status.active_jobs = value.as_u64().map_err(status_decode)? as usize,
            9 => status
                .blockers
                .push(value.as_str(9).map_err(status_decode)?.to_string()),
            10 => status.authority_support_available = value.as_bool().map_err(status_decode)?,
            11 => status.hdc_bound = value.as_bool().map_err(status_decode)?,
            12 => status.hdc_sha256 = value.as_str(12).map_err(status_decode)?.to_string(),
            13 => status.hardware_campaign = value.as_str(13).map_err(status_decode)?.to_string(),
            _ => {}
        }
    }
    if status.supervisor_pid == 0
        || status.daemon_pid == 0
        || status.epoch == 0
        || status.protocol_major == 0
        || status.daemon_version.is_empty()
    {
        return Err(StandaloneError::new(
            "SUPERVISOR_RESPONSE_INVALID",
            "The authority supervisor returned an incomplete status response.",
            10,
            false,
        ));
    }
    Ok(status)
}

fn decode_request(input: &[u8]) -> Result<(String, Vec<String>), StandaloneError> {
    let mut command = String::new();
    let mut arguments = Vec::new();
    let mut reader = wire::Reader::new(input);
    while let Some((field, value)) = reader
        .next_field()
        .map_err(|error| internal("decode supervisor request", error))?
    {
        match field {
            1 => command = value.as_str(1).map_err(status_decode)?.to_string(),
            2 => arguments.push(value.as_str(2).map_err(status_decode)?.to_string()),
            _ => {}
        }
    }
    if command.is_empty() {
        return Err(StandaloneError::new(
            "SUPERVISOR_REQUEST_INVALID",
            "The supervisor request contains no command.",
            2,
            false,
        ));
    }
    Ok((command, arguments))
}

fn write_reply(
    stream: &mut LocalStream,
    result: Result<Vec<u8>, StandaloneError>,
) -> Result<(), StandaloneError> {
    let mut out = Vec::new();
    match result {
        Ok(payload) => {
            wire::write_uint32(&mut out, 1, 1);
            wire::write_bytes(&mut out, 4, &payload);
        }
        Err(error) => {
            wire::write_uint32(&mut out, 1, 2);
            wire::write_string(&mut out, 2, &error.code);
            wire::write_string(&mut out, 3, &error.message);
            wire::write_uint32(&mut out, 5, error.exit_code as u32);
            wire::write_bool(&mut out, 6, error.retryable);
            for token in &error.required_acknowledgements {
                wire::write_string(&mut out, 7, token);
            }
        }
    }
    write_frame(stream, &out).map_err(|error| internal("write supervisor response", error))
}

fn decode_reply(input: &[u8]) -> Result<Vec<u8>, StandaloneError> {
    let mut disposition = 0;
    let mut code = String::new();
    let mut message = String::new();
    let mut payload = Vec::new();
    let mut exit_code = 10;
    let mut retryable = false;
    let mut required_acknowledgements = Vec::new();
    let mut reader = wire::Reader::new(input);
    while let Some((field, value)) = reader
        .next_field()
        .map_err(|error| internal("decode supervisor response", error))?
    {
        match field {
            1 => disposition = value.as_u64().map_err(status_decode)?,
            2 => code = value.as_str(2).map_err(status_decode)?.to_string(),
            3 => message = value.as_str(3).map_err(status_decode)?.to_string(),
            4 => payload = value.as_bytes().map_err(status_decode)?.to_vec(),
            5 => exit_code = value.as_u64().map_err(status_decode)? as i32,
            6 => retryable = value.as_bool().map_err(status_decode)?,
            7 => {
                required_acknowledgements.push(value.as_str(7).map_err(status_decode)?.to_string())
            }
            _ => {}
        }
    }
    match disposition {
        1 => Ok(payload),
        2 if !code.is_empty() => Err(StandaloneError::new(code, message, exit_code, retryable)
            .with_required_acknowledgements(required_acknowledgements)),
        _ => Err(StandaloneError::new(
            "SUPERVISOR_RESPONSE_INVALID",
            "The authority supervisor returned an incomplete response envelope.",
            10,
            false,
        )),
    }
}

fn status_decode(error: impl std::fmt::Display) -> StandaloneError {
    StandaloneError::new(
        "SUPERVISOR_RESPONSE_INVALID",
        format!("The authority supervisor response is invalid: {error}"),
        10,
        false,
    )
}

/// Creates the per-user runtime root and applies the host owner-only boundary.
/// Offline artifact import uses this before the daemon exists so Windows CAS
/// children inherit the same protected DACL.
pub fn prepare_storage(runtime_dir: &Path) -> Result<(), StandaloneError> {
    std::fs::create_dir_all(runtime_dir)
        .map_err(|error| internal("create the runtime directory", error))?;
    protect_path(runtime_dir, true)
        .map_err(|error| internal("protect the runtime directory", error))
}

fn prepare_runtime(runtime_dir: &Path) -> Result<(), StandaloneError> {
    prepare_storage(runtime_dir)
}

fn fresh_pairing(runtime_dir: &Path) -> Result<(u64, Vec<u8>), StandaloneError> {
    let mut secret = [0u8; 32];
    fill_random(&mut secret)
        .map_err(|error| internal("read host randomness for pairing", error))?;
    let authority = runtime_dir.join("authority");
    std::fs::create_dir_all(&authority)
        .map_err(|error| internal("create pairing epoch journal", error))?;
    protect_path(&authority, true)
        .map_err(|error| internal("protect pairing epoch journal", error))?;
    let path = authority.join("pairing-epoch");
    let previous = if path.exists() {
        std::fs::read_to_string(&path)
            .map_err(|error| internal("read pairing epoch journal", error))?
            .trim()
            .parse::<u64>()
            .map_err(|_| {
                internal(
                    "decode pairing epoch journal",
                    "epoch is not an unsigned integer",
                )
            })?
    } else {
        0
    };
    let epoch = previous.checked_add(1).ok_or_else(|| {
        StandaloneError::new(
            "PAIRING_EPOCH_EXHAUSTED",
            "The durable pairing epoch cannot advance; this runtime must not execute.",
            10,
            false,
        )
    })?;
    let transaction_id = &sha256(&secret).to_hex()[..16];
    let temporary = authority.join(format!(
        ".pairing-epoch.{}.{}.tmp",
        std::process::id(),
        transaction_id
    ));
    let mut journal = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| internal("create pairing epoch transaction", error))?;
    protect_path(&temporary, false)
        .map_err(|error| internal("protect pairing epoch transaction", error))?;
    journal
        .write_all(epoch.to_string().as_bytes())
        .and_then(|_| journal.sync_all())
        .map_err(|error| internal("commit pairing epoch bytes", error))?;
    replace_file(&temporary, &path).map_err(|error| internal("publish pairing epoch", error))?;
    sync_directory(&authority).map_err(|error| internal("sync pairing epoch directory", error))?;
    Ok((epoch, secret.to_vec()))
}

fn spawn_daemon(
    runtime_dir: &Path,
    options: &DaemonOptions,
    epoch: u64,
    secret: &[u8],
    foreground_output: bool,
) -> Result<Child, StandaloneError> {
    let daemon = sibling_daemon()?;
    let mut command = Command::new(&daemon);
    command
        .arg("--runtime-dir")
        .arg(runtime_dir)
        .arg("--pair-from-stdin")
        .arg(epoch.to_string())
        .stdin(Stdio::piped());
    for profile in &options.profile_files {
        command.arg("--profile").arg(profile);
    }
    if let Some(campaign) = &options.hardware_campaign {
        command.arg("--hardware-campaign").arg(campaign);
    }
    if foreground_output {
        command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    } else {
        command.stdout(Stdio::null()).stderr(Stdio::null());
    }
    let mut child = command.spawn().map_err(|error| {
        StandaloneError::new(
            "MECHANICS_DAEMON_UNAVAILABLE",
            format!("Cannot start {}: {error}", daemon.display()),
            5,
            true,
        )
    })?;
    let stdin = child.stdin.as_mut().ok_or_else(|| {
        StandaloneError::new(
            "PAIRING_PIPE_UNAVAILABLE",
            "arkforged did not expose its inherited pairing pipe.",
            10,
            false,
        )
    })?;
    stdin
        .write_all(secret)
        .and_then(|_| stdin.flush())
        .map_err(|error| internal("send the pairing secret through the inherited pipe", error))?;
    Ok(child)
}

fn validate_tool_bindings(options: &DaemonOptions) -> Result<(), StandaloneError> {
    if let (Some(hdc), Some(expected)) = (&options.hdc, &options.expect_hdc_sha256) {
        let actual = arkforged::dispatch::executable_digest(hdc).map_err(|_| {
            StandaloneError::new(
                "HDC_BINDING_REFUSED",
                "The exact HDC executable could not be opened and hashed.",
                3,
                false,
            )
        })?;
        if actual.to_hex() != *expected {
            return Err(StandaloneError::new(
                "HDC_DIGEST_MISMATCH",
                format!(
                    "The HDC executable digest is {actual}, not the caller expectation {expected}."
                ),
                3,
                false,
            ));
        }
    }
    if options.require_release_signing {
        let daemon = sibling_daemon()?;
        #[cfg(windows)]
        {
            verify_windows_signature(&daemon, "mechanics daemon")?;
            if let Some(hdc) = &options.hdc {
                verify_windows_signature(hdc, "HDC executable")?;
            }
        }
        #[cfg(not(windows))]
        {
            let signed = arkforged::packaging::read_file(&daemon).map_err(|error| {
                StandaloneError::new(
                    "RELEASE_SIGNING_REQUIRED",
                    format!("Cannot inspect the mechanics daemon signing contract: {error}"),
                    3,
                    false,
                )
            })?;
            let violations = signed.violations(arkforged::packaging::ContractMode::Release);
            if !violations.is_empty() {
                return Err(StandaloneError::new(
                    "RELEASE_SIGNING_REQUIRED",
                    format!(
                        "The mechanics daemon has {} release-signing contract violation(s).",
                        violations.len()
                    ),
                    3,
                    false,
                ));
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn verify_windows_signature(path: &Path, role: &str) -> Result<(), StandaloneError> {
    arkforge_platform::verify_trusted_signature(path).map_err(|error| {
        StandaloneError::new(
            "RELEASE_SIGNING_REQUIRED",
            format!(
                "Windows does not trust the selected {role} {}: {error}",
                path.display()
            ),
            3,
            false,
        )
    })
}

fn sibling_daemon() -> Result<PathBuf, StandaloneError> {
    let executable =
        std::env::current_exe().map_err(|error| internal("identify arkforge", error))?;
    let path = executable
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("arkforged{}", std::env::consts::EXE_SUFFIX));
    if !path.is_file() {
        return Err(StandaloneError::new(
            "MECHANICS_DAEMON_UNAVAILABLE",
            format!(
                "The canonical mechanics daemon is not installed beside arkforge at {}.",
                path.display()
            ),
            5,
            false,
        ));
    }
    Ok(path)
}

fn wait_for_daemon(
    runtime_dir: &Path,
    daemon: &mut Child,
) -> Result<PublicRuntimeInfo, StandaloneError> {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        if let Some(exit) = daemon
            .try_wait()
            .map_err(|error| internal("observe arkforged startup", error))?
        {
            return Err(StandaloneError::new(
                "MECHANICS_DAEMON_EXITED",
                format!("arkforged exited during startup with {exit}."),
                10,
                true,
            ));
        }
        if let Ok(client) = PublicClient::connect(runtime_dir) {
            return Ok(client.runtime_info().clone());
        }
        if Instant::now() >= deadline {
            return Err(StandaloneError::new(
                "MECHANICS_DAEMON_START_TIMEOUT",
                "arkforged did not accept a versioned public session within 10 seconds.",
                5,
                true,
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn internal(context: &str, error: impl std::fmt::Display) -> StandaloneError {
    StandaloneError::new(
        "SUPERVISOR_IO_FAILED",
        format!("Cannot {context}: {error}"),
        10,
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use arkforge_ipc::messages::KeyValue;

    fn stable_observation(observed_at_epoch_ms: u64) -> DeviceObservationView {
        DeviceObservationView {
            observation_id: "USB-2207-5000-01120000".into(),
            observed_at_epoch_ms,
            mode: "hdc-normal".into(),
            topology_sha256: sha256(b"topology").to_hex(),
            descriptor_sha256: sha256(b"descriptor").to_hex(),
            identity_strength: "serialAndTopology".into(),
            malformed_descriptor: false,
            protocol_identity: vec![KeyValue {
                key: "profile".into(),
                value: "org.openharmony.dayu200".into(),
            }],
            serial_sha256: sha256(b"serial").to_hex(),
            serial_evidence_kind: "descriptor".into(),
        }
    }

    fn stable_probe(observed_at_epoch_ms: u64, facts_sha256: &str) -> DeviceProbeView {
        DeviceProbeView {
            observation: stable_observation(observed_at_epoch_ms),
            protocol_facts: vec![KeyValue {
                key: "transport".into(),
                value: "arkforge.transport.usb".into(),
            }],
            profile_id: "org.openharmony.dayu200".into(),
            facts_sha256: facts_sha256.into(),
        }
    }

    fn admission_for(observation: &DeviceObservationView) -> StepAdmissionSnapshot {
        StepAdmissionSnapshot {
            step_id: "STEP-022".into(),
            private_action_sha256: sha256(b"postflight action").as_bytes().to_vec(),
            observed_mode: observation.mode.clone(),
            topology_sha256: Sha256Digest::parse_hex(&observation.topology_sha256)
                .unwrap()
                .as_bytes()
                .to_vec(),
            descriptor_sha256: Sha256Digest::parse_hex(&observation.descriptor_sha256)
                .unwrap()
                .as_bytes()
                .to_vec(),
            serial_sha256: Sha256Digest::parse_hex(&observation.serial_sha256)
                .unwrap()
                .as_bytes()
                .to_vec(),
            serial_evidence_kind: observation.serial_evidence_kind.clone(),
            protocol_identity: observation.protocol_identity.clone(),
            identity_strength: observation.identity_strength.clone(),
            malformed_descriptor: observation.malformed_descriptor,
            ..StepAdmissionSnapshot::default()
        }
    }

    fn pending_authority_assessment() -> arkforge_ipc::messages::Assessment {
        arkforge_ipc::messages::Assessment {
            availability: "unavailable".into(),
            unavailable_reason: "pending authority support".into(),
            mechanics_maturity_state: "hardwareCampaign".into(),
            unknowns: vec![KeyValue {
                key: "RK-A01".into(),
                value: "authority implementation is hardwareGated".into(),
            }],
            evidence_requirements: vec![KeyValue {
                key: "EVR-RK-A01".into(),
                value: "close authority support".into(),
            }],
            ..arkforge_ipc::messages::Assessment::default()
        }
    }

    #[test]
    fn stable_identity_ignores_fresh_probe_time_but_binds_explicit_identity_facts() {
        let discovery = stable_observation(100);
        let first = stable_probe(101, &sha256(b"full observation at 101").to_hex());
        let fresh = stable_probe(202, &sha256(b"full observation at 202").to_hex());

        assert_eq!(
            stable_identity_digest(&discovery, &first),
            stable_identity_digest(&discovery, &fresh),
            "a timestamp-only fresh probe must preserve the sealed target binding"
        );

        let mut replaced = fresh.clone();
        replaced.observation.descriptor_sha256 = sha256(b"replacement descriptor").to_hex();
        assert_ne!(
            stable_identity_digest(&discovery, &first),
            stable_identity_digest(&discovery, &replaced),
            "same-handle replacement evidence must still invalidate the binding"
        );

        let mut changed_protocol = fresh;
        changed_protocol.protocol_facts[0].value = "arkforge.transport.replacement".into();
        assert_ne!(
            stable_identity_digest(&discovery, &first),
            stable_identity_digest(&discovery, &changed_protocol),
            "provider protocol facts remain effect-relevant binding material"
        );
    }

    #[test]
    fn postflight_rebind_requires_the_immediately_prior_sealed_reboot_edge() {
        let observation = stable_observation(100);
        let snapshot = admission_for(&observation);
        let mut plan = ExecutablePlan {
            public_steps: vec![
                arkforge_ipc::messages::PublicStep {
                    step_id: "STEP-021".into(),
                    kind: "reboot".into(),
                    expected_mode_before: "rockusb-loader".into(),
                    expected_mode_after: "hdc-normal".into(),
                    ..arkforge_ipc::messages::PublicStep::default()
                },
                arkforge_ipc::messages::PublicStep {
                    step_id: "STEP-022".into(),
                    kind: "postflightProbe".into(),
                    expected_mode_before: "hdc-normal".into(),
                    expected_mode_after: "hdc-normal".into(),
                    private_action_sha256: sha256(b"postflight action").to_hex(),
                    ..arkforge_ipc::messages::PublicStep::default()
                },
            ],
            ..ExecutablePlan::default()
        };

        assert!(prior_reboot_authorizes_rebind(&plan, &snapshot));

        plan.public_steps[0].kind = "verifyTarget".into();
        assert!(!prior_reboot_authorizes_rebind(&plan, &snapshot));
        plan.public_steps[0].kind = "reboot".into();
        plan.public_steps[0].expected_mode_after = "rockusb-loader".into();
        assert!(!prior_reboot_authorizes_rebind(&plan, &snapshot));
    }

    #[test]
    fn postflight_rebind_matches_every_typed_identity_field_but_not_probe_time() {
        let observation = stable_observation(100);
        let snapshot = admission_for(&observation);
        let mut fresh = observation.clone();
        fresh.observation_id = "USB-2207-5000-fresh".into();
        fresh.observed_at_epoch_ms = 200;

        assert!(observation_matches_admission(&fresh, &snapshot));

        fresh.descriptor_sha256 = sha256(b"replacement descriptor").to_hex();
        assert!(!observation_matches_admission(&fresh, &snapshot));
        fresh = observation.clone();
        fresh.protocol_identity[0].value = "org.openharmony.replacement".into();
        assert!(!observation_matches_admission(&fresh, &snapshot));
    }

    #[test]
    fn assessment_closes_only_the_resolved_authority_blocker() {
        let mut ready = pending_authority_assessment();
        close_resolved_authority_blocker(&mut ready);
        assert_eq!(ready.availability, "available");
        assert!(ready.unavailable_reason.is_empty());
        assert!(ready.unknowns.is_empty());
        assert!(ready.evidence_requirements.is_empty());

        let mut still_blocked = pending_authority_assessment();
        still_blocked.unknowns.push(KeyValue {
            key: "RK-V10".into(),
            value: "device mode is not declared".into(),
        });
        close_resolved_authority_blocker(&mut still_blocked);
        assert_eq!(still_blocked.availability, "unavailable");
        assert_eq!(still_blocked.unknowns.len(), 1);
        assert_eq!(still_blocked.unknowns[0].key, "RK-V10");
    }

    #[test]
    fn daemon_options_require_exact_hdc_path_and_digest_together() {
        let absolute_hdc = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(DaemonOptions::parse(&["--hdc".into(), absolute_hdc.clone()]).is_err());
        assert!(DaemonOptions::parse(&["--expect-hdc-sha256".into(), "0".repeat(64),]).is_err());
        assert!(
            DaemonOptions::parse(&[
                "--hdc".into(),
                "relative-hdc".into(),
                "--expect-hdc-sha256".into(),
                "0".repeat(64),
            ])
            .is_err()
        );
        assert!(
            DaemonOptions::parse(&[
                "--hdc".into(),
                absolute_hdc,
                "--expect-hdc-sha256".into(),
                "0".repeat(64),
            ])
            .is_ok()
        );
        assert!(
            DaemonOptions::parse(&[
                "--hardware-campaign".into(),
                "CLI-AC-28".into(),
                "--hardware-campaign".into(),
                "CLI-AC-29".into(),
            ])
            .is_err()
        );
    }

    #[test]
    fn status_protocol_keeps_an_empty_blocker_field() {
        let original = DaemonStatus {
            supervisor_pid: 12,
            daemon_pid: 13,
            epoch: 14,
            protocol_major: 1,
            protocol_minor: 0,
            daemon_version: "0.1.0".into(),
            mechanics_ready: true,
            authority_support_available: false,
            hdc_bound: true,
            hdc_sha256: sha256(b"hdc").to_hex(),
            hardware_campaign: "CLI-AC-28".into(),
            active_jobs: 0,
            blockers: Vec::new(),
        };
        let status = decode_status(&encode_status(&original)).unwrap();
        assert_eq!(status, original);
        assert_eq!(status.supervisor_pid, 12);
        assert_eq!(status.daemon_pid, 13);
        assert_eq!(status.epoch, 14);
        assert!(status.mechanics_ready);
        assert!(status.blockers.is_empty());
    }

    #[test]
    fn stop_refusal_preserves_active_job_count() {
        let mut encoded = Vec::new();
        wire::write_uint32(&mut encoded, 1, 2);
        wire::write_string(&mut encoded, 2, "ACTIVE_JOBS");
        wire::write_string(&mut encoded, 3, "The runtime has 2 active jobs.");
        wire::write_uint32(&mut encoded, 5, 6);
        wire::write_bool(&mut encoded, 6, true);
        let error = decode_reply(&encoded).unwrap_err();
        assert_eq!(error.code, "ACTIVE_JOBS");
        assert_eq!(error.exit_code, 6);
        assert!(error.message.contains('2'));
    }

    #[test]
    fn supervisor_error_wire_preserves_required_acknowledgements() {
        let mut encoded = Vec::new();
        wire::write_uint32(&mut encoded, 1, 2);
        wire::write_string(&mut encoded, 2, "ACKNOWLEDGEMENT_REQUIRED");
        wire::write_string(&mut encoded, 3, "The exact token set is incomplete.");
        wire::write_uint32(&mut encoded, 5, 4);
        wire::write_bool(&mut encoded, 6, true);
        wire::write_string(&mut encoded, 7, "data-loss:userdata");
        let error = decode_reply(&encoded).unwrap_err();
        assert_eq!(error.exit_code, 4);
        assert!(error.retryable);
        assert_eq!(error.required_acknowledgements, vec!["data-loss:userdata"]);
    }

    #[test]
    fn apply_rechecks_the_exact_current_authority_support_axes() {
        let mechanics = sha256(b"mechanics key");
        let hdc = sha256(b"hdc build");
        let (support_key, support_state) =
            current_authority_support(mechanics, Some(hdc), Some("CLI-AC-28")).unwrap();
        let plan = ExecutablePlan {
            mechanics_maturity_key_sha256: mechanics.to_hex(),
            authority_support_key_sha256: support_key.to_hex(),
            authority_support_state: support_state.as_str().into(),
            authority_support_campaign: support_state.campaign().unwrap().into(),
            ..ExecutablePlan::default()
        };

        require_current_authority_support(&plan, Some(hdc), Some("CLI-AC-28")).unwrap();
        assert_eq!(
            require_current_authority_support(
                &plan,
                Some(sha256(b"different hdc")),
                Some("CLI-AC-28")
            )
            .unwrap_err()
            .code,
            "AUTHORITY_SUPPORT_SEAL_MISMATCH"
        );
        assert_eq!(
            require_current_authority_support(&plan, Some(hdc), Some("CLI-AC-29"))
                .unwrap_err()
                .code,
            "AUTHORITY_SUPPORT_SEAL_MISMATCH"
        );
        assert_eq!(
            require_current_authority_support(&plan, None, Some("CLI-AC-28"))
                .unwrap_err()
                .code,
            "AUTHORITY_SUPPORT_UNAVAILABLE"
        );
    }

    #[test]
    fn pairing_epoch_is_durable_and_strictly_rotates_before_pairing() {
        let root = std::env::temp_dir().join(format!(
            "arkforge-cli-pairing-epoch-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let (first_epoch, first_secret) = fresh_pairing(&root).unwrap();
        let (second_epoch, second_secret) = fresh_pairing(&root).unwrap();
        assert_eq!(first_epoch, 1);
        assert_eq!(second_epoch, 2);
        assert_ne!(first_secret, second_secret);
        assert_eq!(
            std::fs::read_to_string(root.join("authority/pairing-epoch")).unwrap(),
            "2"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(root.join("authority/pairing-epoch"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn permit_is_durable_before_submission_and_same_epoch_retry_is_byte_exact() {
        let root =
            std::env::temp_dir().join(format!("arkforge-cli-permit-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let plan_digest = sha256(b"plan");
        let stable = sha256(b"stable target");
        let admitted = sha256(b"admitted device");
        let snapshot = StepAdmissionSnapshot {
            job_id: "JOB-TEST".into(),
            plan_id: "PLAN-TEST".into(),
            plan_sha256: plan_digest.as_bytes().to_vec(),
            step_id: "STEP-TEST".into(),
            attempt_id: "ATTEMPT-1".into(),
            public_step_sha256: sha256(b"public").as_bytes().to_vec(),
            private_action_sha256: sha256(b"private").as_bytes().to_vec(),
            effect_set_sha256: sha256(b"effects").as_bytes().to_vec(),
            admitted_device_facts_sha256: admitted.as_bytes().to_vec(),
            request_id: "REQUEST-TEST".into(),
            ..StepAdmissionSnapshot::default()
        };
        let record = AuthorityPlanRecord {
            plan: ExecutablePlan {
                plan_id: "PLAN-TEST".into(),
                plan_sha256: plan_digest.to_hex(),
                ..ExecutablePlan::default()
            },
            binding_id: "CLI-BIND-TEST".into(),
            stable_identity_sha256: stable.as_bytes().to_vec(),
            device_id: "OBS-TEST".into(),
            profile_id: "profile@1.0.0".into(),
            supersedes_job_id: String::new(),
            topology_sha256: "topology".into(),
            toolchain_id: "toolchain-a".into(),
        };
        let pairing = ControllerPairingSecret::new(PairingEpoch(7), b"secret".to_vec());
        let first =
            load_or_create_permit(&root, 7, &pairing, &snapshot, &record, admitted, 100, 200)
                .unwrap();
        let retried =
            load_or_create_permit(&root, 7, &pairing, &snapshot, &record, admitted, 101, 199)
                .unwrap();
        assert_eq!(retried.permit_cbor, first.permit_cbor);
        assert_eq!(retried.integrity_tag, first.integrity_tag);
        let permit = StepPermit::from_canonical_bytes(&first.permit_cbor).unwrap();
        let expected = mint_integrity_tag(&permit, &pairing).unwrap();
        assert_eq!(first.integrity_tag, expected.tag.as_bytes());
        let rotated_pairing =
            ControllerPairingSecret::new(PairingEpoch(8), b"rotated secret".to_vec());
        let rotated = load_or_create_permit(
            &root,
            8,
            &rotated_pairing,
            &snapshot,
            &record,
            admitted,
            101,
            201,
        )
        .unwrap();
        assert_ne!(rotated.permit_cbor, first.permit_cbor);
        assert_ne!(rotated.integrity_tag, first.integrity_tag);
        let rotated_permit = StepPermit::from_canonical_bytes(&rotated.permit_cbor).unwrap();
        assert_eq!(
            rotated_permit.controller_session_id.as_str(),
            "CLI-SESSION-8"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn recovery_acknowledgement_is_part_of_the_exact_apply_set() {
        let record = AuthorityPlanRecord {
            plan: ExecutablePlan {
                persistent_effects: vec![arkforge_ipc::messages::Effect {
                    target: "userdata".into(),
                    ..arkforge_ipc::messages::Effect::default()
                }],
                ..ExecutablePlan::default()
            },
            binding_id: "CLI-BIND-TEST".into(),
            stable_identity_sha256: sha256(b"stable").as_bytes().to_vec(),
            device_id: "OBS-TEST".into(),
            profile_id: "profile@1.0.0".into(),
            supersedes_job_id: "JOB-OLD".into(),
            topology_sha256: "topology".into(),
            toolchain_id: "toolchain-a".into(),
        };
        assert_eq!(
            record_acknowledgements(&record),
            vec![
                "data-loss:userdata".to_string(),
                "recovery:supersedes-job=JOB-OLD".to_string()
            ]
        );
    }
}
