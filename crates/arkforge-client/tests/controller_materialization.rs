//! Real local IPC with a scripted daemon. No installed daemon or device is used.
use arkforge_client::{ControllerClient, PublicClient};
use arkforge_ipc::framing::{FrameError, read_frame, write_frame};
use arkforge_ipc::messages::{
    ErrorBody, Hello, HelloAck, InspectArtifactResponse, Request, Response,
};
use arkforge_ipc::{Api, SessionKind, Status, wire};
use arkforge_platform::{LocalChannel, LocalEndpoint, LocalListener, LocalStream};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

struct Root(PathBuf);

impl Root {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        #[cfg(target_os = "macos")]
        let base = PathBuf::from("/private/tmp");
        #[cfg(not(target_os = "macos"))]
        let base = std::env::temp_dir();
        let path = base.join(format!(
            "af-client-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn listener(&self) -> LocalListener {
        LocalListener::bind(&LocalEndpoint::for_runtime(
            &self.0,
            LocalChannel::Controller,
        ))
        .unwrap()
    }

    fn serve(&self, handler: impl FnOnce(LocalStream) + Send + 'static) -> JoinHandle<()> {
        self.serve_kind(SessionKind::Controller, handler)
    }

    fn serve_kind(
        &self,
        kind: SessionKind,
        handler: impl FnOnce(LocalStream) + Send + 'static,
    ) -> JoinHandle<()> {
        let channel = if kind == SessionKind::Controller {
            LocalChannel::Controller
        } else {
            LocalChannel::Public
        };
        let mut listener =
            LocalListener::bind(&LocalEndpoint::for_runtime(&self.0, channel)).unwrap();
        thread::spawn(move || {
            let mut stream = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(30)))
                .unwrap();
            let hello = Hello::decode(&read_frame(&mut stream).unwrap().unwrap()).unwrap();
            assert_eq!(hello.session_kind, kind);
            assert_eq!(hello.protocol_major, 1);
            write_frame(
                &mut stream,
                &HelloAck {
                    protocol_major: 1,
                    protocol_minor: 0,
                    session_kind: kind,
                    daemon_version: "scripted".into(),
                    refusal: None,
                    execution_ready: false,
                    execution_blockers: Vec::new(),
                    toolchain_id: String::new(),
                    toolchain_sha256: String::new(),
                }
                .encode(),
            )
            .unwrap();
            handler(stream);
        })
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn request(stream: &mut LocalStream, api: Api) -> Request {
    let request = Request::decode(&read_frame(stream).unwrap().unwrap()).unwrap();
    assert_eq!(request.api, api);
    request
}

fn response(stream: &mut LocalStream, request: &Request, status: Status, payload: Vec<u8>) {
    write_frame(
        stream,
        &Response {
            request_id: request.request_id.clone(),
            api: request.api,
            status,
            payload,
            stream_sequence: 0,
            stream_end: true,
        }
        .encode(),
    )
    .unwrap();
}

fn assert_closed(stream: &mut LocalStream) {
    match read_frame(stream) {
        Ok(None) => {}
        Err(FrameError::Io(error))
            if matches!(error.raw_os_error(), Some(109 | 232 | 233))
                || matches!(
                    error.kind(),
                    std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
                ) => {}
        other => panic!("expected a closed connection, not another frame or a timeout: {other:?}"),
    }
}

#[test]
fn controller_inspection_and_discovery_use_the_existing_typed_payloads() {
    let root = Root::new();
    let peer = root.serve(|mut stream| {
        let inspect = request(&mut stream, Api::InspectArtifact);
        assert_eq!(inspect.payload, b"\x0a\x01A");
        assert_eq!(inspect.request_id, "CLI-CONTROLLER-1");
        response(
            &mut stream,
            &inspect,
            Status::Ok,
            InspectArtifactResponse {
                format_id: "rockchip".into(),
                content_sha256: "content".into(),
                manifest_sha256: "manifest".into(),
                size_bytes: 12,
                ..Default::default()
            }
            .encode(),
        );
        let discover = request(&mut stream, Api::DiscoverDevices);
        assert!(discover.payload.is_empty());
        assert_eq!(discover.request_id, "CLI-CONTROLLER-2");
        let mut observation = Vec::new();
        wire::write_string(&mut observation, 1, "O");
        wire::write_uint64(&mut observation, 2, 123);
        wire::write_string(&mut observation, 3, "loader");
        wire::write_string(&mut observation, 4, "topology");
        wire::write_string(&mut observation, 5, "descriptor");
        wire::write_string(&mut observation, 6, "serial");
        let mut payload = Vec::new();
        wire::write_message(&mut payload, 1, &observation);
        response(&mut stream, &discover, Status::Ok, payload);
    });
    let mut client =
        ControllerClient::connect_with_read_timeout(&root.0, Duration::from_secs(30)).unwrap();
    let inspected = client.artifact_show("A").unwrap();
    assert_eq!(inspected.format_id, "rockchip");
    assert_eq!(inspected.content_sha256, "content");
    assert_eq!(inspected.manifest_sha256, "manifest");
    assert_eq!(inspected.size_bytes, 12);
    let observations = client.device_list().unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].observation_id, "O");
    assert_eq!(observations[0].observed_at_epoch_ms, 123);
    assert_eq!(observations[0].topology_sha256, "topology");
    assert_eq!(observations[0].descriptor_sha256, "descriptor");
    peer.join().unwrap();
}

#[test]
fn import_streams_bounded_frames_then_a_terminator_and_keeps_the_connection() {
    const CHUNK: usize = 4 * 1024 * 1024;
    let root = Root::new();
    let path = root.0.join("caller-private-name.bin");
    let bytes: Vec<_> = (0..CHUNK + 19).map(|i| (i % 251) as u8).collect();
    std::fs::write(&path, &bytes).unwrap();
    let peer = root.serve(move |mut stream| {
        let import = request(&mut stream, Api::ImportArtifact);
        let mut reader = wire::Reader::new(&import.payload);
        let (field, value) = reader.next_field().unwrap().unwrap();
        assert_eq!(field, 1);
        assert_eq!(value.as_u64().unwrap(), bytes.len() as u64);
        let (field, value) = reader.next_field().unwrap().unwrap();
        assert_eq!(field, 2);
        assert_eq!(value.as_str(2).unwrap(), "a".repeat(64));
        assert!(
            reader.next_field().unwrap().is_none(),
            "no caller path on the wire"
        );
        assert_eq!(read_frame(&mut stream).unwrap().unwrap(), bytes[..CHUNK]);
        assert_eq!(read_frame(&mut stream).unwrap().unwrap(), bytes[CHUNK..]);
        assert!(read_frame(&mut stream).unwrap().unwrap().is_empty());
        let mut body = Vec::new();
        wire::write_string(&mut body, 1, "artifact");
        wire::write_string(&mut body, 2, &"a".repeat(64));
        wire::write_uint64(&mut body, 3, bytes.len() as u64);
        wire::write_bool(&mut body, 4, true);
        response(&mut stream, &import, Status::Ok, body);
        let discover = request(&mut stream, Api::DiscoverDevices);
        assert_eq!(discover.request_id, "CLI-CONTROLLER-2");
        response(&mut stream, &discover, Status::Ok, Vec::new());
    });
    let mut client = ControllerClient::connect(&root.0).unwrap();
    let imported = client.import_artifact(&path, &"a".repeat(64)).unwrap();
    assert_eq!(imported.artifact_id, "artifact");
    assert_eq!(imported.sha256, "a".repeat(64));
    assert_eq!(imported.size_bytes, (CHUNK + 19) as u64);
    assert!(imported.deduplicated);
    assert!(client.device_list().unwrap().is_empty());
    peer.join().unwrap();
}

#[test]
fn empty_import_still_terminates_and_a_local_file_error_sends_no_request() {
    let root = Root::new();
    let path = root.0.join("empty");
    std::fs::write(&path, []).unwrap();
    let peer = root.serve(|mut stream| {
        let import = request(&mut stream, Api::ImportArtifact);
        assert_eq!(import.request_id, "CLI-CONTROLLER-1");
        assert!(import.payload.is_empty());
        assert!(read_frame(&mut stream).unwrap().unwrap().is_empty());
        response(
            &mut stream,
            &import,
            Status::Ok,
            b"\x0a\x01A\x12\x01D".to_vec(),
        );
    });
    let mut client = ControllerClient::connect(&root.0).unwrap();
    assert!(client.import_artifact(&root.0.join("absent"), "").is_err());
    assert_eq!(client.import_artifact(&path, "").unwrap().size_bytes, 0);
    peer.join().unwrap();
}

#[test]
fn a_daemon_refusal_is_preserved_and_a_non_import_connection_remains_usable() {
    let root = Root::new();
    let peer = root.serve(|mut stream| {
        let inspect = request(&mut stream, Api::InspectArtifact);
        response(
            &mut stream,
            &inspect,
            Status::NotFound,
            ErrorBody {
                code: "ARTIFACT_NOT_FOUND".into(),
                message: "not in this store".into(),
            }
            .encode(),
        );
        let discover = request(&mut stream, Api::DiscoverDevices);
        response(&mut stream, &discover, Status::Ok, Vec::new());
    });
    let mut client = ControllerClient::connect(&root.0).unwrap();
    let error = client.artifact_show("absent").unwrap_err();
    assert_eq!(error.code, "ARTIFACT_NOT_FOUND");
    assert_eq!(error.message, "not in this store");
    assert_eq!(error.exit_code, 5);
    assert!(!error.retryable);
    assert!(client.device_list().unwrap().is_empty());
    peer.join().unwrap();
}

#[test]
fn an_import_refusal_discards_the_connection_without_retrying() {
    let root = Root::new();
    let path = root.0.join("empty");
    std::fs::write(&path, []).unwrap();
    let peer = root.serve(|mut stream| {
        let import = request(&mut stream, Api::ImportArtifact);
        assert!(read_frame(&mut stream).unwrap().unwrap().is_empty());
        response(
            &mut stream,
            &import,
            Status::Refused,
            ErrorBody {
                code: "IMPORT_REFUSED".into(),
                message: "digest mismatch".into(),
            }
            .encode(),
        );
        assert_closed(&mut stream);
    });
    let mut client = ControllerClient::connect(&root.0).unwrap();
    let error = client.import_artifact(&path, "a").unwrap_err();
    assert_eq!(error.code, "IMPORT_REFUSED");
    assert_eq!(error.message, "digest mismatch");
    assert!(client.device_list().is_err());
    peer.join().unwrap();
}

#[test]
fn mismatched_envelopes_and_truncated_frames_close_the_connection() {
    for failure in ["request-id", "api", "truncated"] {
        let root = Root::new();
        let peer = root.serve(move |mut stream| {
            let req = request(&mut stream, Api::DiscoverDevices);
            if failure == "truncated" {
                stream.write_all(&[0, 0, 0, 4, 1]).unwrap();
                stream.flush().unwrap();
            } else {
                write_frame(
                    &mut stream,
                    &Response {
                        request_id: if failure == "request-id" {
                            "different".into()
                        } else {
                            req.request_id
                        },
                        api: if failure == "api" {
                            Api::InspectArtifact
                        } else {
                            req.api
                        },
                        status: Status::Ok,
                        payload: Vec::new(),
                        stream_sequence: 0,
                        stream_end: true,
                    }
                    .encode(),
                )
                .unwrap();
                assert_closed(&mut stream);
            }
        });
        let mut client = ControllerClient::connect(&root.0).unwrap();
        let error = client.device_list().unwrap_err();
        assert_eq!(
            error.code,
            if failure == "truncated" {
                "CONTROLLER_IO_FAILED"
            } else {
                "CONTROLLER_RESPONSE_INVALID"
            }
        );
        assert!(
            client.device_list().is_err(),
            "no new request after {failure}"
        );
        peer.join().unwrap();
    }
}

#[test]
fn a_silent_handshake_and_a_silent_response_have_bounded_reads() {
    let timeout = Duration::from_millis(250);
    let root = Root::new();
    let mut listener = root.listener();
    let peer = thread::spawn(move || {
        let mut stream = listener.accept().unwrap();
        read_frame(&mut stream).unwrap().unwrap();
        assert_closed(&mut stream);
    });
    let started = Instant::now();
    let error = ControllerClient::connect_with_read_timeout(&root.0, timeout).unwrap_err();
    assert_eq!(error.code, "CONTROLLER_IO_FAILED");
    assert!(started.elapsed() < Duration::from_secs(10));
    peer.join().unwrap();

    let root = Root::new();
    let peer = root.serve(|mut stream| {
        request(&mut stream, Api::DiscoverDevices);
        assert_closed(&mut stream);
    });
    let mut client = ControllerClient::connect_with_read_timeout(&root.0, timeout).unwrap();
    let started = Instant::now();
    assert_eq!(
        client.device_list().unwrap_err().code,
        "CONTROLLER_IO_FAILED"
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(client.device_list().is_err());
    peer.join().unwrap();
}

#[test]
fn zero_timeout_is_rejected_before_connecting() {
    let root = Root::new();
    assert_eq!(
        ControllerClient::connect_with_read_timeout(&root.0, Duration::ZERO)
            .unwrap_err()
            .code,
        "INVALID_TIMEOUT"
    );
    assert_eq!(
        PublicClient::connect_with_read_timeout(&root.0, Duration::ZERO)
            .unwrap_err()
            .code,
        "INVALID_TIMEOUT"
    );
}

#[test]
fn losing_the_peer_during_upload_discards_the_connection() {
    let root = Root::new();
    let path = root.0.join("archive");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(8 * 1024 * 1024).unwrap();
    drop(file);
    let peer = root.serve(|mut stream| {
        request(&mut stream, Api::ImportArtifact);
        // Close before accepting the content; this is not an import receipt.
    });
    let mut client = ControllerClient::connect(&root.0).unwrap();
    assert_eq!(
        client.import_artifact(&path, "").unwrap_err().code,
        "CONTROLLER_IO_FAILED"
    );
    assert!(client.device_list().is_err());
    peer.join().unwrap();
}

#[test]
fn malformed_typed_controller_payloads_are_not_successes() {
    for api in [
        Api::InspectArtifact,
        Api::DiscoverDevices,
        Api::ImportArtifact,
    ] {
        let root = Root::new();
        let path = root.0.join("empty");
        std::fs::write(&path, []).unwrap();
        let peer = root.serve(move |mut stream| {
            let req = request(&mut stream, api);
            if api == Api::ImportArtifact {
                assert!(read_frame(&mut stream).unwrap().unwrap().is_empty());
            }
            response(&mut stream, &req, Status::Ok, vec![0x0a, 0x05, b'A']);
        });
        let mut client = ControllerClient::connect(&root.0).unwrap();
        let error = match api {
            Api::InspectArtifact => client.artifact_show("A").unwrap_err(),
            Api::DiscoverDevices => client.device_list().unwrap_err(),
            Api::ImportArtifact => client.import_artifact(&path, "").unwrap_err(),
            _ => unreachable!(),
        };
        assert!(matches!(
            error.code.as_str(),
            "CONTROLLER_RESPONSE_INVALID" | "IPC_RESPONSE_INVALID"
        ));
        peer.join().unwrap();
    }
}

#[test]
fn public_assessment_keeps_its_existing_payload_and_its_read_timeout() {
    use arkforge_ipc::messages::{Assessment, MaterializePlanResponse};
    let root = Root::new();
    let peer = root.serve_kind(SessionKind::Public, |mut stream| {
        let assess = request(&mut stream, Api::MaterializePlan);
        assert_eq!(
            assess.payload,
            b"\x0a\x01A\x12\x01P\x1a\x01O\x22\x0bfullRestore"
        );
        response(
            &mut stream,
            &assess,
            Status::Ok,
            MaterializePlanResponse::Assessment(Assessment {
                mechanics_maturity_key_sha256: "mechanics".into(),
                ..Default::default()
            })
            .encode(),
        );
        request(&mut stream, Api::DiscoverDevices);
        // The client must close after the read timeout, with no next request.
        assert_closed(&mut stream);
    });
    let mut client =
        PublicClient::connect_with_read_timeout(&root.0, Duration::from_millis(250)).unwrap();
    assert_eq!(
        client
            .flash_assess("A", "P", "O")
            .unwrap()
            .mechanics_maturity_key_sha256,
        "mechanics"
    );
    let started = Instant::now();
    assert!(client.device_list().is_err());
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(client.device_list().is_err());
    peer.join().unwrap();
}

#[test]
fn a_bounded_public_client_still_refuses_an_executable_plan() {
    use arkforge_ipc::messages::{ExecutablePlan, MaterializePlanResponse};
    let root = Root::new();
    let peer = root.serve_kind(SessionKind::Public, |mut stream| {
        let assess = request(&mut stream, Api::MaterializePlan);
        response(
            &mut stream,
            &assess,
            Status::Ok,
            MaterializePlanResponse::Plan(ExecutablePlan {
                plan_id: "forbidden".into(),
                ..Default::default()
            })
            .encode(),
        );
    });
    let mut client =
        PublicClient::connect_with_read_timeout(&root.0, Duration::from_secs(30)).unwrap();
    assert_eq!(
        client.flash_assess("A", "P", "O").unwrap_err().code,
        "PUBLIC_ASSESSMENT_VIOLATION"
    );
    peer.join().unwrap();
}
