//! Typed client for ArkForge's public, read-only local endpoint.
//!
//! The canonical CLI derives `public.sock` from one runtime directory. It does
//! not accept an arbitrary socket path and it never opens the controller
//! authority surface.

use arkforge_ipc::framing::{read_frame, write_frame};
use arkforge_ipc::messages::{
    Assessment, ErrorBody, Hello, HelloAck, InspectArtifactResponse, JobEvent, JobSummary,
    KeyValue, MaterializePlanResponse, Request, Response, WatchJobRequest,
};
use arkforge_ipc::{Api, PROTOCOL_MAJOR, PROTOCOL_MINOR, SessionKind, Status, wire};
use arkforge_platform::{LocalChannel, LocalEndpoint, LocalStream};
use std::path::Path;
use std::time::Duration;

/// How long the public handshake waits for the daemon's first word.
///
/// Connecting proves a pipe exists, not that anyone is serving it: a Windows
/// `bind` publishes a connectable instance before `serve` accepts. This bounds
/// only the handshake — a session that has been acknowledged goes back to
/// waiting as long as the work takes.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientError {
    pub code: String,
    pub message: String,
    pub exit_code: i32,
    pub retryable: bool,
}

/// Compatibility name retained for existing CLI consumers.
pub type PublicClientError = ClientError;

impl ClientError {
    pub(crate) fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        exit_code: i32,
        retryable: bool,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            exit_code,
            retryable,
        }
    }

    fn transport(message: impl Into<String>) -> Self {
        Self::new("IPC_IO_FAILED", message, 10, true)
    }

    fn decode(context: &str, error: impl std::fmt::Display) -> Self {
        Self::new(
            "IPC_RESPONSE_INVALID",
            format!("{context}: {error}"),
            10,
            false,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceObservationView {
    pub observation_id: String,
    pub observed_at_epoch_ms: u64,
    pub mode: String,
    pub topology_sha256: String,
    pub descriptor_sha256: String,
    pub identity_strength: String,
    pub malformed_descriptor: bool,
    pub protocol_identity: Vec<KeyValue>,
    /// Domain-separated digest only; the raw descriptor serial/connect key is
    /// never exposed on the public socket.
    pub serial_sha256: String,
    pub serial_evidence_kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceProbeView {
    pub observation: DeviceObservationView,
    pub protocol_facts: Vec<KeyValue>,
    pub profile_id: String,
    pub facts_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecoveryGuideView {
    pub job_id: String,
    pub original_state: String,
    pub original_outcome_immutable: bool,
    pub automatic_replay_forbidden: bool,
    pub actions: Vec<String>,
    pub complete_overwrite_supported: bool,
    pub contract_id: String,
    pub contract_version: String,
    pub contract_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicRuntimeInfo {
    pub protocol_major: u32,
    pub protocol_minor: u32,
    pub daemon_version: String,
    pub execution_ready: bool,
    pub execution_blockers: Vec<String>,
    pub toolchain_id: String,
    pub toolchain_sha256: String,
}

#[derive(Debug)]
pub struct PublicClient {
    stream: LocalStream,
    next_request: u64,
    runtime_info: PublicRuntimeInfo,
}

impl PublicClient {
    pub fn connect(runtime_dir: &Path) -> Result<Self, ClientError> {
        let endpoint = LocalEndpoint::for_runtime(runtime_dir, LocalChannel::Public);
        let mut stream = LocalStream::connect(&endpoint).map_err(|error| {
            ClientError::new(
                "DAEMON_UNAVAILABLE",
                format!(
                    "Cannot connect to the ArkForge public runtime at {}: {error}",
                    endpoint.display()
                ),
                5,
                true,
            )
        })?;
        stream
            .set_read_timeout(Some(HANDSHAKE_TIMEOUT))
            .map_err(|error| {
                ClientError::transport(format!("Cannot bound the public handshake: {error}"))
            })?;
        let hello = Hello {
            protocol_major: PROTOCOL_MAJOR,
            protocol_minor: PROTOCOL_MINOR,
            session_kind: SessionKind::Public,
        };
        write_frame(&mut stream, &hello.encode()).map_err(|error| {
            ClientError::transport(format!("Cannot send the public handshake: {error}"))
        })?;
        let frame = read_frame(&mut stream)
            .map_err(|error| {
                ClientError::transport(format!(
                    "Cannot read the public handshake response: {error}"
                ))
            })?
            .ok_or_else(|| {
                ClientError::transport("The daemon closed during the public handshake.")
            })?;
        let ack = HelloAck::decode(&frame)
            .map_err(|error| ClientError::decode("Invalid public handshake", error))?;
        if let Some(refusal) = ack.refusal {
            return Err(ClientError::new(
                "PROTOCOL_REFUSED",
                format!("The daemon refused the public session: {refusal}"),
                3,
                false,
            ));
        }
        if ack.protocol_major != PROTOCOL_MAJOR || ack.session_kind != SessionKind::Public {
            return Err(ClientError::new(
                "PROTOCOL_REFUSED",
                format!(
                    "The daemon acknowledged protocol {}.{} as {:?}, not ArkForge public protocol {}.{}.",
                    ack.protocol_major,
                    ack.protocol_minor,
                    ack.session_kind,
                    PROTOCOL_MAJOR,
                    PROTOCOL_MINOR
                ),
                3,
                false,
            ));
        }
        // The handshake proved someone is serving. Everything after it may
        // legitimately take as long as it takes — a followed job, a device
        // that is slow to answer — so the wait goes back to unbounded.
        stream.set_read_timeout(None).map_err(|error| {
            ClientError::transport(format!("Cannot restore the public session wait: {error}"))
        })?;
        Ok(Self {
            stream,
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

    pub fn runtime_info(&self) -> &PublicRuntimeInfo {
        &self.runtime_info
    }

    pub fn device_list(&mut self) -> Result<Vec<DeviceObservationView>, ClientError> {
        let payload = self.call(Api::DiscoverDevices, Vec::new())?;
        decode_observations(&payload)
    }

    pub fn device_probe(
        &mut self,
        device_id: &str,
        profile_id: &str,
    ) -> Result<DeviceProbeView, ClientError> {
        let mut request = Vec::new();
        wire::write_string(&mut request, 1, device_id);
        wire::write_string(&mut request, 2, profile_id);
        let payload = self.call(Api::ProbeDevice, request)?;
        decode_probe(&payload)
    }

    pub fn artifact_show(
        &mut self,
        artifact_id: &str,
    ) -> Result<InspectArtifactResponse, ClientError> {
        let mut request = Vec::new();
        wire::write_string(&mut request, 1, artifact_id);
        let payload = self.call(Api::InspectArtifact, request)?;
        InspectArtifactResponse::decode(&payload)
            .map_err(|error| ClientError::decode("Invalid artifact manifest", error))
    }

    pub fn flash_assess(
        &mut self,
        artifact_id: &str,
        profile_id: &str,
        device_id: &str,
    ) -> Result<Assessment, ClientError> {
        let mut request = Vec::new();
        wire::write_string(&mut request, 1, artifact_id);
        wire::write_string(&mut request, 2, profile_id);
        wire::write_string(&mut request, 3, device_id);
        wire::write_string(&mut request, 4, "fullRestore");
        let payload = self.call(Api::MaterializePlan, request)?;
        match MaterializePlanResponse::decode(&payload)
            .map_err(|error| ClientError::decode("Invalid flash assessment", error))?
        {
            MaterializePlanResponse::Assessment(assessment) => Ok(assessment),
            MaterializePlanResponse::Plan(_) => Err(ClientError::new(
                "PUBLIC_ASSESSMENT_VIOLATION",
                "The public runtime returned an executable plan to a read-only client.",
                10,
                false,
            )),
        }
    }

    pub fn job_list(&mut self) -> Result<Vec<JobSummary>, ClientError> {
        let payload = self.call(Api::ListJobs, Vec::new())?;
        decode_job_summaries(&payload)
    }

    pub fn job_show(&mut self, job_id: &str) -> Result<JobSummary, ClientError> {
        let mut request = Vec::new();
        wire::write_string(&mut request, 1, job_id);
        let payload = self.call(Api::GetJob, request)?;
        let mut summaries = decode_job_summaries(&payload)?;
        if summaries.len() != 1 {
            return Err(ClientError::new(
                "IPC_RESPONSE_INVALID",
                format!(
                    "getJob returned {} summaries; exactly one is required.",
                    summaries.len()
                ),
                10,
                false,
            ));
        }
        Ok(summaries.remove(0))
    }

    pub fn job_events(
        &mut self,
        job_id: &str,
        after_sequence: u64,
    ) -> Result<Vec<JobEvent>, ClientError> {
        let request = WatchJobRequest {
            job_id: job_id.to_string(),
            from_sequence: after_sequence,
        };
        let payload = self.call(Api::WatchJob, request.encode())?;
        let mut events = Vec::new();
        let mut reader = wire::Reader::new(&payload);
        while let Some((field, value)) = reader
            .next_field()
            .map_err(|error| ClientError::decode("Invalid job event list", error))?
        {
            if field == 1 {
                events.push(
                    JobEvent::decode(
                        value
                            .as_bytes()
                            .map_err(|error| ClientError::decode("Invalid job event", error))?,
                    )
                    .map_err(|error| ClientError::decode("Invalid job event", error))?,
                );
            }
        }
        if events
            .windows(2)
            .any(|pair| pair[0].sequence >= pair[1].sequence)
        {
            return Err(ClientError::new(
                "IPC_RESPONSE_INVALID",
                "watchJob returned events outside strict sequence order.",
                10,
                false,
            ));
        }
        Ok(events)
    }

    pub fn recovery_guide(&mut self, job_id: &str) -> Result<RecoveryGuideView, ClientError> {
        let mut request = Vec::new();
        wire::write_string(&mut request, 1, job_id);
        let payload = self.call(Api::GetRecoveryGuide, request)?;
        decode_recovery_guide(&payload)
    }

    fn call(&mut self, api: Api, payload: Vec<u8>) -> Result<Vec<u8>, ClientError> {
        let request_id = format!("arkforge-{}-{}", std::process::id(), self.next_request);
        self.next_request += 1;
        let request = Request {
            request_id: request_id.clone(),
            api,
            payload,
        };
        write_frame(&mut self.stream, &request.encode())
            .map_err(|error| ClientError::transport(format!("Cannot send {api}: {error}")))?;
        let frame = read_frame(&mut self.stream)
            .map_err(|error| {
                ClientError::transport(format!("Cannot read the {api} response: {error}"))
            })?
            .ok_or_else(|| {
                ClientError::transport(format!(
                    "The daemon closed before returning the {api} response."
                ))
            })?;
        let response = Response::decode(&frame)
            .map_err(|error| ClientError::decode("Invalid response envelope", error))?;
        if response.request_id != request_id || response.api != api {
            return Err(ClientError::new(
                "IPC_RESPONSE_MISMATCH",
                format!(
                    "Expected {api} response {request_id}, received {} response {}.",
                    response.api, response.request_id
                ),
                10,
                false,
            ));
        }
        if response.status == Status::Ok {
            return Ok(response.payload);
        }
        let error = ErrorBody::decode(&response.payload)
            .map_err(|decode| ClientError::decode("Invalid daemon error", decode))?;
        let (exit_code, retryable) = match response.status {
            Status::InvalidArgument => (2, false),
            Status::NotFound => (5, false),
            Status::Refused => (3, false),
            Status::Unavailable => (3, true),
            Status::Internal => (10, true),
            Status::Ok => unreachable!(),
        };
        Err(ClientError::new(
            error.code,
            error.message,
            exit_code,
            retryable,
        ))
    }
}

fn decode_observations(payload: &[u8]) -> Result<Vec<DeviceObservationView>, ClientError> {
    let mut observations = Vec::new();
    let mut reader = wire::Reader::new(payload);
    while let Some((field, value)) = reader
        .next_field()
        .map_err(|error| ClientError::decode("Invalid device list", error))?
    {
        if field == 1 {
            observations.push(decode_observation(value.as_bytes().map_err(|error| {
                ClientError::decode("Invalid device observation", error)
            })?)?);
        }
    }
    Ok(observations)
}

fn decode_observation(payload: &[u8]) -> Result<DeviceObservationView, ClientError> {
    let mut observation = DeviceObservationView::default();
    let mut reader = wire::Reader::new(payload);
    while let Some((field, value)) = reader
        .next_field()
        .map_err(|error| ClientError::decode("Invalid device observation", error))?
    {
        match field {
            1 => observation.observation_id = string_value(value, field, "device observation")?,
            2 => {
                observation.observed_at_epoch_ms = value.as_u64().map_err(|error| {
                    ClientError::decode("Invalid device observation time", error)
                })?
            }
            3 => observation.mode = string_value(value, field, "device mode")?,
            4 => observation.topology_sha256 = string_value(value, field, "topology digest")?,
            5 => observation.descriptor_sha256 = string_value(value, field, "descriptor digest")?,
            6 => observation.identity_strength = string_value(value, field, "identity strength")?,
            7 => {
                observation.malformed_descriptor = value
                    .as_bool()
                    .map_err(|error| ClientError::decode("Invalid descriptor status", error))?
            }
            8 => {
                observation.protocol_identity.push(
                    KeyValue::decode(value.as_bytes().map_err(|error| {
                        ClientError::decode("Invalid protocol identity", error)
                    })?)
                    .map_err(|error| ClientError::decode("Invalid protocol identity", error))?,
                )
            }
            9 => observation.serial_sha256 = string_value(value, field, "serial digest")?,
            10 => {
                observation.serial_evidence_kind =
                    string_value(value, field, "serial evidence kind")?
            }
            _ => {}
        }
    }
    if observation.observation_id.is_empty() {
        return Err(ClientError::new(
            "IPC_RESPONSE_INVALID",
            "A device observation has no observation_id.",
            10,
            false,
        ));
    }
    Ok(observation)
}

fn decode_probe(payload: &[u8]) -> Result<DeviceProbeView, ClientError> {
    let mut observation = None;
    let mut protocol_facts = Vec::new();
    let mut profile_id = String::new();
    let mut facts_sha256 = String::new();
    let mut reader = wire::Reader::new(payload);
    while let Some((field, value)) = reader
        .next_field()
        .map_err(|error| ClientError::decode("Invalid device probe", error))?
    {
        match field {
            1 => {
                observation = Some(decode_observation(value.as_bytes().map_err(|error| {
                    ClientError::decode("Invalid probed observation", error)
                })?)?)
            }
            2 => protocol_facts.push(
                KeyValue::decode(
                    value
                        .as_bytes()
                        .map_err(|error| ClientError::decode("Invalid probe fact", error))?,
                )
                .map_err(|error| ClientError::decode("Invalid probe fact", error))?,
            ),
            3 => profile_id = string_value(value, field, "probe profile")?,
            4 => facts_sha256 = string_value(value, field, "probe facts digest")?,
            _ => {}
        }
    }
    Ok(DeviceProbeView {
        observation: observation.ok_or_else(|| {
            ClientError::new(
                "IPC_RESPONSE_INVALID",
                "The device probe has no observation.",
                10,
                false,
            )
        })?,
        protocol_facts,
        profile_id,
        facts_sha256,
    })
}

fn decode_job_summaries(payload: &[u8]) -> Result<Vec<JobSummary>, ClientError> {
    let mut summaries = Vec::new();
    let mut reader = wire::Reader::new(payload);
    while let Some((field, value)) = reader
        .next_field()
        .map_err(|error| ClientError::decode("Invalid job list", error))?
    {
        if field == 1 {
            summaries.push(
                JobSummary::decode(
                    value
                        .as_bytes()
                        .map_err(|error| ClientError::decode("Invalid job summary", error))?,
                )
                .map_err(|error| ClientError::decode("Invalid job summary", error))?,
            );
        }
    }
    Ok(summaries)
}

fn decode_recovery_guide(payload: &[u8]) -> Result<RecoveryGuideView, ClientError> {
    let mut guide = RecoveryGuideView::default();
    let mut reader = wire::Reader::new(payload);
    while let Some((field, value)) = reader
        .next_field()
        .map_err(|error| ClientError::decode("Invalid recovery guide", error))?
    {
        match field {
            1 => guide.job_id = string_value(value, field, "recovery job")?,
            2 => guide.original_state = string_value(value, field, "original job state")?,
            3 => {
                guide.original_outcome_immutable = value.as_bool().map_err(|error| {
                    ClientError::decode("Invalid recovery immutability flag", error)
                })?
            }
            4 => {
                guide.automatic_replay_forbidden = value
                    .as_bool()
                    .map_err(|error| ClientError::decode("Invalid recovery replay flag", error))?
            }
            5 => guide
                .actions
                .push(string_value(value, field, "recovery action")?),
            6 => {
                guide.complete_overwrite_supported = value
                    .as_bool()
                    .map_err(|error| ClientError::decode("Invalid recovery support flag", error))?
            }
            7 => guide.contract_id = string_value(value, field, "recovery contract id")?,
            8 => guide.contract_version = string_value(value, field, "recovery contract version")?,
            9 => guide.contract_sha256 = string_value(value, field, "recovery contract digest")?,
            _ => {}
        }
    }
    if guide.job_id.is_empty() {
        return Err(ClientError::new(
            "IPC_RESPONSE_INVALID",
            "The recovery guide has no job_id.",
            10,
            false,
        ));
    }
    Ok(guide)
}

fn string_value(
    value: wire::FieldValue<'_>,
    field: u32,
    context: &str,
) -> Result<String, ClientError> {
    value
        .as_str(field)
        .map(str::to_string)
        .map_err(|error| ClientError::decode(&format!("Invalid {context}"), error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observation_decoder_preserves_agent_visible_identity_evidence() {
        let mut encoded = Vec::new();
        wire::write_string(&mut encoded, 1, "OBS-1");
        wire::write_uint64(&mut encoded, 2, 42);
        wire::write_string(&mut encoded, 3, "loader");
        wire::write_string(&mut encoded, 4, "topology");
        wire::write_string(&mut encoded, 5, "descriptor");
        wire::write_string(&mut encoded, 6, "serialAndTopology");
        wire::write_bool(&mut encoded, 7, false);
        wire::write_message(
            &mut encoded,
            8,
            &KeyValue {
                key: "usbVendorId".into(),
                value: "2207".into(),
            }
            .encode(),
        );
        wire::write_string(&mut encoded, 9, "serial-digest");
        wire::write_string(&mut encoded, 10, "descriptor");

        let decoded = decode_observation(&encoded).unwrap();
        assert_eq!(decoded.observation_id, "OBS-1");
        assert_eq!(decoded.protocol_identity[0].key, "usbVendorId");
        assert_eq!(decoded.protocol_identity[0].value, "2207");
        assert_eq!(decoded.serial_sha256, "serial-digest");
        assert_eq!(decoded.serial_evidence_kind, "descriptor");
    }

    #[test]
    fn recovery_decoder_carries_no_replay_guards_and_contract_identity() {
        let mut encoded = Vec::new();
        wire::write_string(&mut encoded, 1, "JOB-1");
        wire::write_string(&mut encoded, 2, "outcomeUnknown");
        wire::write_bool(&mut encoded, 3, true);
        wire::write_bool(&mut encoded, 4, true);
        wire::write_string(&mut encoded, 5, "do not replay");
        wire::write_bool(&mut encoded, 6, true);
        wire::write_string(&mut encoded, 7, "complete-overwrite");
        wire::write_string(&mut encoded, 8, "1");
        wire::write_string(&mut encoded, 9, "contract-digest");

        let decoded = decode_recovery_guide(&encoded).unwrap();
        assert!(decoded.original_outcome_immutable);
        assert!(decoded.automatic_replay_forbidden);
        assert_eq!(decoded.contract_id, "complete-overwrite");
        assert_eq!(decoded.contract_version, "1");
        assert_eq!(decoded.contract_sha256, "contract-digest");
    }
}
