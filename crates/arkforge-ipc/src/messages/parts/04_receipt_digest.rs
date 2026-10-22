use super::*;

impl ActionReceiptSummary {
    /// The semantic receipt digest stored in the durable permit ledger.
    ///
    /// Protobuf is intentionally not the digest body: unknown fields and field
    /// order are wire-compatible there. The corresponding canonical CBOR body
    /// is fixed by [`CanonicalCbor`] below so Rust, Swift, Zig and C++ ports can
    /// reproduce the same receipt identity.
    pub fn receipt_digest(&self) -> Result<Sha256Digest, CborError> {
        for (field, value) in [
            ("jobId", self.job_id.as_str()),
            ("planId", self.plan_id.as_str()),
            ("stepId", self.step_id.as_str()),
            ("attemptId", self.attempt_id.as_str()),
            ("permitId", self.permit_id.as_str()),
        ] {
            OpaqueId::new(value).map_err(|error| {
                CborError::ModelViolation(format!("{field} is not an OpaqueId: {error}"))
            })?;
        }
        if !self.action_id.is_empty() {
            OpaqueId::new(&self.action_id).map_err(|error| {
                CborError::ModelViolation(format!("actionId is not an OpaqueId: {error}"))
            })?;
        }
        if !matches!(
            self.disposition.as_str(),
            "semanticSuccess" | "confirmedNoEffect" | "confirmedPartialEffect" | "outcomeUnknown"
        ) {
            return Err(CborError::ModelViolation(format!(
                "unknown action disposition {:?}",
                self.disposition
            )));
        }
        if self.evidence_sha256.len() != 32 {
            return Err(CborError::ModelViolation(format!(
                "evidenceSha256 is {} bytes, expected 32",
                self.evidence_sha256.len()
            )));
        }
        if !matches!(
            self.verification_outcome.as_str(),
            "" | "verified" | "typedSkip" | "failed"
        ) {
            return Err(CborError::ModelViolation(format!(
                "unknown verification outcome {:?}",
                self.verification_outcome
            )));
        }
        if !self.strength_is_consistent() {
            return Err(CborError::ModelViolation(
                "verificationStrength is present without a verified outcome".into(),
            ));
        }
        if !self.verification_strength.is_empty()
            && !matches!(
                self.verification_strength.as_str(),
                "fullHash" | "sampledRanges" | "prefixHash" | "semanticOnly"
            )
        {
            return Err(CborError::ModelViolation(format!(
                "unknown verification strength {:?}",
                self.verification_strength
            )));
        }
        for fact in &self.facts {
            OpaqueId::new(&fact.key).map_err(|error| {
                CborError::ModelViolation(format!(
                    "fact key {:?} is not an OpaqueId: {error}",
                    fact.key
                ))
            })?;
        }
        digest_canonical(Domain::ActionReceipt, self)
    }

    /// Reconstructs the exact semantic receipt body stored in a durable
    /// journal. This is deliberately separate from protobuf decoding: the
    /// journal stores the canonical digest body so a restarted daemon can
    /// reproduce both the receipt and its identity without trusting process
    /// memory or protobuf field order.
    pub fn from_canonical_bytes(input: &[u8]) -> Result<Self, CborError> {
        let value = decode_canonical(input)?;
        let CborValue::Map(entries) = value else {
            return Err(receipt_model("receipt body is not a map"));
        };
        let mut fields = BTreeMap::new();
        for (key, value) in entries {
            let CborValue::Text(key) = key else {
                return Err(receipt_model("receipt field name is not text"));
            };
            if fields.insert(key.clone(), value).is_some() {
                return Err(receipt_model(format!("duplicate receipt field {key:?}")));
            }
        }
        if fields.len() != 15 {
            return Err(receipt_model(format!(
                "receipt body has {} fields, expected 15",
                fields.len()
            )));
        }
        match take_receipt_field(&mut fields, "schemaVersion")? {
            CborValue::Unsigned(1) => {}
            _ => return Err(receipt_model("schemaVersion must be 1")),
        }

        let verified_range = take_receipt_field(&mut fields, "verifiedRange")?;
        let (verified_range_start, verified_range_length) = match verified_range {
            CborValue::Null => (0, 0),
            CborValue::Map(entries) => {
                let mut range = BTreeMap::new();
                for (key, value) in entries {
                    let CborValue::Text(key) = key else {
                        return Err(receipt_model("verifiedRange field name is not text"));
                    };
                    if range.insert(key.clone(), value).is_some() {
                        return Err(receipt_model(format!(
                            "duplicate verifiedRange field {key:?}"
                        )));
                    }
                }
                if range.len() != 2 {
                    return Err(receipt_model("verifiedRange must contain start and length"));
                }
                let start = take_unsigned_field(&mut range, "start")?;
                let length = take_unsigned_field(&mut range, "length")?;
                (start, length)
            }
            _ => return Err(receipt_model("verifiedRange must be a map or null")),
        };

        let facts_value = take_receipt_field(&mut fields, "facts")?;
        let CborValue::Map(fact_entries) = facts_value else {
            return Err(receipt_model("facts must be a map"));
        };
        let mut facts = Vec::with_capacity(fact_entries.len());
        for (key, value) in fact_entries {
            let (CborValue::Text(key), CborValue::Text(value)) = (key, value) else {
                return Err(receipt_model("fact keys and values must be text"));
            };
            facts.push(KeyValue { key, value });
        }

        let receipt = ActionReceiptSummary {
            job_id: take_text_field(&mut fields, "jobId")?,
            plan_id: take_text_field(&mut fields, "planId")?,
            step_id: take_text_field(&mut fields, "stepId")?,
            action_id: take_optional_text_field(&mut fields, "actionId")?,
            attempt_id: take_text_field(&mut fields, "attemptId")?,
            permit_id: take_text_field(&mut fields, "permitId")?,
            disposition: take_text_field(&mut fields, "disposition")?,
            evidence_sha256: match take_receipt_field(&mut fields, "evidenceSha256")? {
                CborValue::Bytes(value) => value,
                _ => return Err(receipt_model("evidenceSha256 must be bytes")),
            },
            verification_outcome: take_optional_text_field(&mut fields, "verificationOutcome")?,
            verification_strength: take_optional_text_field(&mut fields, "verificationStrength")?,
            verified_range_start,
            verified_range_length,
            typed_skip_reason: take_optional_text_field(&mut fields, "typedSkipReason")?,
            failure_classification: take_optional_text_field(&mut fields, "failureClassification")?,
            facts,
        };
        if !fields.is_empty() {
            return Err(receipt_model(format!(
                "unknown receipt fields: {:?}",
                fields.keys().collect::<Vec<_>>()
            )));
        }
        // Reuse the same model validation as the hashing ingress.
        receipt.receipt_digest()?;
        Ok(receipt)
    }

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

pub(super) fn receipt_model(message: impl Into<String>) -> CborError {
    CborError::ModelViolation(message.into())
}

pub(super) fn take_receipt_field(
    fields: &mut BTreeMap<String, CborValue>,
    name: &str,
) -> Result<CborValue, CborError> {
    fields
        .remove(name)
        .ok_or_else(|| receipt_model(format!("missing receipt field {name}")))
}

pub(super) fn take_text_field(
    fields: &mut BTreeMap<String, CborValue>,
    name: &str,
) -> Result<String, CborError> {
    match take_receipt_field(fields, name)? {
        CborValue::Text(value) => Ok(value),
        _ => Err(receipt_model(format!("{name} must be text"))),
    }
}

pub(super) fn take_optional_text_field(
    fields: &mut BTreeMap<String, CborValue>,
    name: &str,
) -> Result<String, CborError> {
    match take_receipt_field(fields, name)? {
        CborValue::Text(value) => Ok(value),
        CborValue::Null => Ok(String::new()),
        _ => Err(receipt_model(format!("{name} must be text or null"))),
    }
}

pub(super) fn take_unsigned_field(
    fields: &mut BTreeMap<String, CborValue>,
    name: &str,
) -> Result<u64, CborError> {
    match fields
        .remove(name)
        .ok_or_else(|| receipt_model(format!("missing field {name}")))?
    {
        CborValue::Unsigned(value) => Ok(value),
        _ => Err(receipt_model(format!("{name} must be unsigned"))),
    }
}

impl CanonicalCbor for ActionReceiptSummary {
    fn to_cbor(&self) -> CborValue {
        let optional_text = |value: &str| {
            if value.is_empty() {
                CborValue::Null
            } else {
                CborValue::text(value)
            }
        };
        let facts = CborValue::Map(
            self.facts
                .iter()
                .map(|fact| {
                    (
                        CborValue::text(fact.key.clone()),
                        CborValue::text(fact.value.clone()),
                    )
                })
                .collect(),
        );
        let verified_range = if self.verification_outcome.is_empty() {
            CborValue::Null
        } else {
            CborValue::map(vec![
                ("start", CborValue::Unsigned(self.verified_range_start)),
                ("length", CborValue::Unsigned(self.verified_range_length)),
            ])
        };

        CborValue::map(vec![
            ("schemaVersion", CborValue::Unsigned(1)),
            ("jobId", CborValue::text(self.job_id.clone())),
            ("planId", CborValue::text(self.plan_id.clone())),
            ("stepId", CborValue::text(self.step_id.clone())),
            ("actionId", optional_text(&self.action_id)),
            ("attemptId", CborValue::text(self.attempt_id.clone())),
            ("permitId", CborValue::text(self.permit_id.clone())),
            ("disposition", CborValue::text(self.disposition.clone())),
            (
                "evidenceSha256",
                CborValue::bytes(self.evidence_sha256.clone()),
            ),
            (
                "verificationOutcome",
                optional_text(&self.verification_outcome),
            ),
            (
                "verificationStrength",
                optional_text(&self.verification_strength),
            ),
            ("verifiedRange", verified_range),
            ("typedSkipReason", optional_text(&self.typed_skip_reason)),
            (
                "failureClassification",
                optional_text(&self.failure_classification),
            ),
            ("facts", facts),
        ])
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
