//! `arkforged` — the ArkForge mechanics daemon.
//!
//! architecture.md 15.1/15.2. The public Unix-domain socket is read-only; the
//! controller socket carries execution, admission and recovery assessment.
//!
//! Two sockets, two capabilities:
//!
//! - `public.sock` (0600): inspect, discover, probe, job status and guides;
//! - `controller.sock` (0600): the above plus import, execution and permits.
//!
//! Windows named pipes are a design reservation, out of AF-V1/AF-V2 acceptance
//! (architecture.md 15.2).

use arkforge_ipc::framing::{read_frame, write_frame};
use arkforge_ipc::messages::{Hello, HelloAck, Request, Response};
use arkforge_ipc::{Api, PROTOCOL_MAJOR, PROTOCOL_MINOR, SessionKind, Status, negotiate};
use arkforged::{Clock, Service};
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const DAEMON_VERSION: &str = env!("CARGO_PKG_VERSION");

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
        "                 [--pair-from-stdin <epoch>]\n",
        "\n",
        "  --runtime-dir      where the content store and sockets live\n",
        "  --profile          a DeviceProfile YAML document (repeatable)\n",
        "  --transcript       a golden transcript to serve as a replay transport (repeatable)\n",
        "  --pair-from-stdin  read the authority's pairing secret from stdin and close it\n",
        "  RockUSB dispatch is always the native implementation compiled into arkforged.\n",
        "  --hardware-campaign <id>  run as a named DAYU200 acceptance campaign\n",
        "                     Without it a DAYU200 combination is hardwareGated and only\n",
        "                     assessments materialize, because a combination becomes\n",
        "                     productionVerified only after a real flash — which needs a\n",
        "                     plan, which needs this. The campaign is sealed into every\n",
        "                     plan digest it produces, so its receipts stay campaign\n",
        "                     evidence and cannot be read back as a support claim.\n",
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
    let mut hardware_campaign: Option<String> = None;

    let mut index = 0usize;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--runtime-dir" => {
                index += 1;
                runtime_dir = Some(PathBuf::from(arguments.get(index).ok_or_else(usage)?));
            }
            "--profile" => {
                index += 1;
                profile_paths.push(PathBuf::from(arguments.get(index).ok_or_else(usage)?));
            }
            "--transcript" => {
                index += 1;
                transcript_paths.push(PathBuf::from(arguments.get(index).ok_or_else(usage)?));
            }
            "--hardware-campaign" => {
                index += 1;
                let raw = arguments.get(index).ok_or_else(usage)?;
                if raw.trim().is_empty() {
                    return Err(
                        "--hardware-campaign needs a campaign identifier; an unnamed campaign is \
                         one nobody can hold to a result"
                            .into(),
                    );
                }
                hardware_campaign = Some(raw.clone());
            }
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
    for (name, source) in [
        (
            "shipped dayu200",
            include_str!("../../../profiles/dayu200.yaml"),
        ),
        (
            "shipped dayu600",
            include_str!("../../../profiles/dayu600.yaml"),
        ),
    ] {
        profiles.push(
            arkforge_core::profile::load(source).map_err(|error| format!("{name}: {error}"))?,
        );
    }
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
            std::fs::read_to_string(path)
                .map_err(|error| format!("{}: {error}", path.display()))?,
        );
    }

    let mut service = Service::new(
        &runtime_dir.join("store"),
        profiles,
        transcripts,
        // Read per fact, not once here. A captured constant froze every
        // timestamp at launch and expired every admission it ever offered.
        Clock::System,
        hardware_campaign.as_deref(),
    )
    .map_err(|error| error.to_string())?;
    if let Some(campaign) = &hardware_campaign {
        // Said out loud at startup, next to the native backend identity.
        // A daemon that can execute writes on an unverified combination is
        // not the ordinary case, and an operator reading this log should not
        // have to infer it from the absence of a refusal later.
        println!(
            "maturity: hardware campaign {campaign} — DAYU200 plans are executable and their \
             receipts are campaign evidence, not a production support claim"
        );
    }
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
    let executable = std::env::current_exe()
        .map_err(|error| format!("cannot identify the running arkforged build: {error}"))?;
    let backend_digest = arkforged::dispatch::executable_digest(&executable)?;
    {
        let Ok(mut guard) = service.lock() else {
            return Err("the service lock is poisoned".into());
        };
        guard.bind_native_dispatcher(arkforge_engine::BoundToolchain {
            id: arkforge_core::ids::OpaqueId::new("arkforged-native-rockusb")
                .map_err(|error| error.to_string())?,
            backend_digest,
        });
    }
    println!(
        "dispatch: native RockUSB port \
         (TEST_UNIT_READY/READ_LBA/GPT/WRITE_LBA/DEVICE_RESET); \
         arkforged-build-sha256={backend_digest}"
    );
    let dispatch_handle = Some(spawn_dispatcher(
        arkforged::dispatch::NativeRockUsbPort::new(),
        runtime_dir.join("store"),
        runtime_dir.join("work"),
        Arc::clone(&service),
    ));

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

fn spawn_dispatcher<P>(
    port: P,
    store_root: PathBuf,
    work_root: PathBuf,
    dispatch_service: Arc<Mutex<Service>>,
) -> std::thread::JoinHandle<()>
where
    P: arkforge_provider::rockchip_execute::RockUsbPort + Send + 'static,
{
    std::thread::spawn(move || {
        let mut dispatcher = arkforged::dispatch::Dispatcher::new(store_root, work_root, &port);
        loop {
            let work = {
                let Ok(mut guard) = dispatch_service.lock() else {
                    return;
                };
                // The same sweep that feeds the dispatcher enforces the
                // control deadline, so a job whose authority went silent is
                // classified instead of parked forever.
                for job_id in guard.expire_stale_controls() {
                    eprintln!(
                        "arkforged: {job_id}: managed control expired unanswered; outcome \
                         classified unknown"
                    );
                }
                guard.refresh_pending_admissions();
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
    })
}

fn bind(path: &Path) -> Result<UnixListener, String> {
    // A stale socket from a previous run would otherwise make bind fail.
    let _ = std::fs::remove_file(path);
    let listener =
        UnixListener::bind(path).map_err(|error| format!("{}: {error}", path.display()))?;
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
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        error.to_string(),
                    ));
                }
            }
        }
        let count = (self.buffer.len() - self.position).min(out.len());
        out[..count].copy_from_slice(&self.buffer[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

/// Keeps `Write` in the import path honest about flushing.
#[allow(dead_code)]
fn flush<W: Write>(writer: &mut W) -> io::Result<()> {
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::usage;

    #[test]
    fn only_native_rockusb_is_exposed() {
        let text = usage();
        assert!(text.contains("always the native implementation"));
        for retired in [
            "--rockusb-port",
            "--rkdeveloptool",
            "vendor is migration-only",
        ] {
            assert!(
                !text.contains(retired),
                "retired surface returned: {retired}"
            );
        }
    }
}
