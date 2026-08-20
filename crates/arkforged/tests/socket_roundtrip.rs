//! Live socket test: start the real daemon, talk to both sockets.
//!
//! The API-surface tests assert what the service decides; this asserts that the
//! process, the sockets, the handshake and the framing actually work together —
//! including that the public socket refuses a controller handshake and that
//! `startExecution` is unavailable over the wire, not just in the handler.

use arkforge_artifact::fixture;
use arkforge_core::digest::sha256;
use arkforge_ipc::framing::{read_frame, write_frame};
use arkforge_ipc::messages::{
    ErrorBody, Hello, HelloAck, InspectArtifactResponse, MaterializePlanResponse, Request, Response,
};
use arkforge_ipc::{Api, PROTOCOL_MAJOR, PROTOCOL_MINOR, SessionKind, Status, wire};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Daemon {
    child: Child,
    runtime_dir: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.runtime_dir);
    }
}

impl Daemon {
    fn start(name: &str) -> Option<Self> {
        let runtime_dir =
            std::env::temp_dir().join(format!("arkforged-live-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&runtime_dir);
        std::fs::create_dir_all(&runtime_dir).ok()?;

        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()?
            .parent()?
            .to_path_buf();

        let child = Command::new(env!("CARGO_BIN_EXE_arkforged"))
            .arg("--runtime-dir")
            .arg(&runtime_dir)
            .arg("--profile")
            .arg(repo_root.join("profiles/dayu200.yaml"))
            .arg("--transcript")
            .arg(repo_root.join("transcripts/dayu200-gj4-ecamp-96effff15.yaml"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;

        let daemon = Daemon { child, runtime_dir };
        // Wait for both sockets rather than sleeping a fixed interval.
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if daemon.public_socket().exists() && daemon.controller_socket().exists() {
                // The file existing is not the same as the listener accepting.
                if UnixStream::connect(daemon.public_socket()).is_ok() {
                    return Some(daemon);
                }
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        None
    }

    fn public_socket(&self) -> PathBuf {
        self.runtime_dir.join("public.sock")
    }

    fn controller_socket(&self) -> PathBuf {
        self.runtime_dir.join("controller.sock")
    }

    fn connect(&self, kind: SessionKind) -> Result<UnixStream, String> {
        self.connect_with_ack(kind).map(|(stream, _)| stream)
    }

    /// The handshake plus what the daemon said about itself.
    fn connect_with_ack(&self, kind: SessionKind) -> Result<(UnixStream, HelloAck), String> {
        let path = match kind {
            SessionKind::Public => self.public_socket(),
            SessionKind::Controller => self.controller_socket(),
        };
        let mut stream = UnixStream::connect(path).map_err(|error| error.to_string())?;
        let hello = Hello {
            protocol_major: PROTOCOL_MAJOR,
            protocol_minor: PROTOCOL_MINOR,
            session_kind: kind,
        };
        write_frame(&mut stream, &hello.encode()).map_err(|error| error.to_string())?;
        let frame = read_frame(&mut stream)
            .map_err(|error| error.to_string())?
            .ok_or("closed during handshake")?;
        let ack = HelloAck::decode(&frame).map_err(|error| error.to_string())?;
        match &ack.refusal {
            Some(refusal) => Err(refusal.clone()),
            None => Ok((stream, ack)),
        }
    }
}

fn call(stream: &mut UnixStream, api: Api, payload: Vec<u8>) -> Response {
    let request = Request {
        request_id: "REQ-live".into(),
        api,
        payload,
    };
    write_frame(stream, &request.encode()).unwrap();
    let frame = read_frame(stream).unwrap().unwrap();
    Response::decode(&frame).unwrap()
}

#[test]
fn the_daemon_serves_the_read_only_vertical_over_unix_sockets() {
    let Some(daemon) = Daemon::start("vertical") else {
        panic!("the daemon did not come up");
    };

    // Import over the controller socket, streaming the archive in frames.
    let mut controller = daemon.connect(SessionKind::Controller).unwrap();
    let archive = fixture::dayu200_archive();
    let mut payload = Vec::new();
    wire::write_uint64(&mut payload, 1, archive.len() as u64);
    let request = Request {
        request_id: "REQ-import".into(),
        api: Api::ImportArtifact,
        payload,
    };
    write_frame(&mut controller, &request.encode()).unwrap();
    for chunk in archive.chunks(64 * 1024) {
        write_frame(&mut controller, chunk).unwrap();
    }
    write_frame(&mut controller, &[]).unwrap();
    controller.flush().unwrap();

    let frame = read_frame(&mut controller).unwrap().unwrap();
    let response = Response::decode(&frame).unwrap();
    assert_eq!(
        response.status,
        Status::Ok,
        "{:?}",
        ErrorBody::decode(&response.payload)
    );
    let mut artifact_id = String::new();
    let mut reader = wire::Reader::new(&response.payload);
    while let Some((field, value)) = reader.next_field().unwrap() {
        if field == 1 {
            artifact_id = value.as_str(1).unwrap().to_string();
        }
    }
    assert_eq!(artifact_id.len(), 64);

    // Inspect over the public socket.
    let mut public = daemon.connect(SessionKind::Public).unwrap();
    let mut payload = Vec::new();
    wire::write_string(&mut payload, 1, &artifact_id);
    let response = call(&mut public, Api::InspectArtifact, payload);
    assert_eq!(response.status, Status::Ok);
    let manifest = InspectArtifactResponse::decode(&response.payload).unwrap();
    assert_eq!(manifest.members.len(), 17);
    assert_eq!(manifest.partitions.len(), 15);

    // Discover, then assess.
    let response = call(&mut public, Api::DiscoverDevices, Vec::new());
    assert_eq!(response.status, Status::Ok);

    let mut payload = Vec::new();
    wire::write_string(&mut payload, 1, &artifact_id);
    wire::write_string(&mut payload, 2, "org.openharmony.dayu200");
    wire::write_string(&mut payload, 3, "OBS-PREFLIGHT");
    wire::write_string(&mut payload, 4, "fullRestore");
    wire::write_string(&mut payload, 5, "arkforged-native-rockusb");
    wire::write_string(&mut payload, 6, "test-authority");
    wire::write_string(&mut payload, 7, "BINDING-SOCKET");
    wire::write_uint64(&mut payload, 8, 1);
    wire::write_bytes(&mut payload, 9, sha256(b"stable-device-socket").as_bytes());
    wire::write_string(&mut payload, 10, "primaryFlash");
    let response = call(&mut public, Api::MaterializePlan, payload);
    assert_eq!(response.status, Status::Ok);
    match MaterializePlanResponse::decode(&response.payload).unwrap() {
        MaterializePlanResponse::Assessment(assessment) => {
            assert_eq!(assessment.availability, "unavailable");
            assert_eq!(assessment.known_persistent_effects.len(), 9);
        }
        other => panic!("expected an assessment, got {other:?}"),
    }

    // startExecution is unavailable on the wire, from the controller socket.
    // The default native dispatcher is bound, but no authority is paired, so
    // the request is still refused before any plan or device action.
    let response = call(&mut controller, Api::StartExecution, Vec::new());
    assert_eq!(response.status, Status::Unavailable);
    assert_eq!(
        ErrorBody::decode(&response.payload).unwrap().code,
        "NO_PAIRED_AUTHORITY"
    );
}

/// Readiness reaches a client on the handshake, before it materializes a plan
/// it could not run.
#[test]
fn the_handshake_reports_execution_readiness() {
    // Keep this label short: macOS caps Unix-domain socket paths, and its
    // per-user temporary directory already consumes most of that budget.
    let Some(daemon) = Daemon::start("ready") else {
        panic!("the daemon did not come up");
    };
    let (_stream, ack) = daemon.connect_with_ack(SessionKind::Controller).unwrap();

    assert!(!ack.execution_ready);
    assert_eq!(
        ack.execution_blockers,
        vec!["NO_PAIRED_AUTHORITY".to_string()],
        "the default native dispatcher is bound before the handshake"
    );
    assert_eq!(ack.toolchain_id, "arkforged-native-rockusb");
    assert_eq!(ack.toolchain_sha256.len(), 64);
}

#[test]
fn the_public_socket_refuses_a_controller_handshake() {
    let Some(daemon) = Daemon::start("handshake") else {
        panic!("the daemon did not come up");
    };
    let mut stream = UnixStream::connect(daemon.public_socket()).unwrap();
    let hello = Hello {
        protocol_major: PROTOCOL_MAJOR,
        protocol_minor: PROTOCOL_MINOR,
        session_kind: SessionKind::Controller,
    };
    write_frame(&mut stream, &hello.encode()).unwrap();
    let frame = read_frame(&mut stream).unwrap().unwrap();
    let ack = HelloAck::decode(&frame).unwrap();
    assert!(
        ack.refusal.is_some(),
        "a controller handshake on the public socket must be refused"
    );
}

#[test]
fn an_incompatible_protocol_major_is_refused() {
    let Some(daemon) = Daemon::start("version") else {
        panic!("the daemon did not come up");
    };
    let mut stream = UnixStream::connect(daemon.public_socket()).unwrap();
    let hello = Hello {
        protocol_major: PROTOCOL_MAJOR + 1,
        protocol_minor: 0,
        session_kind: SessionKind::Public,
    };
    write_frame(&mut stream, &hello.encode()).unwrap();
    let frame = read_frame(&mut stream).unwrap().unwrap();
    let ack = HelloAck::decode(&frame).unwrap();
    assert!(ack.refusal.unwrap().contains("major"));
}

#[cfg(unix)]
#[test]
fn the_sockets_are_not_world_accessible() {
    use std::os::unix::fs::PermissionsExt;
    let Some(daemon) = Daemon::start("perms") else {
        panic!("the daemon did not come up");
    };
    for socket in [daemon.public_socket(), daemon.controller_socket()] {
        let mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode & 0o077,
            0,
            "{} mode {mode:o} exposes group/other",
            socket.display()
        );
    }
}
