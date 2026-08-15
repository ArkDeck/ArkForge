//! Message codec for `proto/arkforge.proto`.
//!
//! Only the AF-V1 surface is encoded here. The execute-side payloads exist in
//! the schema so the contract is complete, but this build answers those calls
//! with `STATUS_UNAVAILABLE` and never needs to decode their bodies.

use crate::wire::{self, decode_enum, Reader, WireError};
use crate::{Api, SessionKind, Status};

/// The handshake a peer opens with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub protocol_major: u32,
    pub protocol_minor: u32,
    pub session_kind: SessionKind,
}

impl Hello {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_uint32(&mut out, 1, self.protocol_major);
        wire::write_uint32(&mut out, 2, self.protocol_minor);
        wire::write_enum(&mut out, 3, self.session_kind.wire_value());
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut major = 0u32;
        let mut minor = 0u32;
        let mut session_kind = None;
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => major = value.as_u64()? as u32,
                2 => minor = value.as_u64()? as u32,
                3 => session_kind = Some(decode_enum(3, &value, SessionKind::from_wire)?),
                _ => {}
            }
        }
        Ok(Hello {
            protocol_major: major,
            protocol_minor: minor,
            // Absent means the zero value SESSION_KIND_UNSPECIFIED, which is
            // not a session this daemon serves.
            session_kind: session_kind.ok_or(WireError::MissingField {
                message: "Hello",
                field: 3,
            })?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelloAck {
    pub protocol_major: u32,
    pub protocol_minor: u32,
    pub session_kind: SessionKind,
    pub daemon_version: String,
    pub refusal: Option<String>,
}

impl HelloAck {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_uint32(&mut out, 1, self.protocol_major);
        wire::write_uint32(&mut out, 2, self.protocol_minor);
        wire::write_enum(&mut out, 3, self.session_kind.wire_value());
        wire::write_string(&mut out, 4, &self.daemon_version);
        if let Some(refusal) = &self.refusal {
            wire::write_string(&mut out, 5, refusal);
        }
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut ack = HelloAck {
            protocol_major: 0,
            protocol_minor: 0,
            session_kind: SessionKind::Public,
            daemon_version: String::new(),
            refusal: None,
        };
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => ack.protocol_major = value.as_u64()? as u32,
                2 => ack.protocol_minor = value.as_u64()? as u32,
                3 => ack.session_kind = decode_enum(3, &value, SessionKind::from_wire)?,
                4 => ack.daemon_version = value.as_str(4)?.to_string(),
                5 => ack.refusal = Some(value.as_str(5)?.to_string()),
                _ => {}
            }
        }
        Ok(ack)
    }
}

/// A request envelope. The payload stays opaque so one framing serves all APIs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub request_id: String,
    pub api: Api,
    pub payload: Vec<u8>,
}

impl Request {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_string(&mut out, 1, &self.request_id);
        wire::write_enum(&mut out, 2, self.api.wire_value());
        wire::write_bytes(&mut out, 3, &self.payload);
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut request_id = String::new();
        let mut api = None;
        let mut payload = Vec::new();
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => request_id = value.as_str(1)?.to_string(),
                2 => api = Some(decode_enum(2, &value, Api::from_wire)?),
                3 => payload = value.as_bytes()?.to_vec(),
                _ => {}
            }
        }
        Ok(Request {
            request_id,
            api: api.ok_or(WireError::MissingField {
                message: "Request",
                field: 2,
            })?,
            payload,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub request_id: String,
    pub api: Api,
    pub status: Status,
    pub payload: Vec<u8>,
    pub stream_sequence: u64,
    pub stream_end: bool,
}

impl Response {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_string(&mut out, 1, &self.request_id);
        wire::write_enum(&mut out, 2, self.api.wire_value());
        wire::write_enum(&mut out, 3, self.status.wire_value());
        wire::write_bytes(&mut out, 4, &self.payload);
        wire::write_uint64(&mut out, 5, self.stream_sequence);
        wire::write_bool(&mut out, 6, self.stream_end);
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut request_id = String::new();
        let mut api = None;
        let mut status = None;
        let mut payload = Vec::new();
        let mut stream_sequence = 0u64;
        let mut stream_end = false;
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => request_id = value.as_str(1)?.to_string(),
                2 => api = Some(decode_enum(2, &value, Api::from_wire)?),
                3 => status = Some(decode_enum(3, &value, Status::from_wire)?),
                4 => payload = value.as_bytes()?.to_vec(),
                5 => stream_sequence = value.as_u64()?,
                6 => stream_end = value.as_bool()?,
                _ => {}
            }
        }
        Ok(Response {
            request_id,
            api: api.ok_or(WireError::MissingField {
                message: "Response",
                field: 2,
            })?,
            status: status.ok_or(WireError::MissingField {
                message: "Response",
                field: 3,
            })?,
            payload,
            stream_sequence,
            stream_end,
        })
    }
}

/// A typed error body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

impl ErrorBody {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_string(&mut out, 1, &self.code);
        wire::write_string(&mut out, 2, &self.message);
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut body = ErrorBody {
            code: String::new(),
            message: String::new(),
        };
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => body.code = value.as_str(1)?.to_string(),
                2 => body.message = value.as_str(2)?.to_string(),
                _ => {}
            }
        }
        Ok(body)
    }
}

/// A `key: value` pair, used for facts, unknowns and impacts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyValue {
    pub key: String,
    pub value: String,
}

impl KeyValue {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_string(&mut out, 1, &self.key);
        wire::write_string(&mut out, 2, &self.value);
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut pair = KeyValue {
            key: String::new(),
            value: String::new(),
        };
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => pair.key = value.as_str(1)?.to_string(),
                2 => pair.value = value.as_str(2)?.to_string(),
                _ => {}
            }
        }
        Ok(pair)
    }
}

/// `InspectArtifactResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InspectArtifactResponse {
    pub format_id: String,
    pub content_sha256: String,
    pub size_bytes: u64,
    pub members: Vec<ArchiveMember>,
    pub partitions: Vec<PartitionEntry>,
    pub build_facts: Vec<KeyValue>,
    pub unclassified_members: Vec<String>,
    pub execution_relevant_unknowns: Vec<KeyValue>,
    pub confidence: String,
    pub manifest_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ArchiveMember {
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub role: String,
}

impl ArchiveMember {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_string(&mut out, 1, &self.path);
        wire::write_uint64(&mut out, 2, self.size_bytes);
        wire::write_string(&mut out, 3, &self.sha256);
        wire::write_string(&mut out, 4, &self.role);
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut member = ArchiveMember::default();
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => member.path = value.as_str(1)?.to_string(),
                2 => member.size_bytes = value.as_u64()?,
                3 => member.sha256 = value.as_str(3)?.to_string(),
                4 => member.role = value.as_str(4)?.to_string(),
                _ => {}
            }
        }
        Ok(member)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PartitionEntry {
    pub index: u32,
    pub name: String,
    pub offset_sectors: u64,
    /// `None` for a remainder partition — carried explicitly rather than as a
    /// zero, because zero sectors and "grows to the end" are different facts.
    pub size_sectors: Option<u64>,
    pub attribute: String,
    pub grammar_branch: String,
}

impl PartitionEntry {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_uint32(&mut out, 1, self.index);
        wire::write_string(&mut out, 2, &self.name);
        wire::write_uint64(&mut out, 3, self.offset_sectors);
        if let Some(size) = self.size_sectors {
            wire::write_uint64(&mut out, 4, size);
            wire::write_bool(&mut out, 5, true);
        }
        wire::write_string(&mut out, 6, &self.attribute);
        wire::write_string(&mut out, 7, &self.grammar_branch);
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut entry = PartitionEntry::default();
        let mut size = 0u64;
        let mut has_size = false;
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => entry.index = value.as_u64()? as u32,
                2 => entry.name = value.as_str(2)?.to_string(),
                3 => entry.offset_sectors = value.as_u64()?,
                4 => size = value.as_u64()?,
                5 => has_size = value.as_bool()?,
                6 => entry.attribute = value.as_str(6)?.to_string(),
                7 => entry.grammar_branch = value.as_str(7)?.to_string(),
                _ => {}
            }
        }
        entry.size_sectors = if has_size { Some(size) } else { None };
        Ok(entry)
    }
}

impl InspectArtifactResponse {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_string(&mut out, 1, &self.format_id);
        wire::write_string(&mut out, 2, &self.content_sha256);
        wire::write_uint64(&mut out, 3, self.size_bytes);
        for member in &self.members {
            wire::write_message(&mut out, 4, &member.encode());
        }
        for partition in &self.partitions {
            wire::write_message(&mut out, 5, &partition.encode());
        }
        for fact in &self.build_facts {
            wire::write_message(&mut out, 6, &fact.encode());
        }
        for member in &self.unclassified_members {
            wire::write_string(&mut out, 7, member);
        }
        for unknown in &self.execution_relevant_unknowns {
            wire::write_message(&mut out, 8, &unknown.encode());
        }
        wire::write_string(&mut out, 9, &self.confidence);
        wire::write_string(&mut out, 10, &self.manifest_sha256);
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut response = InspectArtifactResponse::default();
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => response.format_id = value.as_str(1)?.to_string(),
                2 => response.content_sha256 = value.as_str(2)?.to_string(),
                3 => response.size_bytes = value.as_u64()?,
                4 => response.members.push(ArchiveMember::decode(value.as_bytes()?)?),
                5 => response
                    .partitions
                    .push(PartitionEntry::decode(value.as_bytes()?)?),
                6 => response.build_facts.push(KeyValue::decode(value.as_bytes()?)?),
                7 => response.unclassified_members.push(value.as_str(7)?.to_string()),
                8 => response
                    .execution_relevant_unknowns
                    .push(KeyValue::decode(value.as_bytes()?)?),
                9 => response.confidence = value.as_str(9)?.to_string(),
                10 => response.manifest_sha256 = value.as_str(10)?.to_string(),
                _ => {}
            }
        }
        Ok(response)
    }
}

/// `MaterializePlanResponse`. Exactly one side is populated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaterializePlanResponse {
    Plan(ExecutablePlan),
    Assessment(Assessment),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExecutablePlan {
    pub plan_id: String,
    pub plan_sha256: String,
    pub provider_execution_plan_sha256: String,
    pub public_projection_sha256: String,
    pub public_steps: Vec<PublicStep>,
    pub persistent_effects: Vec<Effect>,
    pub transient_effects: Vec<Effect>,
    pub data_impact: Vec<KeyValue>,
    pub expires_at_epoch_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Assessment {
    pub would_be_steps: Vec<PublicStep>,
    pub known_persistent_effects: Vec<Effect>,
    pub unknowns: Vec<KeyValue>,
    pub evidence_requirements: Vec<KeyValue>,
    pub availability: String,
    pub unavailable_reason: String,
    pub data_impact: Vec<KeyValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PublicStep {
    pub step_id: String,
    pub kind: String,
    pub effect: String,
    pub cancellation: String,
    pub binding: String,
    pub semantic_target: String,
    pub content_sha256: String,
    pub expected_mode_before: String,
    pub expected_mode_after: String,
    pub private_action_sha256: String,
}

impl PublicStep {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_string(&mut out, 1, &self.step_id);
        wire::write_string(&mut out, 2, &self.kind);
        wire::write_string(&mut out, 3, &self.effect);
        wire::write_string(&mut out, 4, &self.cancellation);
        wire::write_string(&mut out, 5, &self.binding);
        wire::write_string(&mut out, 6, &self.semantic_target);
        wire::write_string(&mut out, 7, &self.content_sha256);
        wire::write_string(&mut out, 8, &self.expected_mode_before);
        wire::write_string(&mut out, 9, &self.expected_mode_after);
        wire::write_string(&mut out, 10, &self.private_action_sha256);
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut step = PublicStep::default();
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => step.step_id = value.as_str(1)?.to_string(),
                2 => step.kind = value.as_str(2)?.to_string(),
                3 => step.effect = value.as_str(3)?.to_string(),
                4 => step.cancellation = value.as_str(4)?.to_string(),
                5 => step.binding = value.as_str(5)?.to_string(),
                6 => step.semantic_target = value.as_str(6)?.to_string(),
                7 => step.content_sha256 = value.as_str(7)?.to_string(),
                8 => step.expected_mode_before = value.as_str(8)?.to_string(),
                9 => step.expected_mode_after = value.as_str(9)?.to_string(),
                10 => step.private_action_sha256 = value.as_str(10)?.to_string(),
                _ => {}
            }
        }
        Ok(step)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Effect {
    pub kind: String,
    pub target: String,
    pub range_start: u64,
    pub range_length: u64,
    pub content_sha256: String,
}

impl Effect {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_string(&mut out, 1, &self.kind);
        wire::write_string(&mut out, 2, &self.target);
        wire::write_uint64(&mut out, 3, self.range_start);
        wire::write_uint64(&mut out, 4, self.range_length);
        wire::write_string(&mut out, 5, &self.content_sha256);
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut effect = Effect::default();
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => effect.kind = value.as_str(1)?.to_string(),
                2 => effect.target = value.as_str(2)?.to_string(),
                3 => effect.range_start = value.as_u64()?,
                4 => effect.range_length = value.as_u64()?,
                5 => effect.content_sha256 = value.as_str(5)?.to_string(),
                _ => {}
            }
        }
        Ok(effect)
    }
}

impl ExecutablePlan {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_string(&mut out, 1, &self.plan_id);
        wire::write_string(&mut out, 2, &self.plan_sha256);
        wire::write_string(&mut out, 3, &self.provider_execution_plan_sha256);
        wire::write_string(&mut out, 4, &self.public_projection_sha256);
        for step in &self.public_steps {
            wire::write_message(&mut out, 5, &step.encode());
        }
        for effect in &self.persistent_effects {
            wire::write_message(&mut out, 6, &effect.encode());
        }
        for effect in &self.transient_effects {
            wire::write_message(&mut out, 7, &effect.encode());
        }
        for impact in &self.data_impact {
            wire::write_message(&mut out, 8, &impact.encode());
        }
        wire::write_uint64(&mut out, 9, self.expires_at_epoch_ms);
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut plan = ExecutablePlan::default();
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => plan.plan_id = value.as_str(1)?.to_string(),
                2 => plan.plan_sha256 = value.as_str(2)?.to_string(),
                3 => plan.provider_execution_plan_sha256 = value.as_str(3)?.to_string(),
                4 => plan.public_projection_sha256 = value.as_str(4)?.to_string(),
                5 => plan.public_steps.push(PublicStep::decode(value.as_bytes()?)?),
                6 => plan.persistent_effects.push(Effect::decode(value.as_bytes()?)?),
                7 => plan.transient_effects.push(Effect::decode(value.as_bytes()?)?),
                8 => plan.data_impact.push(KeyValue::decode(value.as_bytes()?)?),
                9 => plan.expires_at_epoch_ms = value.as_u64()?,
                _ => {}
            }
        }
        Ok(plan)
    }
}

impl Assessment {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for step in &self.would_be_steps {
            wire::write_message(&mut out, 1, &step.encode());
        }
        for effect in &self.known_persistent_effects {
            wire::write_message(&mut out, 2, &effect.encode());
        }
        for unknown in &self.unknowns {
            wire::write_message(&mut out, 3, &unknown.encode());
        }
        for requirement in &self.evidence_requirements {
            wire::write_message(&mut out, 4, &requirement.encode());
        }
        wire::write_string(&mut out, 5, &self.availability);
        wire::write_string(&mut out, 6, &self.unavailable_reason);
        for impact in &self.data_impact {
            wire::write_message(&mut out, 7, &impact.encode());
        }
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut assessment = Assessment::default();
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => assessment
                    .would_be_steps
                    .push(PublicStep::decode(value.as_bytes()?)?),
                2 => assessment
                    .known_persistent_effects
                    .push(Effect::decode(value.as_bytes()?)?),
                3 => assessment.unknowns.push(KeyValue::decode(value.as_bytes()?)?),
                4 => assessment
                    .evidence_requirements
                    .push(KeyValue::decode(value.as_bytes()?)?),
                5 => assessment.availability = value.as_str(5)?.to_string(),
                6 => assessment.unavailable_reason = value.as_str(6)?.to_string(),
                7 => assessment.data_impact.push(KeyValue::decode(value.as_bytes()?)?),
                _ => {}
            }
        }
        Ok(assessment)
    }
}

impl MaterializePlanResponse {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            MaterializePlanResponse::Plan(plan) => wire::write_message(&mut out, 1, &plan.encode()),
            MaterializePlanResponse::Assessment(assessment) => {
                wire::write_message(&mut out, 2, &assessment.encode())
            }
        }
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut plan = None;
        let mut assessment = None;
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => plan = Some(ExecutablePlan::decode(value.as_bytes()?)?),
                2 => assessment = Some(Assessment::decode(value.as_bytes()?)?),
                _ => {}
            }
        }
        match (plan, assessment) {
            (Some(plan), None) => Ok(MaterializePlanResponse::Plan(plan)),
            (None, Some(assessment)) => Ok(MaterializePlanResponse::Assessment(assessment)),
            // Both or neither is a message this schema cannot mean.
            _ => Err(WireError::MissingField {
                message: "MaterializePlanResponse",
                field: 1,
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Execution and admission (proto: API 6/7/12/13)
// ---------------------------------------------------------------------------
//
// The daemon never calls out. It asks on the `watchJob` stream and waits for
// the authority to call back in, which keeps every message client-initiated.
//
// The permit does not appear here field by field. It crosses as the exact
// canonical CBOR bytes the authority signed, because a permit re-encoded by a
// second codec is a different permit, and "the same permit" is what the
// integrity tag exists to pin down (architecture.md 8.6).

/// `WatchJobRequest`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WatchJobRequest {
    pub job_id: String,
    pub from_sequence: u64,
}

impl WatchJobRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_string(&mut out, 1, &self.job_id);
        wire::write_uint64(&mut out, 2, self.from_sequence);
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut request = WatchJobRequest::default();
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => request.job_id = value.as_str(1)?.to_string(),
                2 => request.from_sequence = value.as_u64()?,
                _ => {}
            }
        }
        Ok(request)
    }
}

/// `JobEventKind`. An unknown value is a hard error, never a default
/// (architecture.md 15.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum JobEventKind {
    StateChanged,
    StepAdmissionRequested,
    ManagedControlRequested,
    ActionReceipt,
    StepCheckpointed,
    PostflightRecorded,
    OutcomeClassified,
    PossibleEffectSet,
    RecoveryAssessment,
}

impl JobEventKind {
    pub const ALL: [JobEventKind; 9] = [
        JobEventKind::StateChanged,
        JobEventKind::StepAdmissionRequested,
        JobEventKind::ManagedControlRequested,
        JobEventKind::ActionReceipt,
        JobEventKind::StepCheckpointed,
        JobEventKind::PostflightRecorded,
        JobEventKind::OutcomeClassified,
        JobEventKind::PossibleEffectSet,
        JobEventKind::RecoveryAssessment,
    ];

    pub fn wire_value(self) -> i32 {
        match self {
            JobEventKind::StateChanged => 1,
            JobEventKind::StepAdmissionRequested => 2,
            JobEventKind::ManagedControlRequested => 3,
            JobEventKind::ActionReceipt => 4,
            JobEventKind::StepCheckpointed => 5,
            JobEventKind::PostflightRecorded => 6,
            JobEventKind::OutcomeClassified => 7,
            JobEventKind::PossibleEffectSet => 8,
            JobEventKind::RecoveryAssessment => 9,
        }
    }

    pub fn from_wire(value: i32) -> Option<Self> {
        JobEventKind::ALL
            .into_iter()
            .find(|kind| kind.wire_value() == value)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            JobEventKind::StateChanged => "stateChanged",
            JobEventKind::StepAdmissionRequested => "stepAdmissionRequested",
            JobEventKind::ManagedControlRequested => "managedControlRequested",
            JobEventKind::ActionReceipt => "actionReceipt",
            JobEventKind::StepCheckpointed => "stepCheckpointed",
            JobEventKind::PostflightRecorded => "postflightRecorded",
            JobEventKind::OutcomeClassified => "outcomeClassified",
            JobEventKind::PossibleEffectSet => "possibleEffectSet",
            JobEventKind::RecoveryAssessment => "recoveryAssessment",
        }
    }
}

/// `StepAdmissionSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StepAdmissionSnapshot {
    pub job_id: String,
    pub plan_id: String,
    pub plan_sha256: Vec<u8>,
    pub step_id: String,
    pub attempt_id: String,
    pub public_step_sha256: Vec<u8>,
    pub private_action_sha256: Vec<u8>,
    pub effect_set_sha256: Vec<u8>,
    pub admitted_device_facts_sha256: Vec<u8>,
    pub observed_mode: String,
    pub observed_at_epoch_ms: u64,
    pub snapshot_lifetime_ms: u64,
    pub request_id: String,
}

impl StepAdmissionSnapshot {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_string(&mut out, 1, &self.job_id);
        wire::write_string(&mut out, 2, &self.plan_id);
        wire::write_bytes(&mut out, 3, &self.plan_sha256);
        wire::write_string(&mut out, 4, &self.step_id);
        wire::write_string(&mut out, 5, &self.attempt_id);
        wire::write_bytes(&mut out, 6, &self.public_step_sha256);
        wire::write_bytes(&mut out, 7, &self.private_action_sha256);
        wire::write_bytes(&mut out, 8, &self.effect_set_sha256);
        wire::write_bytes(&mut out, 9, &self.admitted_device_facts_sha256);
        wire::write_string(&mut out, 10, &self.observed_mode);
        wire::write_uint64(&mut out, 11, self.observed_at_epoch_ms);
        wire::write_uint64(&mut out, 12, self.snapshot_lifetime_ms);
        wire::write_string(&mut out, 13, &self.request_id);
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut snapshot = StepAdmissionSnapshot::default();
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => snapshot.job_id = value.as_str(1)?.to_string(),
                2 => snapshot.plan_id = value.as_str(2)?.to_string(),
                3 => snapshot.plan_sha256 = value.as_bytes()?.to_vec(),
                4 => snapshot.step_id = value.as_str(4)?.to_string(),
                5 => snapshot.attempt_id = value.as_str(5)?.to_string(),
                6 => snapshot.public_step_sha256 = value.as_bytes()?.to_vec(),
                7 => snapshot.private_action_sha256 = value.as_bytes()?.to_vec(),
                8 => snapshot.effect_set_sha256 = value.as_bytes()?.to_vec(),
                9 => snapshot.admitted_device_facts_sha256 = value.as_bytes()?.to_vec(),
                10 => snapshot.observed_mode = value.as_str(10)?.to_string(),
                11 => snapshot.observed_at_epoch_ms = value.as_u64()?,
                12 => snapshot.snapshot_lifetime_ms = value.as_u64()?,
                13 => snapshot.request_id = value.as_str(13)?.to_string(),
                _ => {}
            }
        }
        Ok(snapshot)
    }

    /// Whether a permit signed against this snapshot may still be accepted.
    ///
    /// Past the lifetime the daemon takes a new snapshot rather than accepting
    /// a late permit (architecture.md 8.3): the device facts the authority
    /// checked are no longer the facts in front of it.
    pub fn is_fresh_at(&self, now_epoch_ms: u64) -> bool {
        match self.observed_at_epoch_ms.checked_add(self.snapshot_lifetime_ms) {
            Some(expiry) => now_epoch_ms < expiry,
            // An overflowing lifetime is not "forever"; it is a malformed
            // snapshot, and a malformed snapshot is never fresh.
            None => false,
        }
    }
}

/// `ManagedControlAction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ManagedControlAction {
    EnterUpdater,
    RebootToNormal,
    ReadProductFacts,
    ReadBuildFacts,
}

impl ManagedControlAction {
    pub const ALL: [ManagedControlAction; 4] = [
        ManagedControlAction::EnterUpdater,
        ManagedControlAction::RebootToNormal,
        ManagedControlAction::ReadProductFacts,
        ManagedControlAction::ReadBuildFacts,
    ];

    pub fn wire_value(self) -> i32 {
        match self {
            ManagedControlAction::EnterUpdater => 1,
            ManagedControlAction::RebootToNormal => 2,
            ManagedControlAction::ReadProductFacts => 3,
            ManagedControlAction::ReadBuildFacts => 4,
        }
    }

    pub fn from_wire(value: i32) -> Option<Self> {
        ManagedControlAction::ALL
            .into_iter()
            .find(|action| action.wire_value() == value)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ManagedControlAction::EnterUpdater => "enter-updater",
            ManagedControlAction::RebootToNormal => "reboot-to-normal",
            ManagedControlAction::ReadProductFacts => "read-product-facts",
            ManagedControlAction::ReadBuildFacts => "read-build-facts",
        }
    }
}

/// `ManagedControlRequest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedControlRequest {
    pub job_id: String,
    pub step_id: String,
    pub request_id: String,
    pub action: ManagedControlAction,
    pub permit_id: String,
    pub expected_facts: Vec<KeyValue>,
    pub deadline_epoch_ms: u64,
}

impl Default for ManagedControlRequest {
    fn default() -> Self {
        ManagedControlRequest {
            job_id: String::new(),
            step_id: String::new(),
            request_id: String::new(),
            // No `unspecified` variant exists, so a default has to name
            // something. The read-only one is the safe thing to name.
            action: ManagedControlAction::ReadBuildFacts,
            permit_id: String::new(),
            expected_facts: Vec::new(),
            deadline_epoch_ms: 0,
        }
    }
}

impl ManagedControlRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_string(&mut out, 1, &self.job_id);
        wire::write_string(&mut out, 2, &self.step_id);
        wire::write_string(&mut out, 3, &self.request_id);
        wire::write_enum(&mut out, 4, self.action.wire_value());
        wire::write_string(&mut out, 5, &self.permit_id);
        for fact in &self.expected_facts {
            wire::write_message(&mut out, 6, &fact.encode());
        }
        wire::write_uint64(&mut out, 7, self.deadline_epoch_ms);
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut request = ManagedControlRequest::default();
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => request.job_id = value.as_str(1)?.to_string(),
                2 => request.step_id = value.as_str(2)?.to_string(),
                3 => request.request_id = value.as_str(3)?.to_string(),
                4 => {
                    request.action =
                        wire::decode_enum(4, &value, ManagedControlAction::from_wire)?
                }
                5 => request.permit_id = value.as_str(5)?.to_string(),
                6 => request.expected_facts.push(KeyValue::decode(value.as_bytes()?)?),
                7 => request.deadline_epoch_ms = value.as_u64()?,
                _ => {}
            }
        }
        Ok(request)
    }
}

/// `ActionReceiptSummary`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ActionReceiptSummary {
    pub job_id: String,
    pub plan_id: String,
    pub step_id: String,
    pub action_id: String,
    pub attempt_id: String,
    pub permit_id: String,
    pub disposition: String,
    pub evidence_sha256: Vec<u8>,
    pub verification_outcome: String,
    pub verification_strength: String,
    pub verified_range_start: u64,
    pub verified_range_length: u64,
    pub typed_skip_reason: String,
    pub failure_classification: String,
    pub facts: Vec<KeyValue>,
}

impl ActionReceiptSummary {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_string(&mut out, 1, &self.job_id);
        wire::write_string(&mut out, 2, &self.plan_id);
        wire::write_string(&mut out, 3, &self.step_id);
        wire::write_string(&mut out, 4, &self.action_id);
        wire::write_string(&mut out, 5, &self.attempt_id);
        wire::write_string(&mut out, 6, &self.permit_id);
        wire::write_string(&mut out, 7, &self.disposition);
        wire::write_bytes(&mut out, 8, &self.evidence_sha256);
        wire::write_string(&mut out, 9, &self.verification_outcome);
        wire::write_string(&mut out, 10, &self.verification_strength);
        wire::write_uint64(&mut out, 11, self.verified_range_start);
        wire::write_uint64(&mut out, 12, self.verified_range_length);
        wire::write_string(&mut out, 13, &self.typed_skip_reason);
        wire::write_string(&mut out, 14, &self.failure_classification);
        for fact in &self.facts {
            wire::write_message(&mut out, 15, &fact.encode());
        }
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut receipt = ActionReceiptSummary::default();
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => receipt.job_id = value.as_str(1)?.to_string(),
                2 => receipt.plan_id = value.as_str(2)?.to_string(),
                3 => receipt.step_id = value.as_str(3)?.to_string(),
                4 => receipt.action_id = value.as_str(4)?.to_string(),
                5 => receipt.attempt_id = value.as_str(5)?.to_string(),
                6 => receipt.permit_id = value.as_str(6)?.to_string(),
                7 => receipt.disposition = value.as_str(7)?.to_string(),
                8 => receipt.evidence_sha256 = value.as_bytes()?.to_vec(),
                9 => receipt.verification_outcome = value.as_str(9)?.to_string(),
                10 => receipt.verification_strength = value.as_str(10)?.to_string(),
                11 => receipt.verified_range_start = value.as_u64()?,
                12 => receipt.verified_range_length = value.as_u64()?,
                13 => receipt.typed_skip_reason = value.as_str(13)?.to_string(),
                14 => receipt.failure_classification = value.as_str(14)?.to_string(),
                15 => receipt.facts.push(KeyValue::decode(value.as_bytes()?)?),
                _ => {}
            }
        }
        Ok(receipt)
    }

    /// A typed skip is never any grade of verified (architecture.md 16.4).
    ///
    /// Checked on the wire as well as in the domain type, because this is the
    /// field a second implementation is most likely to fill in "helpfully".
    pub fn strength_is_consistent(&self) -> bool {
        if self.verification_strength.is_empty() {
            return true;
        }
        self.verification_outcome == "verified"
    }
}

/// `JobEvent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobEvent {
    pub job_id: String,
    pub sequence: u64,
    pub kind: JobEventKind,
    pub at_epoch_ms: u64,
    pub journal_record_sha256: Vec<u8>,
    pub job_state: String,
    pub admission: Option<StepAdmissionSnapshot>,
    pub control_request: Option<ManagedControlRequest>,
    pub receipt: Option<ActionReceiptSummary>,
    pub facts: Vec<KeyValue>,
}

impl Default for JobEvent {
    fn default() -> Self {
        JobEvent {
            job_id: String::new(),
            sequence: 0,
            kind: JobEventKind::StateChanged,
            at_epoch_ms: 0,
            journal_record_sha256: Vec::new(),
            job_state: String::new(),
            admission: None,
            control_request: None,
            receipt: None,
            facts: Vec::new(),
        }
    }
}

impl JobEvent {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_string(&mut out, 1, &self.job_id);
        wire::write_uint64(&mut out, 2, self.sequence);
        wire::write_enum(&mut out, 3, self.kind.wire_value());
        wire::write_uint64(&mut out, 4, self.at_epoch_ms);
        wire::write_bytes(&mut out, 5, &self.journal_record_sha256);
        wire::write_string(&mut out, 6, &self.job_state);
        if let Some(admission) = &self.admission {
            wire::write_message(&mut out, 7, &admission.encode());
        }
        if let Some(request) = &self.control_request {
            wire::write_message(&mut out, 8, &request.encode());
        }
        if let Some(receipt) = &self.receipt {
            wire::write_message(&mut out, 9, &receipt.encode());
        }
        for fact in &self.facts {
            wire::write_message(&mut out, 10, &fact.encode());
        }
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut event = JobEvent::default();
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => event.job_id = value.as_str(1)?.to_string(),
                2 => event.sequence = value.as_u64()?,
                3 => event.kind = wire::decode_enum(3, &value, JobEventKind::from_wire)?,
                4 => event.at_epoch_ms = value.as_u64()?,
                5 => event.journal_record_sha256 = value.as_bytes()?.to_vec(),
                6 => event.job_state = value.as_str(6)?.to_string(),
                7 => {
                    event.admission = Some(StepAdmissionSnapshot::decode(value.as_bytes()?)?)
                }
                8 => {
                    event.control_request =
                        Some(ManagedControlRequest::decode(value.as_bytes()?)?)
                }
                9 => event.receipt = Some(ActionReceiptSummary::decode(value.as_bytes()?)?),
                10 => event.facts.push(KeyValue::decode(value.as_bytes()?)?),
                _ => {}
            }
        }
        Ok(event)
    }
}

/// `SubmitStepPermitRequest`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SubmitStepPermitRequest {
    pub job_id: String,
    pub request_id: String,
    /// The permit as the authority signed it. Never re-encoded here.
    pub permit_cbor: Vec<u8>,
    pub integrity_tag: Vec<u8>,
    pub pairing_epoch: u64,
    /// Set instead of the permit when the authority declines. A refusal is an
    /// answer; silence is not, and the daemon distinguishes them.
    pub refusal: String,
}

impl SubmitStepPermitRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_string(&mut out, 1, &self.job_id);
        wire::write_string(&mut out, 2, &self.request_id);
        wire::write_bytes(&mut out, 3, &self.permit_cbor);
        wire::write_bytes(&mut out, 4, &self.integrity_tag);
        wire::write_uint64(&mut out, 5, self.pairing_epoch);
        wire::write_string(&mut out, 6, &self.refusal);
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut request = SubmitStepPermitRequest::default();
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => request.job_id = value.as_str(1)?.to_string(),
                2 => request.request_id = value.as_str(2)?.to_string(),
                3 => request.permit_cbor = value.as_bytes()?.to_vec(),
                4 => request.integrity_tag = value.as_bytes()?.to_vec(),
                5 => request.pairing_epoch = value.as_u64()?,
                6 => request.refusal = value.as_str(6)?.to_string(),
                _ => {}
            }
        }
        Ok(request)
    }

    /// A submission carries a permit or a refusal, never both and never
    /// neither. Both would leave the daemon choosing which one the authority
    /// meant; neither is silence dressed up as an answer.
    pub fn is_well_formed(&self) -> bool {
        self.permit_cbor.is_empty() != self.refusal.is_empty()
    }
}

/// `SubmitStepPermitResponse` and `SubmitManagedControlReceiptResponse` share a
/// shape: what the daemon did with the submission, and why not.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SubmissionOutcome {
    pub accepted: bool,
    pub rejection_code: String,
    pub rejection_message: String,
}

impl SubmissionOutcome {
    pub fn accepted() -> Self {
        SubmissionOutcome {
            accepted: true,
            ..SubmissionOutcome::default()
        }
    }

    pub fn rejected(code: impl Into<String>, message: impl Into<String>) -> Self {
        SubmissionOutcome {
            accepted: false,
            rejection_code: code.into(),
            rejection_message: message.into(),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_bool(&mut out, 1, self.accepted);
        wire::write_string(&mut out, 2, &self.rejection_code);
        wire::write_string(&mut out, 3, &self.rejection_message);
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut outcome = SubmissionOutcome::default();
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => outcome.accepted = value.as_bool()?,
                2 => outcome.rejection_code = value.as_str(2)?.to_string(),
                3 => outcome.rejection_message = value.as_str(3)?.to_string(),
                _ => {}
            }
        }
        Ok(outcome)
    }
}

/// `SubmitManagedControlReceiptRequest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitManagedControlReceiptRequest {
    pub job_id: String,
    pub request_id: String,
    pub action: ManagedControlAction,
    /// False means the authority's own semantic success was not observed. It
    /// does NOT mean nothing happened: a mode change may have taken effect and
    /// gone unobserved, which the daemon records as an unknown outcome.
    pub accepted: bool,
    pub facts: Vec<KeyValue>,
    pub evidence_sha256: Vec<u8>,
    pub failure_reason: String,
}

impl Default for SubmitManagedControlReceiptRequest {
    fn default() -> Self {
        SubmitManagedControlReceiptRequest {
            job_id: String::new(),
            request_id: String::new(),
            action: ManagedControlAction::ReadBuildFacts,
            accepted: false,
            facts: Vec::new(),
            evidence_sha256: Vec::new(),
            failure_reason: String::new(),
        }
    }
}

/// Fact keys a control receipt may never carry (architecture.md 9.2).
///
/// The port exists so ArkForge never learns these. A receipt that carried one
/// would be the leak the port was built to prevent, so the daemon refuses the
/// whole receipt rather than dropping the field and carrying on.
pub const FORBIDDEN_CONTROL_RECEIPT_FACTS: [&str; 6] = [
    "connectKey",
    "hdcExecutablePath",
    "hdcEndpoint",
    "argv",
    "shell",
    "serverLifecycleAction",
];

impl SubmitManagedControlReceiptRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_string(&mut out, 1, &self.job_id);
        wire::write_string(&mut out, 2, &self.request_id);
        wire::write_enum(&mut out, 3, self.action.wire_value());
        wire::write_bool(&mut out, 4, self.accepted);
        for fact in &self.facts {
            wire::write_message(&mut out, 5, &fact.encode());
        }
        wire::write_bytes(&mut out, 6, &self.evidence_sha256);
        wire::write_string(&mut out, 7, &self.failure_reason);
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut request = SubmitManagedControlReceiptRequest::default();
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => request.job_id = value.as_str(1)?.to_string(),
                2 => request.request_id = value.as_str(2)?.to_string(),
                3 => {
                    request.action =
                        wire::decode_enum(3, &value, ManagedControlAction::from_wire)?
                }
                4 => request.accepted = value.as_bool()?,
                5 => request.facts.push(KeyValue::decode(value.as_bytes()?)?),
                6 => request.evidence_sha256 = value.as_bytes()?.to_vec(),
                7 => request.failure_reason = value.as_str(7)?.to_string(),
                _ => {}
            }
        }
        Ok(request)
    }

    /// Fact keys this receipt carries that the port forbids.
    pub fn forbidden_facts(&self) -> Vec<&str> {
        self.facts
            .iter()
            .map(|fact| fact.key.as_str())
            .filter(|key| FORBIDDEN_CONTROL_RECEIPT_FACTS.contains(key))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_round_trips() {
        let hello = Hello {
            protocol_major: 1,
            protocol_minor: 0,
            session_kind: SessionKind::Controller,
        };
        assert_eq!(Hello::decode(&hello.encode()).unwrap(), hello);
    }

    #[test]
    fn a_hello_with_no_session_kind_is_refused() {
        // SESSION_KIND_UNSPECIFIED is the proto3 zero, so it is absent on the
        // wire. Reading it as "public" would hand an unidentified peer the
        // read-only surface.
        let empty: Vec<u8> = Vec::new();
        assert!(matches!(
            Hello::decode(&empty),
            Err(WireError::MissingField {
                message: "Hello",
                field: 3
            })
        ));
    }

    #[test]
    fn a_hello_with_an_unknown_session_kind_is_refused() {
        let mut out = Vec::new();
        wire::write_uint32(&mut out, 1, 1);
        wire::write_enum(&mut out, 3, 42);
        assert!(matches!(
            Hello::decode(&out),
            Err(WireError::UnknownEnumValue { field: 3, .. })
        ));
    }

    #[test]
    fn request_and_response_round_trip() {
        let request = Request {
            request_id: "REQ-1".into(),
            api: Api::MaterializePlan,
            payload: vec![1, 2, 3],
        };
        assert_eq!(Request::decode(&request.encode()).unwrap(), request);

        let response = Response {
            request_id: "REQ-1".into(),
            api: Api::StartExecution,
            status: Status::Unavailable,
            payload: vec![9],
            stream_sequence: 3,
            stream_end: true,
        };
        assert_eq!(Response::decode(&response.encode()).unwrap(), response);
    }

    #[test]
    fn a_response_with_an_unknown_status_is_refused() {
        let mut out = Vec::new();
        wire::write_string(&mut out, 1, "REQ-1");
        wire::write_enum(&mut out, 2, Api::InspectArtifact.wire_value());
        wire::write_enum(&mut out, 3, 77);
        assert!(matches!(
            Response::decode(&out),
            Err(WireError::UnknownEnumValue { field: 3, .. })
        ));
    }

    #[test]
    fn inspect_response_round_trips_with_a_remainder_partition() {
        let response = InspectArtifactResponse {
            format_id: "example-images-targz".into(),
            content_sha256: "a".repeat(64),
            size_bytes: 730_769_584,
            members: vec![ArchiveMember {
                path: "system.img".into(),
                size_bytes: 2_147_483_648,
                sha256: "b".repeat(64),
                role: "imageCandidate".into(),
            }],
            partitions: vec![
                PartitionEntry {
                    index: 0,
                    name: "uboot".into(),
                    offset_sectors: 8192,
                    size_sectors: Some(8192),
                    attribute: String::new(),
                    grammar_branch: "fixed".into(),
                },
                PartitionEntry {
                    index: 14,
                    name: "userdata".into(),
                    offset_sectors: 19_955_712,
                    size_sectors: None,
                    attribute: "grow".into(),
                    grammar_branch: "remainderGrow".into(),
                },
            ],
            build_facts: vec![KeyValue {
                key: "const.ohos.fullname".into(),
                value: "example-build-1.0.0".into(),
            }],
            unclassified_members: vec!["updater_binary".into()],
            execution_relevant_unknowns: vec![],
            confidence: "researchOnly".into(),
            manifest_sha256: "c".repeat(64),
        };
        let decoded = InspectArtifactResponse::decode(&response.encode()).unwrap();
        assert_eq!(decoded, response);
        // The remainder stays a remainder rather than becoming zero sectors.
        assert_eq!(decoded.partitions[1].size_sectors, None);
    }

    #[test]
    fn materialize_response_round_trips_both_arms() {
        let plan = MaterializePlanResponse::Plan(ExecutablePlan {
            plan_id: "PLAN-1".into(),
            plan_sha256: "d".repeat(64),
            public_steps: vec![PublicStep {
                step_id: "STEP-001".into(),
                kind: "writeTarget".into(),
                ..PublicStep::default()
            }],
            ..ExecutablePlan::default()
        });
        assert_eq!(MaterializePlanResponse::decode(&plan.encode()).unwrap(), plan);

        let assessment = MaterializePlanResponse::Assessment(Assessment {
            availability: "unavailable".into(),
            unavailable_reason: "hardware gated".into(),
            unknowns: vec![KeyValue {
                key: "RK-M02".into(),
                value: "AF-V2 required".into(),
            }],
            ..Assessment::default()
        });
        assert_eq!(
            MaterializePlanResponse::decode(&assessment.encode()).unwrap(),
            assessment
        );
    }

    #[test]
    fn a_materialize_response_that_is_both_or_neither_is_refused() {
        assert!(MaterializePlanResponse::decode(&[]).is_err());
        let mut both = Vec::new();
        wire::write_message(&mut both, 1, &ExecutablePlan::default().encode());
        wire::write_message(&mut both, 2, &Assessment::default().encode());
        assert!(MaterializePlanResponse::decode(&both).is_err());
    }

    #[test]
    fn a_newer_peers_extra_fields_do_not_break_decoding() {
        let mut out = Request {
            request_id: "REQ-1".into(),
            api: Api::InspectArtifact,
            payload: vec![],
        }
        .encode();
        wire::write_string(&mut out, 500, "a field from a future version");
        wire::write_uint64(&mut out, 501, 12345);
        let decoded = Request::decode(&out).unwrap();
        assert_eq!(decoded.api, Api::InspectArtifact);
        assert_eq!(decoded.request_id, "REQ-1");
    }
    // -----------------------------------------------------------------------
    // Execution and admission
    // -----------------------------------------------------------------------

    fn key_value(key: &str, value: &str) -> KeyValue {
        KeyValue {
            key: key.into(),
            value: value.into(),
        }
    }

    fn snapshot() -> StepAdmissionSnapshot {
        StepAdmissionSnapshot {
            job_id: "JOB-1".into(),
            plan_id: "PLAN-1".into(),
            plan_sha256: vec![0x11; 32],
            step_id: "STEP-WRITE-SYSTEM".into(),
            attempt_id: "ATTEMPT-1".into(),
            public_step_sha256: vec![0x22; 32],
            private_action_sha256: vec![0x33; 32],
            effect_set_sha256: vec![0x44; 32],
            admitted_device_facts_sha256: vec![0x55; 32],
            observed_mode: "updater".into(),
            observed_at_epoch_ms: 1_770_000_000_000,
            snapshot_lifetime_ms: 60_000,
            request_id: "REQ-1".into(),
        }
    }

    #[test]
    fn the_admission_snapshot_round_trips() {
        let encoded = snapshot().encode();
        assert_eq!(StepAdmissionSnapshot::decode(&encoded).unwrap(), snapshot());
    }

    /// architecture.md 8.3: past the lifetime the daemon takes a new snapshot
    /// rather than accepting a late permit, because the facts the authority
    /// checked are no longer the facts in front of the device.
    #[test]
    fn a_snapshot_stops_being_fresh_at_its_lifetime() {
        let snapshot = snapshot();
        assert!(snapshot.is_fresh_at(1_770_000_000_000));
        assert!(snapshot.is_fresh_at(1_770_000_059_999));
        assert!(!snapshot.is_fresh_at(1_770_000_060_000));
        assert!(!snapshot.is_fresh_at(1_770_000_060_001));
    }

    #[test]
    fn a_snapshot_whose_lifetime_overflows_is_never_fresh() {
        // "Forever" is not a lifetime a malformed snapshot gets to claim.
        let forever = StepAdmissionSnapshot {
            observed_at_epoch_ms: u64::MAX,
            snapshot_lifetime_ms: 1,
            ..snapshot()
        };
        assert!(!forever.is_fresh_at(0));
        assert!(!forever.is_fresh_at(u64::MAX));
    }

    #[test]
    fn a_job_event_round_trips_with_each_of_its_payloads() {
        let base = JobEvent {
            job_id: "JOB-1".into(),
            sequence: 7,
            kind: JobEventKind::StateChanged,
            at_epoch_ms: 1_770_000_000_000,
            journal_record_sha256: vec![0xAB; 32],
            job_state: "awaitingPermit".into(),
            facts: vec![key_value("note", "state")],
            ..JobEvent::default()
        };
        for event in [
            base.clone(),
            JobEvent {
                kind: JobEventKind::StepAdmissionRequested,
                admission: Some(snapshot()),
                ..base.clone()
            },
            JobEvent {
                kind: JobEventKind::ManagedControlRequested,
                control_request: Some(ManagedControlRequest {
                    job_id: "JOB-1".into(),
                    step_id: "STEP-ENSURE-MODE".into(),
                    request_id: "REQ-2".into(),
                    action: ManagedControlAction::EnterUpdater,
                    permit_id: "PERMIT-1".into(),
                    expected_facts: vec![key_value("example.build.fullname", "example-build-1.0.0")],
                    deadline_epoch_ms: 1_770_000_120_000,
                }),
                ..base.clone()
            },
            JobEvent {
                kind: JobEventKind::ActionReceipt,
                receipt: Some(ActionReceiptSummary {
                    job_id: "JOB-1".into(),
                    plan_id: "PLAN-1".into(),
                    step_id: "STEP-VERIFY".into(),
                    action_id: "ACT-013".into(),
                    attempt_id: "ATTEMPT-1".into(),
                    permit_id: "PERMIT-1".into(),
                    disposition: "semanticSuccess".into(),
                    evidence_sha256: vec![0xCD; 32],
                    verification_outcome: "typedSkip".into(),
                    verification_strength: String::new(),
                    verified_range_start: 0,
                    verified_range_length: 0,
                    typed_skip_reason: "skipped-lba-read-window".into(),
                    failure_classification: String::new(),
                    facts: vec![key_value("partition", "example-target")],
                }),
                ..base
            },
        ] {
            let encoded = event.encode();
            assert_eq!(JobEvent::decode(&encoded).unwrap(), event, "{:?}", event.kind);
        }
    }

    #[test]
    fn every_event_kind_and_control_action_round_trips_through_its_wire_value() {
        for kind in JobEventKind::ALL {
            assert_eq!(JobEventKind::from_wire(kind.wire_value()), Some(kind));
            assert!(!kind.as_str().is_empty());
        }
        for action in ManagedControlAction::ALL {
            assert_eq!(ManagedControlAction::from_wire(action.wire_value()), Some(action));
        }
        // An unknown enum value is a hard error, never a default
        // (architecture.md 15.2).
        assert_eq!(JobEventKind::from_wire(0), None);
        assert_eq!(JobEventKind::from_wire(99), None);
        assert_eq!(ManagedControlAction::from_wire(0), None);
    }

    /// The control action's wire vocabulary must equal the one the typed port
    /// already publishes, or the adapter's mapping table names actions this
    /// wire cannot carry.
    #[test]
    fn the_wire_control_actions_match_the_typed_ports_vocabulary() {
        let wire: Vec<&str> = ManagedControlAction::ALL
            .into_iter()
            .map(|action| action.as_str())
            .collect();
        assert_eq!(
            wire,
            vec![
                "enter-updater",
                "reboot-to-normal",
                "read-product-facts",
                "read-build-facts",
            ]
        );
    }

    #[test]
    fn a_permit_submission_carries_a_permit_or_a_refusal_but_not_both() {
        let permit = SubmitStepPermitRequest {
            job_id: "JOB-1".into(),
            request_id: "REQ-1".into(),
            permit_cbor: vec![0xA1, 0x01],
            integrity_tag: vec![0x0F; 32],
            pairing_epoch: 3,
            refusal: String::new(),
        };
        assert!(permit.is_well_formed());
        assert_eq!(
            SubmitStepPermitRequest::decode(&permit.encode()).unwrap(),
            permit
        );

        let refusal = SubmitStepPermitRequest {
            permit_cbor: Vec::new(),
            integrity_tag: Vec::new(),
            refusal: "the operator declined the destructive confirmation".into(),
            ..permit.clone()
        };
        assert!(refusal.is_well_formed());

        // Both would leave the daemon choosing which one the authority meant.
        assert!(!SubmitStepPermitRequest {
            refusal: "declined".into(),
            ..permit.clone()
        }
        .is_well_formed());
        // Neither is silence dressed up as an answer.
        assert!(!SubmitStepPermitRequest {
            permit_cbor: Vec::new(),
            ..permit
        }
        .is_well_formed());
    }

    /// architecture.md 9.2, on the wire. The daemon refuses the whole receipt
    /// rather than dropping the field and carrying on, because a receipt that
    /// carried one of these is the leak the port exists to prevent.
    #[test]
    fn a_control_receipt_naming_a_forbidden_fact_is_detected() {
        let clean = SubmitManagedControlReceiptRequest {
            job_id: "JOB-1".into(),
            request_id: "REQ-2".into(),
            action: ManagedControlAction::EnterUpdater,
            accepted: true,
            facts: vec![key_value("mode", "updater")],
            evidence_sha256: vec![0x01; 32],
            failure_reason: String::new(),
        };
        assert!(clean.forbidden_facts().is_empty());
        assert_eq!(
            SubmitManagedControlReceiptRequest::decode(&clean.encode()).unwrap(),
            clean
        );

        for forbidden in FORBIDDEN_CONTROL_RECEIPT_FACTS {
            let leaky = SubmitManagedControlReceiptRequest {
                facts: vec![key_value("mode", "updater"), key_value(forbidden, "x")],
                ..clean.clone()
            };
            assert_eq!(leaky.forbidden_facts(), vec![forbidden]);
        }
    }

    /// A typed skip is never any grade of verified. This is the field a second
    /// implementation is most likely to fill in helpfully.
    #[test]
    fn a_receipt_may_not_carry_a_strength_unless_it_verified() {
        let skipped = ActionReceiptSummary {
            verification_outcome: "typedSkip".into(),
            verification_strength: "fullHash".into(),
            ..ActionReceiptSummary::default()
        };
        assert!(!skipped.strength_is_consistent());

        let verified = ActionReceiptSummary {
            verification_outcome: "verified".into(),
            verification_strength: "prefixHash".into(),
            ..ActionReceiptSummary::default()
        };
        assert!(verified.strength_is_consistent());

        let no_verification = ActionReceiptSummary::default();
        assert!(no_verification.strength_is_consistent());
    }

    #[test]
    fn a_submission_outcome_round_trips_and_a_rejection_always_says_why() {
        let accepted = SubmissionOutcome::accepted();
        assert_eq!(SubmissionOutcome::decode(&accepted.encode()).unwrap(), accepted);

        let rejected = SubmissionOutcome::rejected("STALE_SNAPSHOT", "the snapshot expired");
        assert!(!rejected.accepted);
        assert!(!rejected.rejection_code.is_empty());
        assert!(!rejected.rejection_message.is_empty());
        assert_eq!(SubmissionOutcome::decode(&rejected.encode()).unwrap(), rejected);
    }

    #[test]
    fn a_watch_request_round_trips_with_a_resume_cursor() {
        let request = WatchJobRequest {
            job_id: "JOB-1".into(),
            from_sequence: 42,
        };
        assert_eq!(WatchJobRequest::decode(&request.encode()).unwrap(), request);
    }
}
