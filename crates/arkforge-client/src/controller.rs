//! Controller-socket client used only by the persistent authority supervisor.
//!
//! Short-lived `arkforge` commands never construct this type. Keeping it in a
//! separate module makes the capability direction reviewable: public commands
//! use `PublicClient`; only the supervisor can import, inspect, discover,
//! materialize, start, cancel, reconcile or answer admissions.

use crate::{ClientError, DeviceObservationView, PublicRuntimeInfo};
use arkforge_ipc::framing::{read_frame, write_frame};
use arkforge_ipc::messages::{
    ErrorBody, Hello, HelloAck, ImportArtifactRequest, ImportArtifactResponse,
    InspectArtifactResponse, JobEvent, MaterializePlanResponse, Request, Response,
    SubmissionOutcome, SubmitManagedControlReceiptRequest, SubmitStepPermitRequest,
    WatchJobRequest,
};
use arkforge_ipc::{Api, PROTOCOL_MAJOR, PROTOCOL_MINOR, SessionKind, Status, wire};
use arkforge_platform::{LocalChannel, LocalEndpoint, LocalStream};
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const IMPORT_CHUNK_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug)]
pub struct ControllerClient {
    stream: Option<LocalStream>,
    next_request: u64,
    runtime_info: PublicRuntimeInfo,
}

#[derive(Debug, Clone)]
pub struct MaterializeInput<'a> {
    pub artifact_id: &'a str,
    pub profile_id: &'a str,
    pub device_id: &'a str,
    pub toolchain_id: &'a str,
    pub authority_namespace: &'a str,
    pub binding_id: &'a str,
    pub binding_revision: u64,
    pub stable_identity_sha256: &'a [u8],
    pub execution_purpose: &'a str,
    pub authority_support_key_sha256: &'a [u8],
    pub authority_support_state: &'a str,
    pub authority_support_detail: &'a str,
}

impl MaterializeInput<'_> {
    fn encode(&self) -> Vec<u8> {
        let mut payload = Vec::new();
        wire::write_string(&mut payload, 1, self.artifact_id);
        wire::write_string(&mut payload, 2, self.profile_id);
        wire::write_string(&mut payload, 3, self.device_id);
        wire::write_string(&mut payload, 4, "fullRestore");
        wire::write_string(&mut payload, 5, self.toolchain_id);
        wire::write_string(&mut payload, 6, self.authority_namespace);
        wire::write_string(&mut payload, 7, self.binding_id);
        wire::write_uint64(&mut payload, 8, self.binding_revision);
        wire::write_bytes(&mut payload, 9, self.stable_identity_sha256);
        wire::write_string(&mut payload, 10, self.execution_purpose);
        wire::write_bytes(&mut payload, 11, self.authority_support_key_sha256);
        wire::write_string(&mut payload, 12, self.authority_support_state);
        wire::write_string(&mut payload, 13, self.authority_support_detail);
        payload
    }
}

impl ControllerClient {
    /// The Swift SDK's bound for waiting on archive import/inspection replies.
    /// This is a per-read idle timeout, not a whole-exchange or write deadline.
    pub const MATERIALIZATION_READ_TIMEOUT: Duration = Duration::from_secs(900);

    pub fn connect(runtime_dir: &Path) -> Result<Self, ClientError> {
        Self::connect_inner(runtime_dir, None)
    }

    /// Opens a separate connection for bounded materialization response reads.
    ///
    /// Use `connect` for the execution session, whose job waits remain unbounded.
    /// A timeout or interrupted exchange closes this connection; it never retries
    /// a request or lets a late response become the next request's answer.
    /// This does not bound writes or the total duration of a peer's slow reply.
    pub fn connect_with_read_timeout(
        runtime_dir: &Path,
        timeout: Duration,
    ) -> Result<Self, ClientError> {
        if timeout.is_zero() {
            return Err(ClientError::new(
                "INVALID_TIMEOUT",
                "The controller read timeout must be positive.",
                2,
                false,
            ));
        }
        Self::connect_inner(runtime_dir, Some(timeout))
    }

    fn connect_inner(
        runtime_dir: &Path,
        read_timeout: Option<Duration>,
    ) -> Result<Self, ClientError> {
        let endpoint = LocalEndpoint::for_runtime(runtime_dir, LocalChannel::Controller);
        let mut stream = LocalStream::connect(&endpoint).map_err(|error| {
            ClientError::new(
                "CONTROLLER_UNAVAILABLE",
                format!("Cannot connect to {}: {error}", endpoint.display()),
                5,
                true,
            )
        })?;
        stream
            .set_read_timeout(Some(
                read_timeout
                    .unwrap_or(HANDSHAKE_TIMEOUT)
                    .min(HANDSHAKE_TIMEOUT),
            ))
            .map_err(|error| transport("bound the controller handshake", error))?;
        let hello = Hello {
            protocol_major: PROTOCOL_MAJOR,
            protocol_minor: PROTOCOL_MINOR,
            session_kind: SessionKind::Controller,
        };
        write_frame(&mut stream, &hello.encode())
            .map_err(|error| transport("send the controller handshake", error))?;
        let frame = read_frame(&mut stream)
            .map_err(|error| transport("read the controller handshake", error))?
            .ok_or_else(|| {
                ClientError::new(
                    "CONTROLLER_UNAVAILABLE",
                    "arkforged closed during the controller handshake.",
                    5,
                    true,
                )
            })?;
        let ack = HelloAck::decode(&frame)
            .map_err(|error| invalid_response("decode the controller handshake", error))?;
        if let Some(refusal) = ack.refusal {
            return Err(ClientError::new("PROTOCOL_REFUSED", refusal, 3, false));
        }
        if ack.protocol_major != PROTOCOL_MAJOR || ack.session_kind != SessionKind::Controller {
            return Err(ClientError::new(
                "PROTOCOL_REFUSED",
                "arkforged did not acknowledge the requested controller protocol.",
                3,
                false,
            ));
        }
        stream
            .set_read_timeout(read_timeout)
            .map_err(|error| transport("configure the controller response wait", error))?;
        Ok(Self {
            stream: Some(stream),
            next_request: 1,
            runtime_info: PublicRuntimeInfo {
                protocol_major: ack.protocol_major,
                protocol_minor: ack.protocol_minor,
                daemon_version: ack.daemon_version,
                execution_ready: ack.execution_ready,
                execution_blockers: ack.execution_blockers,
                toolchain_id: ack.toolchain_id,
                toolchain_sha256: ack.toolchain_sha256,
            },
        })
    }

    /// The standing facts the daemon acknowledged this session with: its
    /// readiness to execute and the toolchain it bound, as the public session
    /// reports them too.
    pub fn runtime_info(&self) -> &PublicRuntimeInfo {
        &self.runtime_info
    }

    /// Inspects an object already held by this daemon, using its typed manifest.
    pub fn artifact_show(
        &mut self,
        artifact_id: &str,
    ) -> Result<InspectArtifactResponse, ClientError> {
        let mut payload = Vec::new();
        wire::write_string(&mut payload, 1, artifact_id);
        InspectArtifactResponse::decode(&self.call(Api::InspectArtifact, payload)?)
            .map_err(|error| invalid_response("decode inspectArtifact", error))
    }

    /// Observes devices through the controller session without taking a device.
    pub fn device_list(&mut self) -> Result<Vec<DeviceObservationView>, ClientError> {
        crate::public::decode_observations(&self.call(Api::DiscoverDevices, Vec::new())?)
    }

    /// Streams a regular file through the existing controller import protocol.
    /// Size comes from the opened file, and content uses at most 4 MiB per frame.
    /// The daemon remains responsible for checking the expected content digest.
    pub fn import_artifact(
        &mut self,
        path: &Path,
        expected_sha256: &str,
    ) -> Result<ImportArtifactResponse, ClientError> {
        let mut file = File::open(path).map_err(|error| transport("open the artifact", error))?;
        let metadata = file
            .metadata()
            .map_err(|error| transport("read the artifact size", error))?;
        if !metadata.is_file() {
            return Err(ClientError::new(
                "INVALID_ARTIFACT_FILE",
                "The artifact must be a regular file.",
                2,
                false,
            ));
        }
        let header = ImportArtifactRequest {
            expected_size_bytes: metadata.len(),
            expected_sha256: expected_sha256.to_owned(),
        };
        let response = self.exchange(
            Api::ImportArtifact,
            header.encode(),
            Some((&mut file, metadata.len())),
        )?;
        ImportArtifactResponse::decode(&response)
            .map_err(|error| invalid_response("decode importArtifact", error))
    }

    pub fn materialize_plan(
        &mut self,
        input: &MaterializeInput<'_>,
    ) -> Result<MaterializePlanResponse, ClientError> {
        let payload = input.encode();
        let response = self.call(Api::MaterializePlan, payload)?;
        MaterializePlanResponse::decode(&response)
            .map_err(|error| invalid_response("decode materializePlan", error))
    }

    pub fn start_execution(
        &mut self,
        plan_id: &str,
        plan_sha256: &str,
        execution_purpose: &str,
        controller_session_id: &str,
    ) -> Result<String, ClientError> {
        let mut payload = Vec::new();
        wire::write_string(&mut payload, 1, plan_id);
        wire::write_string(&mut payload, 2, plan_sha256);
        wire::write_string(&mut payload, 3, execution_purpose);
        wire::write_string(&mut payload, 4, controller_session_id);
        let response = self.call(Api::StartExecution, payload)?;
        first_string(&response, 1, "startExecution job id")
    }

    pub fn job_events(
        &mut self,
        job_id: &str,
        after_sequence: u64,
    ) -> Result<Vec<JobEvent>, ClientError> {
        let response = self.call(
            Api::WatchJob,
            WatchJobRequest {
                job_id: job_id.to_string(),
                from_sequence: after_sequence,
            }
            .encode(),
        )?;
        let mut events = Vec::new();
        let mut reader = wire::Reader::new(&response);
        while let Some((field, value)) = reader
            .next_field()
            .map_err(|error| invalid_response("decode watchJob", error))?
        {
            if field == 1 {
                events.push(
                    JobEvent::decode(
                        value
                            .as_bytes()
                            .map_err(|error| invalid_response("decode job event", error))?,
                    )
                    .map_err(|error| invalid_response("decode job event", error))?,
                );
            }
        }
        Ok(events)
    }

    pub fn cancel(&mut self, job_id: &str, expected_sequence: u64) -> Result<String, ClientError> {
        let mut payload = Vec::new();
        wire::write_string(&mut payload, 1, job_id);
        wire::write_uint64(&mut payload, 2, expected_sequence);
        let response = self.call(Api::CancelJob, payload)?;
        first_string(&response, 1, "cancelJob state")
    }

    pub fn reconcile(&mut self, job_id: &str) -> Result<Vec<u8>, ClientError> {
        let mut payload = Vec::new();
        wire::write_string(&mut payload, 1, job_id);
        self.call(Api::ReconcileJob, payload)
    }

    pub fn plan_superseding_recovery(&mut self, job_id: &str) -> Result<Vec<u8>, ClientError> {
        let mut payload = Vec::new();
        wire::write_string(&mut payload, 1, job_id);
        self.call(Api::PlanSupersedingRecovery, payload)
    }

    pub fn submit_permit(
        &mut self,
        submission: &SubmitStepPermitRequest,
    ) -> Result<SubmissionOutcome, ClientError> {
        let response = self.call(Api::SubmitStepPermit, submission.encode())?;
        SubmissionOutcome::decode(&response)
            .map_err(|error| invalid_response("decode submitStepPermit", error))
    }

    pub fn submit_control_receipt(
        &mut self,
        receipt: &SubmitManagedControlReceiptRequest,
    ) -> Result<SubmissionOutcome, ClientError> {
        let response = self.call(Api::SubmitManagedControlReceipt, receipt.encode())?;
        SubmissionOutcome::decode(&response)
            .map_err(|error| invalid_response("decode submitManagedControlReceipt", error))
    }

    fn call(&mut self, api: Api, payload: Vec<u8>) -> Result<Vec<u8>, ClientError> {
        self.exchange(api, payload, None)
    }

    fn exchange(
        &mut self,
        api: Api,
        payload: Vec<u8>,
        content: Option<(&mut File, u64)>,
    ) -> Result<Vec<u8>, ClientError> {
        // Retain the connection only after a complete, correlated response.
        // A partial frame, failed upload or timed-out read cannot be resumed.
        let mut stream = self.stream.take().ok_or_else(|| {
            transport(
                "send a controller request",
                "connection is closed after a failed exchange",
            )
        })?;
        let request = Request {
            request_id: format!("CLI-CONTROLLER-{}", self.next_request),
            api,
            payload,
        };
        self.next_request += 1;
        write_frame(&mut stream, &request.encode())
            .map_err(|error| transport("send a controller request", error))?;
        if let Some((file, size)) = content {
            stream_artifact(&mut stream, file, size)?;
        }
        let frame = read_frame(&mut stream)
            .map_err(|error| transport("read a controller response", error))?
            .ok_or_else(|| transport("read a controller response", "connection closed"))?;
        let response = Response::decode(&frame)
            .map_err(|error| invalid_response("decode controller response", error))?;
        if response.api != api || response.request_id != request.request_id {
            return Err(invalid_response(
                "match controller response",
                "api or request id differs",
            ));
        }
        if response.status == Status::Ok {
            self.stream = Some(stream);
            return Ok(response.payload);
        }
        let error = ErrorBody::decode(&response.payload)
            .map_err(|decode| invalid_response("decode controller refusal", decode))?;
        // Import may be refused before the daemon drains the content frames.
        // Such a connection cannot safely carry a subsequent request.
        if api != Api::ImportArtifact {
            self.stream = Some(stream);
        }
        let exit = match error.code.as_str() {
            "UNKNOWN_JOB" | "PROFILE_NOT_FOUND" | "ARTIFACT_NOT_INSPECTED" => 5,
            "STALE_JOB_SEQUENCE" | "PLAN_NOT_STARTABLE" => 6,
            _ => match response.status {
                Status::InvalidArgument => 2,
                Status::Refused | Status::Unavailable => 3,
                Status::NotFound => 5,
                Status::Internal => 10,
                Status::Ok => 10,
            },
        };
        Err(ClientError::new(error.code, error.message, exit, false))
    }
}

fn stream_artifact(
    stream: &mut impl Write,
    file: &mut impl Read,
    size: u64,
) -> Result<(), ClientError> {
    let mut remaining = size;
    let mut buffer = vec![0; IMPORT_CHUNK_BYTES];
    while remaining > 0 {
        let count = remaining.min(buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..count])
            .map_err(|error| transport("read the artifact content", error))?;
        write_frame(stream, &buffer[..count])
            .map_err(|error| transport("send the artifact content", error))?;
        remaining -= count as u64;
    }
    let mut extra = [0];
    if file
        .read(&mut extra)
        .map_err(|error| transport("finish reading the artifact", error))?
        != 0
    {
        return Err(transport(
            "send the artifact",
            "file size changed during import",
        ));
    }
    write_frame(stream, &[]).map_err(|error| transport("finish the artifact stream", error))
}

fn first_string(payload: &[u8], field: u32, context: &str) -> Result<String, ClientError> {
    let mut reader = wire::Reader::new(payload);
    while let Some((found, value)) = reader
        .next_field()
        .map_err(|error| invalid_response(context, error))?
    {
        if found == field {
            return Ok(value
                .as_str(field)
                .map_err(|error| invalid_response(context, error))?
                .to_string());
        }
    }
    Err(invalid_response(context, "required field is missing"))
}

fn transport(context: &str, error: impl std::fmt::Display) -> ClientError {
    ClientError::new(
        "CONTROLLER_IO_FAILED",
        format!("Cannot {context}: {error}"),
        10,
        true,
    )
}

fn invalid_response(context: &str, error: impl std::fmt::Display) -> ClientError {
    ClientError::new(
        "CONTROLLER_RESPONSE_INVALID",
        format!("Cannot {context}: {error}"),
        10,
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_changed_artifact_size_never_sends_a_successful_terminator() {
        let mut frames = Vec::new();
        let error = stream_artifact(&mut frames, &mut b"short".as_slice(), 10).unwrap_err();
        assert_eq!(error.code, "CONTROLLER_IO_FAILED");
        assert!(frames.is_empty());

        let error = stream_artifact(&mut frames, &mut b"grew".as_slice(), 3).unwrap_err();
        assert_eq!(error.code, "CONTROLLER_IO_FAILED");
        let mut reader = frames.as_slice();
        assert_eq!(read_frame(&mut reader).unwrap(), Some(b"gre".to_vec()));
        assert_eq!(
            read_frame(&mut reader).unwrap(),
            None,
            "no empty success terminator"
        );
    }

    #[test]
    fn materialize_production_encoder_matches_swift_golden() {
        let input = MaterializeInput {
            artifact_id: "A",
            profile_id: "P",
            device_id: "O",
            toolchain_id: "T",
            authority_namespace: "N",
            binding_id: "B",
            binding_revision: 7,
            stable_identity_sha256: &[0xaa, 0xbb],
            execution_purpose: "primary",
            authority_support_key_sha256: &[0xde, 0xad],
            authority_support_state: "hardwareCampaign",
            authority_support_detail: "AFA-AC-8",
        };
        let hex: String = input
            .encode()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(
            hex,
            "0a01411201501a014f220b66756c6c526573746f72652a015432014e3a014240074a02aabb52077072696d6172795a02dead6210686172647761726543616d706169676e6a084146412d41432d38"
        );
    }
}
