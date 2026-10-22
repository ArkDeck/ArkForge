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
    LocalChannel, LocalEndpoint, LocalListener, LocalStream, detach_standard_handles, fill_random,
    protect_path, replace_file, sync_directory, unix_socket_path,
};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const READY_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a supervisor request waits for its answer.
///
/// `status` and `stop` are answered at once by a supervisor that is serving.
/// The bound is for the one that is not: `run` publishes a connectable
/// endpoint at `bind` and only reaches `serve` after pairing and the daemon
/// handshake, and on Windows that pipe connects perfectly well in the interim.
/// Without a bound the caller waits there forever, and `READY_TIMEOUT` cannot
/// help because it is only consulted between calls, never inside one.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

fn request_read_timeout(verb: &str) -> Duration {
    match verb {
        "verify-flash" => Duration::from_secs(620),
        // Parsing and hashing a firmware archive is not a lifecycle handshake.
        // Use the controller client's existing materialization idle-read
        // budget for the CLI-to-supervisor hop too.
        "assess-plan" | "materialize-plan" | "materialize-recovery-plan" => {
            ControllerClient::MATERIALIZATION_READ_TIMEOUT
        }
        _ => REQUEST_TIMEOUT,
    }
}

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
