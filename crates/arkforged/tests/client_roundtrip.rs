//! The typed Rust clients against the real daemon: the controller session a
//! supervisor opens imports, inspects and discovers through `arkforge-client`
//! as the Swift SDK's controller client does, and both sessions acknowledge
//! the same standing readiness.
//!
//! The daemon runs on the DAYU200 transcript, so no USB device is enumerated
//! or touched.

#![cfg(unix)]

use arkforge_artifact::fixture;
use arkforge_client::{ControllerClient, PublicClient};
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
    /// A paired daemon on the transcript, both sockets accepting.
    fn start(name: &str) -> Self {
        #[cfg(target_os = "macos")]
        let base = PathBuf::from("/private/tmp");
        #[cfg(not(target_os = "macos"))]
        let base = std::env::temp_dir();
        let runtime_dir = base.join(format!("arkforged-client-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&runtime_dir);
        std::fs::create_dir_all(&runtime_dir).unwrap();
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let mut child = Command::new(env!("CARGO_BIN_EXE_arkforged"))
            .arg("--runtime-dir")
            .arg(&runtime_dir)
            .arg("--profile")
            .arg(repo_root.join("profiles/dayu200.yaml"))
            .arg("--transcript")
            .arg(repo_root.join("transcripts/dayu200-gj4-ecamp-96effff15.yaml"))
            .arg("--pair-from-stdin")
            .arg("1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.as_mut().unwrap();
        stdin.write_all(&[0xA5; 32]).unwrap();
        stdin.flush().unwrap();
        let daemon = Self { child, runtime_dir };
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if daemon.runtime_dir.join("controller.sock").exists()
                && UnixStream::connect(daemon.runtime_dir.join("public.sock")).is_ok()
            {
                return daemon;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("the daemon did not come up");
    }
}

#[test]
fn both_sessions_acknowledge_the_same_standing_readiness() {
    let daemon = Daemon::start("readiness");
    let controller = ControllerClient::connect(&daemon.runtime_dir).unwrap();
    let public = PublicClient::connect(&daemon.runtime_dir).unwrap();
    assert_eq!(controller.runtime_info(), public.runtime_info());
    assert_eq!(controller.runtime_info().protocol_major, 1);
}

#[test]
fn the_controller_session_imports_inspects_and_discovers() {
    let daemon = Daemon::start("controller");
    let connect = || {
        ControllerClient::connect_with_read_timeout(
            &daemon.runtime_dir,
            ControllerClient::MATERIALIZATION_READ_TIMEOUT,
        )
        .unwrap()
    };
    let mut controller = connect();
    let archive = fixture::dayu200_archive();
    let digest = arkforge_core::digest::sha256(&archive).to_hex();
    let path = daemon.runtime_dir.join("dayu200-fixture.zip");
    std::fs::write(&path, &archive).unwrap();
    let imported = controller.import_artifact(&path, &digest).unwrap();
    assert_eq!(imported.artifact_id, digest);
    assert_eq!(imported.sha256, digest);
    assert_eq!(imported.size_bytes, archive.len() as u64);
    assert!(!imported.deduplicated);
    // The store is addressed by content: the same bytes again are the same
    // artifact.
    let again = controller.import_artifact(&path, &digest).unwrap();
    assert_eq!(again.artifact_id, digest);
    assert!(again.deduplicated);

    let manifest = controller.artifact_show(&digest).unwrap();
    let mut public = PublicClient::connect(&daemon.runtime_dir).unwrap();
    assert_eq!(public.artifact_show(&digest).unwrap(), manifest);

    let observed = controller.device_list().unwrap();
    assert_eq!(observed, public.device_list().unwrap());
    assert!(!observed.is_empty(), "the transcript observes its board");

    // A digest the content does not have is refused, and nothing is stored.
    // A refused import closes its connection, so it runs last.
    let error = controller
        .import_artifact(&path, &"0".repeat(64))
        .unwrap_err();
    assert_eq!(error.code, "IMPORT_REFUSED");
}
