//! Transcripts — the recorded evidence a contract test replays.
//!
//! architecture.md 11.4. A transcript records *types, lengths, hashes, parsed
//! semantic fields, status and timing*. It has no field for a payload, so a
//! full firmware image cannot end up in one by accident; captured payloads
//! belong in the separately authorized encrypted research store.
//!
//! Every transcript declares its `provenance`. A transcript derived from
//! published receipts is a faithful record of what a campaign *reported*; it is
//! not a byte-level capture, and calling it one would put a hardware claim
//! behind a fixture (architecture.md 24.1).

use crate::{
    DeviceObservation, IdentityEvidenceStrength, ProtocolIdentityFact, ProviderCandidateRef,
    SerialEvidence,
};
use arkforge_core::digest::{
    CanonicalCbor, CborError, CborValue, Domain, Sha256Digest, digest_canonical,
};
use arkforge_core::effect::DeviceMode;
use arkforge_core::ids::{ObservationId, OpaqueId};
use arkforge_core::yaml::{self, YamlValue};
use core::fmt;

pub const SCHEMA_VERSION: &str = "arkforge.transcript/v1";

/// Where a transcript's contents came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TranscriptProvenance {
    /// Recorded live from the wire and the tool, hash by hash.
    Captured,
    /// Reconstructed from a campaign's published receipts. Faithful to what was
    /// reported; not a byte-level capture, and never evidence that a protocol
    /// behaves a particular way.
    DerivedFromPublishedReceipts,
    /// Hand-written for a test. Never evidence of anything about hardware.
    Synthetic,
}

impl TranscriptProvenance {
    pub fn as_str(self) -> &'static str {
        match self {
            TranscriptProvenance::Captured => "captured",
            TranscriptProvenance::DerivedFromPublishedReceipts => "derived-from-published-receipts",
            TranscriptProvenance::Synthetic => "synthetic",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "captured" => Some(TranscriptProvenance::Captured),
            "derived-from-published-receipts" => {
                Some(TranscriptProvenance::DerivedFromPublishedReceipts)
            }
            "synthetic" => Some(TranscriptProvenance::Synthetic),
            _ => None,
        }
    }

    /// Only a live capture can support a claim about how hardware behaves.
    pub fn supports_protocol_claims(self) -> bool {
        matches!(self, TranscriptProvenance::Captured)
    }
}

impl CanonicalCbor for TranscriptProvenance {
    fn to_cbor(&self) -> CborValue {
        CborValue::text(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RecordKind {
    Observation,
    Attach,
    Detach,
    Rebind,
    Invocation,
}

impl RecordKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RecordKind::Observation => "observation",
            RecordKind::Attach => "attach",
            RecordKind::Detach => "detach",
            RecordKind::Rebind => "rebind",
            RecordKind::Invocation => "invocation",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "observation" => Some(RecordKind::Observation),
            "attach" => Some(RecordKind::Attach),
            "detach" => Some(RecordKind::Detach),
            "rebind" => Some(RecordKind::Rebind),
            "invocation" => Some(RecordKind::Invocation),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RecordStatus {
    Ok,
    Failed,
    Timeout,
    Disconnected,
}

impl RecordStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RecordStatus::Ok => "ok",
            RecordStatus::Failed => "failed",
            RecordStatus::Timeout => "timeout",
            RecordStatus::Disconnected => "disconnected",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ok" => Some(RecordStatus::Ok),
            "failed" => Some(RecordStatus::Failed),
            "timeout" => Some(RecordStatus::Timeout),
            "disconnected" => Some(RecordStatus::Disconnected),
            _ => None,
        }
    }
}

/// One recorded event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptRecord {
    pub sequence: u32,
    pub kind: RecordKind,
    pub at_epoch_ms: u64,
    pub duration_ms: u64,
    pub status: RecordStatus,
    /// Present for observation/attach/rebind records.
    pub observation: Option<DeviceObservation>,
    /// Present for invocation records: the semantic action, never an argv.
    pub action: Option<OpaqueId>,
    pub request_bytes: Option<u64>,
    pub request_digest: Option<Sha256Digest>,
    pub response_bytes: Option<u64>,
    pub response_digest: Option<Sha256Digest>,
    /// Parsed semantic fields — the marker a tool printed, the sector it read,
    /// the mode it reported.
    pub semantic: Vec<ProtocolIdentityFact>,
}

impl CanonicalCbor for TranscriptRecord {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("sequence", CborValue::Unsigned(self.sequence as u64)),
            ("kind", CborValue::text(self.kind.as_str())),
            ("atEpochMs", CborValue::Unsigned(self.at_epoch_ms)),
            ("durationMs", CborValue::Unsigned(self.duration_ms)),
            ("status", CborValue::text(self.status.as_str())),
            (
                "observation",
                match &self.observation {
                    Some(observation) => observation.to_cbor(),
                    None => CborValue::Null,
                },
            ),
            (
                "action",
                match &self.action {
                    Some(action) => action.to_cbor(),
                    None => CborValue::Null,
                },
            ),
            (
                "requestBytes",
                match self.request_bytes {
                    Some(value) => CborValue::Unsigned(value),
                    None => CborValue::Null,
                },
            ),
            (
                "requestDigest",
                match self.request_digest {
                    Some(digest) => digest.to_cbor(),
                    None => CborValue::Null,
                },
            ),
            (
                "responseBytes",
                match self.response_bytes {
                    Some(value) => CborValue::Unsigned(value),
                    None => CborValue::Null,
                },
            ),
            (
                "responseDigest",
                match self.response_digest {
                    Some(digest) => digest.to_cbor(),
                    None => CborValue::Null,
                },
            ),
            (
                "semantic",
                CborValue::array(self.semantic.iter().map(|f| f.to_cbor()).collect()),
            ),
        ])
    }
}

impl TranscriptRecord {
    pub fn semantic_value(&self, key: &str) -> Option<&str> {
        self.semantic
            .iter()
            .find(|fact| fact.key.as_str() == key)
            .map(|fact| fact.value.as_str())
    }
}

/// A complete transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    pub schema_version: String,
    pub id: OpaqueId,
    pub provenance: TranscriptProvenance,
    pub source: String,
    pub profile_id: OpaqueId,
    pub records: Vec<TranscriptRecord>,
}

impl Transcript {
    pub fn digest(&self) -> Result<Sha256Digest, CborError> {
        digest_canonical(Domain::Transcript, self)
    }

    pub fn observations(&self) -> impl Iterator<Item = &DeviceObservation> {
        self.records
            .iter()
            .filter_map(|record| record.observation.as_ref())
    }

    pub fn invocations(&self, action: &str) -> impl Iterator<Item = &TranscriptRecord> + use<'_> {
        // The action is copied into the closure so the returned iterator
        // borrows only the transcript, not the caller's string.
        let action = action.to_string();
        self.records.iter().filter(move |record| {
            record.kind == RecordKind::Invocation
                && record
                    .action
                    .as_ref()
                    .map(|id| id.as_str() == action)
                    .unwrap_or(false)
        })
    }

    fn validate(&self) -> Result<(), TranscriptError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(TranscriptError::UnknownSchemaVersion(
                self.schema_version.clone(),
            ));
        }
        if self.records.is_empty() {
            return Err(TranscriptError::Empty);
        }
        for (index, record) in self.records.iter().enumerate() {
            if record.sequence as usize != index + 1 {
                return Err(TranscriptError::SequenceNotContiguous(record.sequence));
            }
            if record.kind == RecordKind::Invocation && record.action.is_none() {
                return Err(TranscriptError::InvocationWithoutAction(record.sequence));
            }
            if matches!(record.kind, RecordKind::Observation | RecordKind::Rebind)
                && record.observation.is_none()
            {
                return Err(TranscriptError::ObservationRecordWithoutObservation(
                    record.sequence,
                ));
            }
        }
        Ok(())
    }
}

impl CanonicalCbor for Transcript {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            (
                "schemaVersion",
                CborValue::text(self.schema_version.clone()),
            ),
            ("id", self.id.to_cbor()),
            ("provenance", self.provenance.to_cbor()),
            ("source", CborValue::text(self.source.clone())),
            ("profileId", self.profile_id.to_cbor()),
            (
                "records",
                CborValue::array(self.records.iter().map(|r| r.to_cbor()).collect()),
            ),
        ])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptError {
    Yaml(yaml::YamlError),
    UnknownSchemaVersion(String),
    Empty,
    SequenceNotContiguous(u32),
    InvocationWithoutAction(u32),
    ObservationRecordWithoutObservation(u32),
    MissingField(String),
    BadField { field: String, detail: String },
}

impl fmt::Display for TranscriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TranscriptError::Yaml(error) => write!(f, "{error}"),
            TranscriptError::UnknownSchemaVersion(version) => write!(
                f,
                "unknown transcript schema version {version:?}; this build reads {SCHEMA_VERSION}"
            ),
            TranscriptError::Empty => f.write_str("transcript has no records"),
            TranscriptError::SequenceNotContiguous(sequence) => write!(
                f,
                "record sequence {sequence} breaks the contiguous ordering"
            ),
            TranscriptError::InvocationWithoutAction(sequence) => {
                write!(f, "invocation record {sequence} names no action")
            }
            TranscriptError::ObservationRecordWithoutObservation(sequence) => {
                write!(
                    f,
                    "record {sequence} declares an observation kind but carries none"
                )
            }
            TranscriptError::MissingField(field) => write!(f, "transcript is missing {field}"),
            TranscriptError::BadField { field, detail } => {
                write!(f, "transcript field {field}: {detail}")
            }
        }
    }
}

impl std::error::Error for TranscriptError {}

fn missing(field: &str) -> TranscriptError {
    TranscriptError::MissingField(field.to_string())
}

fn bad(field: &str, detail: impl Into<String>) -> TranscriptError {
    TranscriptError::BadField {
        field: field.to_string(),
        detail: detail.into(),
    }
}

fn text<'a>(value: &'a YamlValue, key: &str, path: &str) -> Result<&'a str, TranscriptError> {
    value
        .get(key)
        .and_then(YamlValue::as_scalar)
        .ok_or_else(|| missing(path))
}

fn number(value: &YamlValue, key: &str, path: &str) -> Result<u64, TranscriptError> {
    text(value, key, path)?
        .parse()
        .map_err(|_| bad(path, "expected a number"))
}

fn optional_number(value: &YamlValue, key: &str) -> Option<u64> {
    value
        .get(key)
        .and_then(YamlValue::as_scalar)
        .and_then(|text| text.parse().ok())
}

fn optional_digest(
    value: &YamlValue,
    key: &str,
    path: &str,
) -> Result<Option<Sha256Digest>, TranscriptError> {
    match value.get(key).and_then(YamlValue::as_scalar) {
        None => Ok(None),
        Some(raw) => {
            let hex = raw.strip_prefix("sha256:").unwrap_or(raw);
            Sha256Digest::parse_hex(hex)
                .map(Some)
                .map_err(|error| bad(path, error.to_string()))
        }
    }
}

fn digest(value: &YamlValue, key: &str, path: &str) -> Result<Sha256Digest, TranscriptError> {
    optional_digest(value, key, path)?.ok_or_else(|| missing(path))
}

fn semantic_facts(value: &YamlValue) -> Result<Vec<ProtocolIdentityFact>, TranscriptError> {
    let mut facts = Vec::new();
    for entry in value
        .get("semantic")
        .and_then(YamlValue::as_sequence)
        .unwrap_or(&[])
    {
        let key = text(entry, "key", "records[].semantic[].key")?;
        let value = text(entry, "value", "records[].semantic[].value")?;
        facts.push(ProtocolIdentityFact {
            key: OpaqueId::new(key)
                .map_err(|error| bad("records[].semantic[].key", error.to_string()))?,
            value: value.to_string(),
        });
    }
    Ok(facts)
}

fn parse_observation(
    value: &YamlValue,
    at_epoch_ms: u64,
) -> Result<Option<DeviceObservation>, TranscriptError> {
    let Some(block) = value.get("observation") else {
        return Ok(None);
    };
    if block.is_null() {
        return Ok(None);
    }
    let serial_kind = block
        .get("serialKind")
        .and_then(YamlValue::as_scalar)
        .unwrap_or("absent");
    let serial_digest =
        optional_digest(block, "serialDigest", "records[].observation.serialDigest")?;
    let serial_evidence = match (serial_kind, serial_digest) {
        ("absent", _) => SerialEvidence::Absent,
        ("descriptor", Some(digest)) => SerialEvidence::Descriptor { digest },
        ("protocolUnique", Some(digest)) => SerialEvidence::ProtocolUnique { digest },
        (kind, None) => {
            return Err(bad(
                "records[].observation.serialDigest",
                format!("serialKind {kind:?} requires a digest"),
            ));
        }
        (kind, _) => {
            return Err(bad(
                "records[].observation.serialKind",
                format!("unknown serial kind {kind:?}"),
            ));
        }
    };

    let mut protocol_identity = Vec::new();
    for entry in block
        .get("protocolIdentity")
        .and_then(YamlValue::as_sequence)
        .unwrap_or(&[])
    {
        protocol_identity.push(ProtocolIdentityFact {
            key: OpaqueId::new(text(entry, "key", "observation.protocolIdentity[].key")?)
                .map_err(|error| bad("observation.protocolIdentity[].key", error.to_string()))?,
            value: text(entry, "value", "observation.protocolIdentity[].value")?.to_string(),
        });
    }

    let mut provider_candidates = Vec::new();
    for entry in block
        .get("providerCandidates")
        .and_then(YamlValue::as_sequence)
        .unwrap_or(&[])
    {
        provider_candidates.push(ProviderCandidateRef {
            provider_id: OpaqueId::new(text(
                entry,
                "providerId",
                "observation.providerCandidates[].providerId",
            )?)
            .map_err(|error| {
                bad(
                    "observation.providerCandidates[].providerId",
                    error.to_string(),
                )
            })?,
            confidence: OpaqueId::new(text(
                entry,
                "confidence",
                "observation.providerCandidates[].confidence",
            )?)
            .map_err(|error| {
                bad(
                    "observation.providerCandidates[].confidence",
                    error.to_string(),
                )
            })?,
        });
    }

    Ok(Some(DeviceObservation {
        observation_id: ObservationId::new(text(block, "id", "records[].observation.id")?)
            .map_err(|error| bad("records[].observation.id", error.to_string()))?,
        observed_at_epoch_ms: at_epoch_ms,
        mode: DeviceMode::new(text(block, "mode", "records[].observation.mode")?)
            .map_err(|error| bad("records[].observation.mode", error.to_string()))?,
        topology_digest: digest(
            block,
            "topologyDigest",
            "records[].observation.topologyDigest",
        )?,
        descriptor_digest: digest(
            block,
            "descriptorDigest",
            "records[].observation.descriptorDigest",
        )?,
        serial_evidence,
        protocol_identity,
        provider_candidates,
        identity_strength: IdentityEvidenceStrength::parse(text(
            block,
            "identityStrength",
            "records[].observation.identityStrength",
        )?)
        .ok_or_else(|| bad("records[].observation.identityStrength", "unknown strength"))?,
        malformed_descriptor: block
            .get("malformedDescriptor")
            .and_then(YamlValue::as_scalar)
            .map(|value| value == "true")
            .unwrap_or(false),
    }))
}

/// Parses a transcript document.
pub fn parse(source: &str) -> Result<Transcript, TranscriptError> {
    let document = yaml::parse(source).map_err(TranscriptError::Yaml)?;
    let header = document
        .get("transcript")
        .ok_or_else(|| missing("transcript"))?;

    let mut records = Vec::new();
    for entry in document
        .get("records")
        .and_then(YamlValue::as_sequence)
        .ok_or_else(|| missing("records"))?
    {
        let at_epoch_ms = number(entry, "atEpochMs", "records[].atEpochMs")?;
        let kind = RecordKind::parse(text(entry, "kind", "records[].kind")?)
            .ok_or_else(|| bad("records[].kind", "unknown record kind"))?;
        let status = RecordStatus::parse(text(entry, "status", "records[].status")?)
            .ok_or_else(|| bad("records[].status", "unknown status"))?;
        let action = match entry.get("action").and_then(YamlValue::as_scalar) {
            Some(value) => Some(
                OpaqueId::new(value).map_err(|error| bad("records[].action", error.to_string()))?,
            ),
            None => None,
        };
        records.push(TranscriptRecord {
            sequence: number(entry, "sequence", "records[].sequence")? as u32,
            kind,
            at_epoch_ms,
            duration_ms: optional_number(entry, "durationMs").unwrap_or(0),
            status,
            observation: parse_observation(entry, at_epoch_ms)?,
            action,
            request_bytes: optional_number(entry, "requestBytes"),
            request_digest: optional_digest(entry, "requestDigest", "records[].requestDigest")?,
            response_bytes: optional_number(entry, "responseBytes"),
            response_digest: optional_digest(entry, "responseDigest", "records[].responseDigest")?,
            semantic: semantic_facts(entry)?,
        });
    }

    let transcript = Transcript {
        schema_version: text(&document, "schemaVersion", "schemaVersion")?.to_string(),
        id: OpaqueId::new(text(header, "id", "transcript.id")?)
            .map_err(|error| bad("transcript.id", error.to_string()))?,
        provenance: TranscriptProvenance::parse(text(
            header,
            "provenance",
            "transcript.provenance",
        )?)
        .ok_or_else(|| bad("transcript.provenance", "unknown provenance"))?,
        source: text(header, "source", "transcript.source")?.to_string(),
        profile_id: OpaqueId::new(text(header, "profileId", "transcript.profileId")?)
            .map_err(|error| bad("transcript.profileId", error.to_string()))?,
        records,
    };
    transcript.validate()?;
    Ok(transcript)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
schemaVersion: arkforge.transcript/v1
transcript:
  id: TEST-0001
  provenance: synthetic
  source: "unit test"
  profileId: org.openharmony.dayu200
records:
  - sequence: 1
    kind: observation
    atEpochMs: 1000
    status: ok
    observation:
      id: OBS-1
      mode: hdc-normal
      topologyDigest: 0000000000000000000000000000000000000000000000000000000000000001
      descriptorDigest: 0000000000000000000000000000000000000000000000000000000000000002
      serialKind: descriptor
      serialDigest: 0000000000000000000000000000000000000000000000000000000000000003
      identityStrength: protocolConfirmed
  - sequence: 2
    kind: invocation
    atEpochMs: 2000
    durationMs: 1500
    status: ok
    action: enter-updater
    responseBytes: 24
    responseDigest: 0000000000000000000000000000000000000000000000000000000000000004
    semantic:
      - key: marker
        value: "accepted"
"#;

    #[test]
    fn parses_records_and_observations() {
        let transcript = parse(SAMPLE).unwrap();
        assert_eq!(transcript.id.as_str(), "TEST-0001");
        assert_eq!(transcript.provenance, TranscriptProvenance::Synthetic);
        assert_eq!(transcript.records.len(), 2);
        assert_eq!(transcript.observations().count(), 1);
        let invocation = transcript.invocations("enter-updater").next().unwrap();
        assert_eq!(invocation.semantic_value("marker"), Some("accepted"));
        assert_eq!(invocation.duration_ms, 1500);
    }

    #[test]
    fn a_transcript_digest_is_reproducible() {
        assert_eq!(
            parse(SAMPLE).unwrap().digest().unwrap(),
            parse(SAMPLE).unwrap().digest().unwrap()
        );
    }

    #[test]
    fn a_gap_in_the_sequence_is_rejected() {
        let document = SAMPLE.replace("sequence: 2", "sequence: 3");
        assert!(matches!(
            parse(&document),
            Err(TranscriptError::SequenceNotContiguous(3))
        ));
    }

    #[test]
    fn an_invocation_without_an_action_is_rejected() {
        let document = SAMPLE.replace("    action: enter-updater\n", "");
        assert!(matches!(
            parse(&document),
            Err(TranscriptError::InvocationWithoutAction(2))
        ));
    }

    #[test]
    fn an_unknown_schema_version_fails_closed() {
        let document = SAMPLE.replace("arkforge.transcript/v1", "arkforge.transcript/v2");
        assert!(matches!(
            parse(&document),
            Err(TranscriptError::UnknownSchemaVersion(_))
        ));
    }

    #[test]
    fn only_a_capture_supports_a_protocol_claim() {
        assert!(TranscriptProvenance::Captured.supports_protocol_claims());
        assert!(!TranscriptProvenance::DerivedFromPublishedReceipts.supports_protocol_claims());
        assert!(!TranscriptProvenance::Synthetic.supports_protocol_claims());
    }
}
