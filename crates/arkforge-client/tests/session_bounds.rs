//! A session bound (`connect_with_read_timeout` for the controller,
//! `set_timeout` for the public session) holds against a daemon that
//! acknowledges and then never answers: the call fails as a transport error
//! within the bound instead of waiting forever. A stand-in on ArkForge's own
//! local channel plays that daemon; no daemon or device is involved.

use arkforge_client::{ControllerClient, PublicClient};
use arkforge_ipc::framing::{read_frame, write_frame};
use arkforge_ipc::messages::{Hello, HelloAck};
use arkforge_ipc::{PROTOCOL_MAJOR, PROTOCOL_MINOR};
use arkforge_platform::{LocalChannel, LocalEndpoint, LocalListener};
use std::path::PathBuf;
use std::time::{Duration, Instant};

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        #[cfg(target_os = "macos")]
        let base = PathBuf::from("/private/tmp");
        #[cfg(not(target_os = "macos"))]
        let base = std::env::temp_dir();
        let root = base.join(format!("arkforge-client-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Acknowledges one session on `channel`, reads one request and stays silent.
fn silent_daemon(root: &TempRoot, channel: LocalChannel) -> std::thread::JoinHandle<()> {
    let endpoint = LocalEndpoint::for_runtime(&root.0, channel);
    let mut listener = LocalListener::bind(&endpoint).unwrap();
    std::thread::spawn(move || {
        let mut stream = listener.accept().unwrap();
        let hello = Hello::decode(&read_frame(&mut stream).unwrap().unwrap()).unwrap();
        let ack = HelloAck {
            protocol_major: PROTOCOL_MAJOR,
            protocol_minor: PROTOCOL_MINOR,
            session_kind: hello.session_kind,
            daemon_version: "0.1.0".into(),
            refusal: None,
            execution_ready: false,
            execution_blockers: vec!["NO_PAIRED_AUTHORITY".into()],
            toolchain_id: String::new(),
            toolchain_sha256: String::new(),
        };
        write_frame(&mut stream, &ack.encode()).unwrap();
        let _request = read_frame(&mut stream).unwrap();
        std::thread::sleep(Duration::from_secs(3));
    })
}

#[test]
fn a_bounded_controller_session_does_not_wait_on_a_silent_daemon() {
    let root = TempRoot::new("controller-bound");
    let daemon = silent_daemon(&root, LocalChannel::Controller);
    let mut controller =
        ControllerClient::connect_with_read_timeout(&root.0, Duration::from_millis(300)).unwrap();
    assert!(!controller.runtime_info().execution_ready);
    assert_eq!(
        controller.runtime_info().execution_blockers,
        ["NO_PAIRED_AUTHORITY"]
    );
    let started = Instant::now();
    let error = controller.device_list().unwrap_err();
    assert_eq!(error.code, "CONTROLLER_IO_FAILED");
    assert!(started.elapsed() < Duration::from_secs(2));
    daemon.join().unwrap();
}

#[test]
fn a_bounded_public_session_does_not_wait_on_a_silent_daemon() {
    let root = TempRoot::new("public-bound");
    let daemon = silent_daemon(&root, LocalChannel::Public);
    let mut public = PublicClient::connect(&root.0).unwrap();
    public
        .set_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    let started = Instant::now();
    let error = public.device_list().unwrap_err();
    assert_eq!(error.code, "IPC_IO_FAILED");
    assert!(started.elapsed() < Duration::from_secs(2));
    daemon.join().unwrap();
}
