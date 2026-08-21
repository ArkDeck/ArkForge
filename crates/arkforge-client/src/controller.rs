//! Controller-socket client used only by the persistent authority supervisor.
//!
//! Short-lived `arkforge` commands never construct this type. Keeping it in a
//! separate module makes the capability direction reviewable: public commands
//! use `PublicClient`; only the supervisor can materialize, start, cancel,
//! reconcile or answer admissions.

use crate::ClientError;
use arkforge_ipc::framing::{read_frame, write_frame};
use arkforge_ipc::messages::{
    ErrorBody, Hello, HelloAck, JobEvent, MaterializePlanResponse, Request, Response,
    SubmissionOutcome, SubmitManagedControlReceiptRequest, SubmitStepPermitRequest,
    WatchJobRequest,
};
use arkforge_ipc::{Api, PROTOCOL_MAJOR, PROTOCOL_MINOR, SessionKind, Status, wire};
use arkforge_platform::{LocalChannel, LocalEndpoint, LocalStream};
use std::path::Path;

#[derive(Debug)]
pub struct ControllerClient {
    stream: LocalStream,
    next_request: u64,
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

impl ControllerClient {
    pub fn connect(runtime_dir: &Path) -> Result<Self, ClientError> {
        let endpoint = LocalEndpoint::for_runtime(runtime_dir, LocalChannel::Controller);
        let mut stream = LocalStream::connect(&endpoint).map_err(|error| {
            ClientError::new(
                "CONTROLLER_UNAVAILABLE",
                format!("Cannot connect to {}: {error}", endpoint.display()),
                5,
                true,
            )
        })?;
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
        Ok(Self {
            stream,
            next_request: 1,
        })
    }

    pub fn materialize_plan(
        &mut self,
        input: &MaterializeInput<'_>,
    ) -> Result<MaterializePlanResponse, ClientError> {
        let mut payload = Vec::new();
        wire::write_string(&mut payload, 1, input.artifact_id);
        wire::write_string(&mut payload, 2, input.profile_id);
        wire::write_string(&mut payload, 3, input.device_id);
        wire::write_string(&mut payload, 4, "fullRestore");
        wire::write_string(&mut payload, 5, input.toolchain_id);
        wire::write_string(&mut payload, 6, input.authority_namespace);
        wire::write_string(&mut payload, 7, input.binding_id);
        wire::write_uint64(&mut payload, 8, input.binding_revision);
        wire::write_bytes(&mut payload, 9, input.stable_identity_sha256);
        wire::write_string(&mut payload, 10, input.execution_purpose);
        wire::write_bytes(&mut payload, 11, input.authority_support_key_sha256);
        wire::write_string(&mut payload, 12, input.authority_support_state);
        wire::write_string(&mut payload, 13, input.authority_support_detail);
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
        let request = Request {
            request_id: format!("CLI-CONTROLLER-{}", self.next_request),
            api,
            payload,
        };
        self.next_request += 1;
        write_frame(&mut self.stream, &request.encode())
            .map_err(|error| transport("send a controller request", error))?;
        let frame = read_frame(&mut self.stream)
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
            return Ok(response.payload);
        }
        let error = ErrorBody::decode(&response.payload)
            .map_err(|decode| invalid_response("decode controller refusal", decode))?;
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
