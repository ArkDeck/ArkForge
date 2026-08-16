//! `arkforged` — the ArkForge mechanics daemon.
//!
//! architecture.md 15.1/15.2. AF-V1 serves the read-only API over a Unix domain
//! socket. `startExecution` answers `UNAVAILABLE` on both sockets.
//!
//! Two sockets, two capabilities:
//!
//! - `public.sock` (0600): inspect, discover, probe, assessment;
//! - `controller.sock` (0600): the above plus artifact import.
//!
//! Windows named pipes are a design reservation, out of AF-V1/AF-V2 acceptance
//! (architecture.md 15.2).

use arkforged::Service;
use arkforge_ipc::framing::{read_frame, write_frame};
use arkforge_ipc::messages::{Hello, HelloAck, Request, Response};
use arkforge_ipc::{negotiate, Api, SessionKind, Status, PROTOCOL_MAJOR, PROTOCOL_MINOR};
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const DAEMON_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How long the tool gets to answer a device-free probe.
///
/// The measured answer is 75 ms on this host. Five seconds is not a guess at
/// how slow the tool might be — it is far past any plausible answer, because
/// the failure this catches does not take longer, it takes forever (AD-015).
const TOOL_SELF_TEST_SECONDS: u64 = 5;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match run(&arguments) {
        Ok(()) => {}
        Err(message) => {
            eprintln!("arkforged: {message}");
            std::process::exit(1);
        }
    }
}

fn usage() -> String {
    concat!(
        "usage: arkforged --runtime-dir <dir> [--profile <file>]... [--transcript <file>]...\n",
        "                 [--pair-from-stdin <epoch>] [--rkdeveloptool <path>]\n",
        "\n",
        "  --runtime-dir      where the content store and sockets live\n",
        "  --profile          a DeviceProfile YAML document (repeatable)\n",
        "  --transcript       a golden transcript to serve as a replay transport (repeatable)\n",
        "  --pair-from-stdin  read the authority's pairing secret from stdin and close it\n",
        "  --rkdeveloptool    absolute path to the pinned vendor tool. Without it,\n",
        "                     startExecution refuses: a job that reached its first\n",
        "                     dispatch would have spent a permit before finding out\n",
        "  --rkdeveloptool-sha256  the digest those bytes must have. Required with\n",
        "                     --rkdeveloptool: an unpinned tool is a tool nobody chose\n",
        "  --require-release-signing  hold the bound tool to the shipped signing shape\n",
        "                     (Developer ID, Hardened Runtime, a Team ID) as well as the\n",
        "                     empty entitlement dictionary, which is required either way\n",
        "\n",
        "A bound tool must pass its digest check and then prove it runs, because\n",
        "byte equality is not usability: quarantined bytes with the right digest\n",
        "hang in dyld. If it cannot run, omit --rkdeveloptool for a read-only daemon.\n",
        "\n",
        "Without --pair-from-stdin no authority is paired, and startExecution is\n",
        "unavailable. The secret is read from stdin rather than an argv or an\n",
        "environment variable, neither of which this process could erase after\n",
        "reading and both of which other processes can sometimes see.\n"
    )
    .to_string()
}

fn run(arguments: &[String]) -> Result<(), String> {
    let mut runtime_dir: Option<PathBuf> = None;
    let mut profile_paths: Vec<PathBuf> = Vec::new();
    let mut transcript_paths: Vec<PathBuf> = Vec::new();
    let mut pairing_epoch: Option<u64> = None;
    let mut rkdeveloptool: Option<PathBuf> = None;
    let mut rkdeveloptool_sha256: Option<String> = None;
    let mut require_release_signing = false;

    let mut index = 0usize;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--runtime-dir" => {
                index += 1;
                runtime_dir = Some(PathBuf::from(
                    arguments.get(index).ok_or_else(|| usage())?,
                ));
            }
            "--profile" => {
                index += 1;
                profile_paths.push(PathBuf::from(arguments.get(index).ok_or_else(|| usage())?));
            }
            "--transcript" => {
                index += 1;
                transcript_paths.push(PathBuf::from(arguments.get(index).ok_or_else(|| usage())?));
            }
            "--rkdeveloptool" => {
                index += 1;
                rkdeveloptool = Some(PathBuf::from(arguments.get(index).ok_or_else(usage)?));
            }
            "--rkdeveloptool-sha256" => {
                index += 1;
                rkdeveloptool_sha256 = Some(arguments.get(index).ok_or_else(usage)?.clone());
            }
            "--require-release-signing" => require_release_signing = true,
            "--pair-from-stdin" => {
                index += 1;
                let raw = arguments.get(index).ok_or_else(usage)?;
                pairing_epoch = Some(
                    raw.parse::<u64>()
                        .map_err(|_| format!("{raw:?} is not a pairing epoch"))?,
                );
            }
            "--help" | "-h" => {
                print!("{}", usage());
                return Ok(());
            }
            other => return Err(format!("unknown argument {other:?}\n\n{}", usage())),
        }
        index += 1;
    }

    let runtime_dir = runtime_dir.ok_or_else(usage)?;
    std::fs::create_dir_all(&runtime_dir).map_err(|error| error.to_string())?;

    let mut profiles = Vec::new();
    for path in &profile_paths {
        let source = std::fs::read_to_string(path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let profile = arkforge_core::profile::load(&source)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        profiles.push(profile);
    }
    let mut transcripts = Vec::new();
    for path in &transcript_paths {
        transcripts.push(
            std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?,
        );
    }

    let now = now_epoch_ms();
    let mut service = Service::new(&runtime_dir.join("store"), profiles, transcripts, now)
        .map_err(|error| error.to_string())?;
    if let Some(epoch) = pairing_epoch {
        service.pair_authority(arkforged::jobs::read_pairing_secret_from_stdin(epoch)?);
    }
    let service = Arc::new(Mutex::new(service));

    let public = bind(&runtime_dir.join("public.sock"))?;
    let controller = bind(&runtime_dir.join("controller.sock"))?;
    println!(
        "arkforged {DAEMON_VERSION} listening: public={} controller={}",
        runtime_dir.join("public.sock").display(),
        runtime_dir.join("controller.sock").display()
    );
    // The dispatcher runs on its own thread and takes the service lock only
    // for the hand-off at either end. A partition write takes minutes; holding
    // the lock across one would stop the event stream reporting on it.
    let dispatch_handle = match rkdeveloptool {
        Some(path) => {
            // The pin is required, not optional. A tool bound without one is a
            // tool nobody chose: the digest is part of the maturity
            // combination (architecture.md 12.3), so binding whatever happens
            // to be at that path would execute a combination nobody published.
            let pinned = rkdeveloptool_sha256.ok_or(
                "--rkdeveloptool requires --rkdeveloptool-sha256; binding a tool without \
                 pinning its bytes would execute a combination nobody published",
            )?;
            let expected = arkforge_core::Sha256Digest::parse_hex(&pinned)
                .map_err(|error| format!("--rkdeveloptool-sha256: {error}"))?;
            let port = arkforged::dispatch::HostFixedToolPort::open(&path)?;
            if port.digest() != expected {
                // Refuse to start rather than start unable to execute. An
                // operator who swapped the binary should hear it now.
                return Err(format!(
                    "{} hashes to {}, and --rkdeveloptool-sha256 pins {expected}",
                    path.display(),
                    port.digest()
                ));
            }
            // The digest settles which bytes these are; the signature settles
            // whether macOS will let them start. Both are static facts about
            // the file, so both are answered before anything depends on it —
            // and the entitlement clause is answered here rather than by the
            // self-test below, because "aborted in libsecinit" and "hung in
            // dyld" look identical from the outside and have different fixes
            // (AD-007 and AD-015 respectively).
            let signing = arkforged::packaging::read_file(&path).map_err(|error| {
                format!("{}: {error}", path.display())
            })?;
            let mode = if require_release_signing {
                arkforged::packaging::ContractMode::Release
            } else {
                arkforged::packaging::ContractMode::Development
            };
            let violations = signing.violations(mode);
            if !violations.is_empty() {
                let detail = violations
                    .iter()
                    .map(|violation| format!("  {}: {violation}", violation.code()))
                    .collect::<Vec<_>>()
                    .join("\n");
                return Err(format!(
                    "{} does not meet the macOS packaging contract ({}):\n{detail}",
                    path.display(),
                    arkforged::packaging::CONTRACT_DOC
                ));
            }
            {
                let Ok(mut guard) = service.lock() else {
                    return Err("the service lock is poisoned".into());
                };
                guard.bind_dispatcher(arkforge_engine::BoundToolchain {
                    id: arkforge_core::ids::OpaqueId::new("rkdeveloptool")
                        .map_err(|error| error.to_string())?,
                    backend_digest: port.digest(),
                });
            }
            // The digest settles which bytes these are. It does not settle
            // whether they can run: quarantined bytes with the right digest
            // hang in dyld (AD-015). `-v` is device-free, so proving it runs
            // costs nothing but a fork.
            let probe = port
                .self_test(
                    &["-v"],
                    "rkdeveloptool",
                    std::time::Duration::from_secs(TOOL_SELF_TEST_SECONDS),
                )
                .map_err(|failure| {
                    format!(
                        "{} passed its digest check and then failed to run.\n  {failure}\n\
                         Omit --rkdeveloptool to start a read-only daemon instead.",
                        path.display()
                    )
                })?;
            println!("dispatch: {} ({})", path.display(), port.digest());
            println!("  signing: {}", signing.summary());
            println!(
                "  self-test: {} in {} ms",
                probe.first_line, probe.duration_ms
            );
            let store_root = runtime_dir.join("store");
            let work_root = runtime_dir.join("work");
            let dispatch_service = Arc::clone(&service);
            Some(std::thread::spawn(move || {
                let mut dispatcher =
                    arkforged::dispatch::Dispatcher::new(store_root, work_root, &port);
                loop {
                    let work = {
                        let Ok(mut guard) = dispatch_service.lock() else {
                            return;
                        };
                        guard.take_pending_dispatch()
                    };
                    let Some(work) = work else {
                        std::thread::sleep(std::time::Duration::from_millis(200));
                        continue;
                    };
                    let outcome = dispatcher.run(&work);
                    let Ok(mut guard) = dispatch_service.lock() else {
                        return;
                    };
                    if let Err(error) = guard.complete_dispatch(&work.job_id, outcome) {
                        eprintln!("arkforged: recording {}: {error}", work.job_id);
                    }
                }
            }))
        }
        None => {
            println!(
                "dispatch: unavailable (no --rkdeveloptool; startExecution will refuse rather \
                 than let a job park at its first dispatch)"
            );
            None
        }
    };

    {
        let Ok(guard) = service.lock() else {
            return Err("the service lock is poisoned".into());
        };
        let readiness = guard.readiness();
        if readiness.is_ready() {
            println!("execution: ready");
        } else {
            let blockers: Vec<&str> = readiness
                .standing_blockers()
                .iter()
                .map(|blocker| blocker.code())
                .collect();
            println!("execution: not ready ({})", blockers.join(", "));
        }
    }

    let public_service = Arc::clone(&service);
    let handle = std::thread::spawn(move || {
        serve(public, SessionKind::Public, public_service);
    });
    serve(controller, SessionKind::Controller, service);
    let _ = handle.join();
    drop(dispatch_handle);
    Ok(())
}

fn bind(path: &Path) -> Result<UnixListener, String> {
    // A stale socket from a previous run would otherwise make bind fail.
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path).map_err(|error| format!("{}: {error}", path.display()))?;
    set_private(path)?;
    Ok(listener)
}

#[cfg(unix)]
fn set_private(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("{}: {error}", path.display()))
}

fn serve(listener: UnixListener, kind: SessionKind, service: Arc<Mutex<Service>>) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let service = Arc::clone(&service);
                std::thread::spawn(move || {
                    if let Err(error) = handle_connection(stream, kind, service) {
                        eprintln!("arkforged: {kind:?} connection ended: {error}");
                    }
                });
            }
            Err(error) => {
                eprintln!("arkforged: accept failed: {error}");
                return;
            }
        }
    }
}

fn handle_connection(
    stream: UnixStream,
    kind: SessionKind,
    service: Arc<Mutex<Service>>,
) -> Result<(), String> {
    let mut reader = stream.try_clone().map_err(|error| error.to_string())?;
    let mut writer = stream;

    // Handshake first: an unversioned peer never reaches a handler.
    let Some(frame) = read_frame(&mut reader).map_err(|error| error.to_string())? else {
        return Ok(());
    };
    let hello = Hello::decode(&frame).map_err(|error| error.to_string())?;
    let refusal = match negotiate(hello.protocol_major, hello.protocol_minor) {
        Ok(()) if hello.session_kind == kind => None,
        Ok(()) => Some(format!(
            "this socket serves {kind:?} sessions, peer announced {:?}",
            hello.session_kind
        )),
        Err(message) => Some(message),
    };
    // Readiness is reported on the handshake so a client learns it before it
    // materializes a plan it could not run, rather than after creating a job.
    let readiness = {
        let Ok(guard) = service.lock() else {
            return Err("the service lock is poisoned".into());
        };
        guard.readiness().clone()
    };
    let ack = HelloAck {
        protocol_major: PROTOCOL_MAJOR,
        protocol_minor: PROTOCOL_MINOR,
        session_kind: kind,
        daemon_version: DAEMON_VERSION.to_string(),
        refusal: refusal.clone(),
        execution_ready: readiness.is_ready(),
        execution_blockers: readiness
            .standing_blockers()
            .iter()
            .map(|blocker| blocker.code().to_string())
            .collect(),
        toolchain_id: readiness
            .dispatcher
            .as_ref()
            .map(|tool| tool.id.to_string())
            .unwrap_or_default(),
        toolchain_sha256: readiness
            .dispatcher
            .as_ref()
            .map(|tool| tool.backend_digest.to_hex())
            .unwrap_or_default(),
    };
    write_frame(&mut writer, &ack.encode()).map_err(|error| error.to_string())?;
    if refusal.is_some() {
        return Ok(());
    }

    while let Some(frame) = read_frame(&mut reader).map_err(|error| error.to_string())? {
        let request = match Request::decode(&frame) {
            Ok(request) => request,
            Err(error) => {
                // A malformed request is answered, then the connection closes:
                // a peer that cannot frame a request cannot be trusted to frame
                // the next one either.
                let response = Response {
                    request_id: String::new(),
                    api: Api::InspectArtifact,
                    status: Status::InvalidArgument,
                    payload: arkforge_ipc::messages::ErrorBody {
                        code: "MALFORMED_REQUEST".into(),
                        message: error.to_string(),
                    }
                    .encode(),
                    stream_sequence: 0,
                    stream_end: true,
                };
                write_frame(&mut writer, &response.encode()).map_err(|e| e.to_string())?;
                return Ok(());
            }
        };

        let response = if request.api == Api::ImportArtifact && kind == SessionKind::Controller {
            let mut content = ContentStream::new(&mut reader);
            let mut guard = service.lock().map_err(|_| "service lock poisoned")?;
            guard.handle(kind, &request, Some(&mut content))
        } else {
            let mut guard = service.lock().map_err(|_| "service lock poisoned")?;
            guard.handle(kind, &request, None)
        };
        write_frame(&mut writer, &response.encode()).map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Reads artifact content from the controller stream as a sequence of frames,
/// ending at a zero-length frame.
///
/// architecture.md 10.1 permits controller-only streaming for the first
/// version; what it forbids is the daemon reopening a path the caller named,
/// and this reads only from the already-authenticated connection.
struct ContentStream<'a> {
    source: &'a mut UnixStream,
    buffer: Vec<u8>,
    position: usize,
    finished: bool,
}

impl<'a> ContentStream<'a> {
    fn new(source: &'a mut UnixStream) -> Self {
        ContentStream {
            source,
            buffer: Vec::new(),
            position: 0,
            finished: false,
        }
    }
}

impl<'a> Read for ContentStream<'a> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.position == self.buffer.len() {
            if self.finished {
                return Ok(0);
            }
            match read_frame(self.source) {
                Ok(Some(frame)) if frame.is_empty() => {
                    self.finished = true;
                    return Ok(0);
                }
                Ok(Some(frame)) => {
                    self.buffer = frame;
                    self.position = 0;
                }
                Ok(None) => {
                    self.finished = true;
                    return Ok(0);
                }
                Err(error) => {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
                }
            }
        }
        let count = (self.buffer.len() - self.position).min(out.len());
        out[..count].copy_from_slice(&self.buffer[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|delta| delta.as_millis() as u64)
        .unwrap_or(0)
}

/// Keeps `Write` in the import path honest about flushing.
#[allow(dead_code)]
fn flush<W: Write>(writer: &mut W) -> io::Result<()> {
    writer.flush()
}
