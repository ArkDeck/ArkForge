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
                value: "OpenHarmony-7.0.0.36".into(),
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
}
