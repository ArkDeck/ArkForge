//! Persistent local CLI authority supervisor.
//!
//! The supervisor, not a short-lived command, owns the pairing secret. It
//! passes the secret to `arkforged` over an anonymous stdin pipe and exposes an
//! owner-only local control socket containing no secret-bearing operation.

use crate::CliError;
use arkforged::public_client::{PublicClient, PublicRuntimeInfo};
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const SOCKET: &str = "supervisor.sock";
const READY_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Default)]
pub struct DaemonOptions {
    profile_files: Vec<PathBuf>,
    hdc: Option<PathBuf>,
    expect_hdc_sha256: Option<String>,
    require_release_signing: bool,
}

impl DaemonOptions {
    pub fn parse(arguments: &[String]) -> Result<Self, CliError> {
        let mut options = Self::default();
        let mut index = 0;
        while index < arguments.len() {
            match arguments[index].as_str() {
                "--profile-file" => {
                    index += 1;
                    options
                        .profile_files
                        .push(PathBuf::from(arguments.get(index).ok_or_else(|| {
                            CliError::invalid("--profile-file requires a file path.")
                        })?));
                }
                "--hdc" => {
                    index += 1;
                    let path =
                        PathBuf::from(arguments.get(index).ok_or_else(|| {
                            CliError::invalid("--hdc requires an absolute path.")
                        })?);
                    if !path.is_absolute() {
                        return Err(CliError::invalid("--hdc requires an absolute path."));
                    }
                    if options.hdc.replace(path).is_some() {
                        return Err(CliError::invalid("--hdc may be supplied only once."));
                    }
                }
                "--expect-hdc-sha256" => {
                    index += 1;
                    let digest = arguments.get(index).ok_or_else(|| {
                        CliError::invalid("--expect-hdc-sha256 requires 64 lowercase hex digits.")
                    })?;
                    if digest.len() != 64
                        || !digest
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    {
                        return Err(CliError::invalid(
                            "--expect-hdc-sha256 requires 64 lowercase hex digits.",
                        ));
                    }
                    if options.expect_hdc_sha256.replace(digest.clone()).is_some() {
                        return Err(CliError::invalid(
                            "--expect-hdc-sha256 may be supplied only once.",
                        ));
                    }
                }
                "--require-release-signing" => {
                    if options.require_release_signing {
                        return Err(CliError::invalid(
                            "--require-release-signing may be supplied only once.",
                        ));
                    }
                    options.require_release_signing = true;
                }
                argument => {
                    return Err(CliError::invalid(format!(
                        "Unknown daemon option {argument:?}."
                    )));
                }
            }
            index += 1;
        }
        if options.hdc.is_some() != options.expect_hdc_sha256.is_some() {
            return Err(CliError::invalid(
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
    pub active_jobs: usize,
    pub blockers: Vec<String>,
}

pub fn start(runtime_dir: PathBuf, options: DaemonOptions) -> Result<DaemonStatus, CliError> {
    if status(&runtime_dir).is_ok() {
        return Err(CliError::new(
            "RUNTIME_ALREADY_RUNNING",
            "This ArkForge runtime already has a live CLI authority supervisor.",
            6,
            false,
        ));
    }
    let executable =
        std::env::current_exe().map_err(|error| internal("identify arkforge", error))?;
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
) -> Result<(), CliError> {
    prepare_runtime(&runtime_dir)?;
    let socket = runtime_dir.join(SOCKET);
    if UnixStream::connect(&socket).is_ok() {
        return Err(CliError::new(
            "RUNTIME_ALREADY_RUNNING",
            "This runtime is already owned by a live CLI authority supervisor.",
            6,
            false,
        ));
    }
    if socket.exists() {
        std::fs::remove_file(&socket)
            .map_err(|error| internal("remove a stale supervisor socket", error))?;
    }
    let listener = UnixListener::bind(&socket)
        .map_err(|error| internal("bind the authority supervisor socket", error))?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| internal("protect the authority supervisor socket", error))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| internal("configure the authority supervisor socket", error))?;

    let (epoch, secret) = fresh_pairing()?;
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
    let _secret = secret;
    let result = serve(listener, &runtime_dir, &mut daemon, epoch);
    let _ = daemon.kill();
    let _ = daemon.wait();
    for name in [SOCKET, "public.sock", "controller.sock"] {
        let _ = std::fs::remove_file(runtime_dir.join(name));
    }
    result
}

pub fn status(runtime_dir: &Path) -> Result<DaemonStatus, CliError> {
    request(runtime_dir, "status")
}

pub fn stop(runtime_dir: &Path) -> Result<DaemonStatus, CliError> {
    request(runtime_dir, "stop")
}

fn request(runtime_dir: &Path, verb: &str) -> Result<DaemonStatus, CliError> {
    let socket = runtime_dir.join(SOCKET);
    let mut stream = UnixStream::connect(&socket).map_err(|error| {
        CliError::new(
            "DAEMON_UNAVAILABLE",
            format!(
                "No CLI authority supervisor is listening at {}: {error}",
                socket.display()
            ),
            5,
            true,
        )
    })?;
    stream
        .write_all(format!("{verb}\n").as_bytes())
        .map_err(|error| internal("write supervisor request", error))?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|error| internal("finish supervisor request", error))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|error| internal("read supervisor response", error))?;
    parse_status(response.trim_end_matches(['\r', '\n']))
}

fn serve(
    listener: UnixListener,
    runtime_dir: &Path,
    daemon: &mut Child,
    epoch: u64,
) -> Result<(), CliError> {
    loop {
        if let Some(exit) = daemon
            .try_wait()
            .map_err(|error| internal("observe arkforged", error))?
        {
            return Err(CliError::new(
                "MECHANICS_DAEMON_EXITED",
                format!("arkforged exited unexpectedly with {exit}."),
                10,
                true,
            ));
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let mut request = String::new();
                stream
                    .read_to_string(&mut request)
                    .map_err(|error| internal("read supervisor request", error))?;
                let mut public = PublicClient::connect(runtime_dir)?;
                let runtime_info = public.runtime_info().clone();
                let active_jobs = public
                    .job_list()?
                    .into_iter()
                    .filter(|job| !job.terminal)
                    .count();
                let status = DaemonStatus {
                    supervisor_pid: std::process::id(),
                    daemon_pid: daemon.id(),
                    epoch,
                    protocol_major: runtime_info.protocol_major,
                    protocol_minor: runtime_info.protocol_minor,
                    daemon_version: runtime_info.daemon_version.clone(),
                    mechanics_ready: runtime_info.execution_ready,
                    active_jobs,
                    blockers: runtime_info.execution_blockers.clone(),
                };
                match request.trim() {
                    "status" => write_status(&mut stream, "STATUS", &status)?,
                    "stop" if active_jobs == 0 => {
                        write_status(&mut stream, "STOPPED", &status)?;
                        return Ok(());
                    }
                    "stop" => {
                        writeln!(stream, "REFUSED\tACTIVE_JOBS\t{active_jobs}")
                            .map_err(|error| internal("write stop refusal", error))?;
                    }
                    _ => {
                        writeln!(stream, "REFUSED\tBAD_REQUEST\t0")
                            .map_err(|error| internal("write request refusal", error))?;
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

fn write_status(
    stream: &mut UnixStream,
    kind: &str,
    status: &DaemonStatus,
) -> Result<(), CliError> {
    writeln!(
        stream,
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        kind,
        status.supervisor_pid,
        status.daemon_pid,
        status.epoch,
        status.protocol_major,
        status.protocol_minor,
        status.daemon_version,
        status.mechanics_ready,
        status.active_jobs,
        status.blockers.join(",")
    )
    .map_err(|error| internal("write supervisor status", error))
}

fn parse_status(response: &str) -> Result<DaemonStatus, CliError> {
    let fields: Vec<&str> = response.split('\t').collect();
    if fields.first() == Some(&"REFUSED") {
        return match fields.get(1).copied() {
            Some("ACTIVE_JOBS") => Err(CliError::new(
                "ACTIVE_JOBS",
                format!(
                    "The runtime has {} active job(s); request cancellation and wait for a terminal state before stopping.",
                    fields.get(2).copied().unwrap_or("one or more")
                ),
                6,
                true,
            )),
            _ => Err(CliError::new(
                "SUPERVISOR_REFUSED",
                "The authority supervisor refused the request.",
                10,
                false,
            )),
        };
    }
    if fields.len() != 10 || !matches!(fields[0], "STATUS" | "STOPPED") {
        return Err(CliError::new(
            "SUPERVISOR_RESPONSE_INVALID",
            "The authority supervisor returned an invalid status response.",
            10,
            false,
        ));
    }
    Ok(DaemonStatus {
        supervisor_pid: parse_field(fields[1], "supervisor pid")?,
        daemon_pid: parse_field(fields[2], "daemon pid")?,
        epoch: parse_field(fields[3], "pairing epoch")?,
        protocol_major: parse_field(fields[4], "protocol major")?,
        protocol_minor: parse_field(fields[5], "protocol minor")?,
        daemon_version: fields[6].to_string(),
        mechanics_ready: parse_field(fields[7], "mechanics readiness")?,
        active_jobs: parse_field(fields[8], "active jobs")?,
        blockers: fields[9]
            .split(',')
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect(),
    })
}

fn parse_field<T: std::str::FromStr>(value: &str, name: &str) -> Result<T, CliError> {
    value.parse().map_err(|_| {
        CliError::new(
            "SUPERVISOR_RESPONSE_INVALID",
            format!("The supervisor returned an invalid {name}."),
            10,
            false,
        )
    })
}

fn prepare_runtime(runtime_dir: &Path) -> Result<(), CliError> {
    std::fs::create_dir_all(runtime_dir)
        .map_err(|error| internal("create the runtime directory", error))?;
    std::fs::set_permissions(runtime_dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| internal("protect the runtime directory", error))
}

fn fresh_pairing() -> Result<(u64, Vec<u8>), CliError> {
    let mut bytes = [0u8; 40];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|error| internal("read host randomness for pairing", error))?;
    let mut epoch_bytes = [0u8; 8];
    epoch_bytes.copy_from_slice(&bytes[..8]);
    let epoch = u64::from_le_bytes(epoch_bytes).max(1);
    Ok((epoch, bytes[8..].to_vec()))
}

fn spawn_daemon(
    runtime_dir: &Path,
    options: &DaemonOptions,
    epoch: u64,
    secret: &[u8],
    foreground_output: bool,
) -> Result<Child, CliError> {
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
    if foreground_output {
        command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    } else {
        command.stdout(Stdio::null()).stderr(Stdio::null());
    }
    let mut child = command.spawn().map_err(|error| {
        CliError::new(
            "MECHANICS_DAEMON_UNAVAILABLE",
            format!("Cannot start {}: {error}", daemon.display()),
            5,
            true,
        )
    })?;
    let mut stdin = child.stdin.take().ok_or_else(|| {
        CliError::new(
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
    drop(stdin);
    Ok(child)
}

fn validate_tool_bindings(options: &DaemonOptions) -> Result<(), CliError> {
    if let (Some(hdc), Some(expected)) = (&options.hdc, &options.expect_hdc_sha256) {
        let actual = arkforged::dispatch::executable_digest(hdc).map_err(|error| {
            CliError::new(
                "HDC_BINDING_REFUSED",
                format!("Cannot bind the exact HDC executable: {error}"),
                3,
                false,
            )
        })?;
        if actual.to_hex() != *expected {
            return Err(CliError::new(
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
        let signed = arkforged::packaging::read_file(&daemon).map_err(|error| {
            CliError::new(
                "RELEASE_SIGNING_REQUIRED",
                format!("Cannot inspect the mechanics daemon signing contract: {error}"),
                3,
                false,
            )
        })?;
        let violations = signed.violations(arkforged::packaging::ContractMode::Release);
        if !violations.is_empty() {
            return Err(CliError::new(
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
    Ok(())
}

fn sibling_daemon() -> Result<PathBuf, CliError> {
    let executable =
        std::env::current_exe().map_err(|error| internal("identify arkforge", error))?;
    let path = executable
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("arkforged");
    if !path.is_file() {
        return Err(CliError::new(
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

fn wait_for_daemon(runtime_dir: &Path, daemon: &mut Child) -> Result<PublicRuntimeInfo, CliError> {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        if let Some(exit) = daemon
            .try_wait()
            .map_err(|error| internal("observe arkforged startup", error))?
        {
            return Err(CliError::new(
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
            return Err(CliError::new(
                "MECHANICS_DAEMON_START_TIMEOUT",
                "arkforged did not accept a versioned public session within 10 seconds.",
                5,
                true,
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn internal(context: &str, error: impl std::fmt::Display) -> CliError {
    CliError::new(
        "SUPERVISOR_IO_FAILED",
        format!("Cannot {context}: {error}"),
        10,
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_options_require_exact_hdc_path_and_digest_together() {
        assert!(DaemonOptions::parse(&["--hdc".into(), "/bin/hdc".into()]).is_err());
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
                "/bin/hdc".into(),
                "--expect-hdc-sha256".into(),
                "0".repeat(64),
            ])
            .is_ok()
        );
    }

    #[test]
    fn status_protocol_keeps_an_empty_blocker_field() {
        let status = parse_status("STATUS\t12\t13\t14\t1\t0\t0.1.0\ttrue\t0\t").unwrap();
        assert_eq!(status.supervisor_pid, 12);
        assert_eq!(status.daemon_pid, 13);
        assert_eq!(status.epoch, 14);
        assert!(status.mechanics_ready);
        assert!(status.blockers.is_empty());
    }

    #[test]
    fn stop_refusal_preserves_active_job_count() {
        let error = parse_status("REFUSED\tACTIVE_JOBS\t2").unwrap_err();
        assert_eq!(error.code, "ACTIVE_JOBS");
        assert_eq!(error.exit_code, 6);
        assert!(error.message.contains('2'));
    }
}
