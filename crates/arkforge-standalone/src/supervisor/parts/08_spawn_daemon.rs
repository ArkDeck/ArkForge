
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
