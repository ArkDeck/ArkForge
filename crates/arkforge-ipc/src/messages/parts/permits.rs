use super::*;

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
                3 => event.kind = decode_enum(3, &value, JobEventKind::from_wire)?,
                4 => event.at_epoch_ms = value.as_u64()?,
                5 => event.journal_record_sha256 = value.as_bytes()?.to_vec(),
                6 => event.job_state = value.as_str(6)?.to_string(),
                7 => event.admission = Some(StepAdmissionSnapshot::decode(value.as_bytes()?)?),
                8 => {
                    event.control_request = Some(ManagedControlRequest::decode(value.as_bytes()?)?)
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
                3 => request.action = decode_enum(3, &value, ManagedControlAction::from_wire)?,
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
