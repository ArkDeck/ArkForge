
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
    // The supervisor is started to outlive this command, so anything it
    // inherits it holds for its whole life — including, on Windows, a copy of
    // our own stdout. A caller capturing our output would then wait on a pipe
    // the supervisor is still holding, having already seen us exit.
    detach_standard_handles()
        .map_err(|error| internal("detach the standard handles from the supervisor", error))?;
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
    materialize_plan_intent(runtime_dir, artifact, profile, device, "fullRestore")
}

pub fn materialize_plan_intent(
    runtime_dir: &Path,
    artifact: &str,
    profile: &str,
    device: &str,
    intent: &str,
) -> Result<MaterializePlanResponse, StandaloneError> {
    materialize_plan_from_source(runtime_dir, artifact, profile, device, intent, None)
}

/// Optionally constrain ramdisk restoration to a durable source job's board.
pub fn materialize_plan_from_source(
    runtime_dir: &Path,
    artifact: &str,
    profile: &str,
    device: &str,
    intent: &str,
    source_job: Option<&str>,
) -> Result<MaterializePlanResponse, StandaloneError> {
    let mut arguments = vec![artifact, profile, device, intent];
    arguments.extend(source_job);
    let payload = request(runtime_dir, "materialize-plan", &arguments)?;
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
    assess_plan_intent(runtime_dir, artifact, profile, device, "fullRestore")
}

pub fn assess_plan_intent(
    runtime_dir: &Path,
    artifact: &str,
    profile: &str,
    device: &str,
    intent: &str,
) -> Result<arkforge_ipc::messages::Assessment, StandaloneError> {
    assess_plan_from_source(runtime_dir, artifact, profile, device, intent, None)
}

pub fn assess_plan_from_source(
    runtime_dir: &Path,
    artifact: &str,
    profile: &str,
    device: &str,
    intent: &str,
    source_job: Option<&str>,
) -> Result<arkforge_ipc::messages::Assessment, StandaloneError> {
    let mut arguments = vec![artifact, profile, device, intent];
    arguments.extend(source_job);
    let payload = request(runtime_dir, "assess-plan", &arguments)?;
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

/// Independently verify the image ranges of a completed job in Normal mode.
/// The authority resolves all device paths, bounds and hashes from its records.
pub fn verify_flash(
    runtime_dir: &Path,
    job_id: &str,
    budget_ms: u64,
    hardware_campaign: Option<&str>,
) -> Result<FlashVerificationResult, StandaloneError> {
    verify_flash_preboot(runtime_dir, job_id, budget_ms, hardware_campaign, "")
}

pub fn verify_flash_preboot(
    runtime_dir: &Path,
    job_id: &str,
    budget_ms: u64,
    hardware_campaign: Option<&str>,
    preboot_ramdisk: &str,
) -> Result<FlashVerificationResult, StandaloneError> {
    let payload = request(
        runtime_dir,
        "verify-flash",
        &[
            job_id,
            &budget_ms.to_string(),
            hardware_campaign.unwrap_or_default(),
            preboot_ramdisk,
        ],
    )?;
    FlashVerificationResult::decode(&payload)
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
    stream
        .set_read_timeout(Some(request_read_timeout(verb)))
        .map_err(|error| internal("bound the supervisor response wait", error))?;
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
