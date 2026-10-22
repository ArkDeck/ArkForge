use crate::wire::{self, Reader, WireError, decode_enum};
use crate::{Api, SessionKind, Status};
use arkforge_core::digest::{
    CanonicalCbor, CborError, CborValue, Domain, Sha256Digest, decode_canonical, digest_canonical,
};
use arkforge_core::ids::OpaqueId;
use std::collections::BTreeMap;

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
    /// Whether this daemon could execute any plan at all. Standing facts,
    /// established at startup and unchangeable by a request, which is why they
    /// belong on the handshake rather than behind a call.
    pub execution_ready: bool,
    /// Stable codes for what is missing. Empty exactly when ready.
    pub execution_blockers: Vec<String>,
    /// The bound tool's identity; empty when none is bound.
    ///
    /// Byte equality is not a promise the tool runs: a quarantined executable
    /// with the right digest hangs in dyld (AD-015), and nothing here shows it.
    pub toolchain_id: String,
    pub toolchain_sha256: String,
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
        wire::write_bool(&mut out, 6, self.execution_ready);
        for blocker in &self.execution_blockers {
            wire::write_string(&mut out, 7, blocker);
        }
        wire::write_string(&mut out, 8, &self.toolchain_id);
        wire::write_string(&mut out, 9, &self.toolchain_sha256);
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut ack = HelloAck {
            protocol_major: 0,
            protocol_minor: 0,
            session_kind: SessionKind::Public,
            daemon_version: String::new(),
            refusal: None,
            execution_ready: false,
            execution_blockers: Vec::new(),
            toolchain_id: String::new(),
            toolchain_sha256: String::new(),
        };
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => ack.protocol_major = value.as_u64()? as u32,
                2 => ack.protocol_minor = value.as_u64()? as u32,
                3 => ack.session_kind = decode_enum(3, &value, SessionKind::from_wire)?,
                4 => ack.daemon_version = value.as_str(4)?.to_string(),
                5 => ack.refusal = Some(value.as_str(5)?.to_string()),
                6 => ack.execution_ready = value.as_bool()?,
                7 => ack.execution_blockers.push(value.as_str(7)?.to_string()),
                8 => ack.toolchain_id = value.as_str(8)?.to_string(),
                9 => ack.toolchain_sha256 = value.as_str(9)?.to_string(),
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

/// The existing controller-only import header. Content follows in frames,
/// terminated by an empty frame; a caller's path is never sent to the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportArtifactRequest {
    pub expected_size_bytes: u64,
    pub expected_sha256: String,
}

impl ImportArtifactRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_uint64(&mut out, 1, self.expected_size_bytes);
        wire::write_string(&mut out, 2, &self.expected_sha256);
        out
    }
}

/// The content-addressed object returned by `importArtifact`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportArtifactResponse {
    pub artifact_id: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub deduplicated: bool,
}

impl ImportArtifactResponse {
    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut response = Self::default();
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => response.artifact_id = value.as_str(1)?.to_string(),
                2 => response.sha256 = value.as_str(2)?.to_string(),
                3 => response.size_bytes = value.as_u64()?,
                4 => response.deduplicated = value.as_bool()?,
                _ => {}
            }
        }
        Ok(response)
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
