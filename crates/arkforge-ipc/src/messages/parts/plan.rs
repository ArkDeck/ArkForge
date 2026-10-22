
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
                4 => response
                    .members
                    .push(ArchiveMember::decode(value.as_bytes()?)?),
                5 => response
                    .partitions
                    .push(PartitionEntry::decode(value.as_bytes()?)?),
                6 => response
                    .build_facts
                    .push(KeyValue::decode(value.as_bytes()?)?),
                7 => response
                    .unclassified_members
                    .push(value.as_str(7)?.to_string()),
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
    pub execution_purpose: String,
    pub mechanics_maturity_key_sha256: String,
    pub mechanics_maturity_state: String,
    pub mechanics_maturity_campaign: String,
    pub authority_support_key_sha256: String,
    pub authority_support_state: String,
    pub authority_support_campaign: String,
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
    pub mechanics_maturity_key_sha256: String,
    pub mechanics_maturity_state: String,
    pub authority_support_key_sha256: String,
    pub authority_support_state: String,
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
        wire::write_string(&mut out, 10, &self.execution_purpose);
        wire::write_string(&mut out, 11, &self.mechanics_maturity_key_sha256);
        wire::write_string(&mut out, 12, &self.mechanics_maturity_state);
        wire::write_string(&mut out, 13, &self.mechanics_maturity_campaign);
        wire::write_string(&mut out, 14, &self.authority_support_key_sha256);
        wire::write_string(&mut out, 15, &self.authority_support_state);
        wire::write_string(&mut out, 16, &self.authority_support_campaign);
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
                5 => plan
                    .public_steps
                    .push(PublicStep::decode(value.as_bytes()?)?),
                6 => plan
                    .persistent_effects
                    .push(Effect::decode(value.as_bytes()?)?),
                7 => plan
                    .transient_effects
                    .push(Effect::decode(value.as_bytes()?)?),
                8 => plan.data_impact.push(KeyValue::decode(value.as_bytes()?)?),
                9 => plan.expires_at_epoch_ms = value.as_u64()?,
                10 => plan.execution_purpose = value.as_str(10)?.to_string(),
                11 => plan.mechanics_maturity_key_sha256 = value.as_str(11)?.to_string(),
                12 => plan.mechanics_maturity_state = value.as_str(12)?.to_string(),
                13 => plan.mechanics_maturity_campaign = value.as_str(13)?.to_string(),
                14 => plan.authority_support_key_sha256 = value.as_str(14)?.to_string(),
                15 => plan.authority_support_state = value.as_str(15)?.to_string(),
                16 => plan.authority_support_campaign = value.as_str(16)?.to_string(),
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
        wire::write_string(&mut out, 8, &self.mechanics_maturity_key_sha256);
        wire::write_string(&mut out, 9, &self.mechanics_maturity_state);
        wire::write_string(&mut out, 10, &self.authority_support_key_sha256);
        wire::write_string(&mut out, 11, &self.authority_support_state);
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
                3 => assessment
                    .unknowns
                    .push(KeyValue::decode(value.as_bytes()?)?),
                4 => assessment
                    .evidence_requirements
                    .push(KeyValue::decode(value.as_bytes()?)?),
                5 => assessment.availability = value.as_str(5)?.to_string(),
                6 => assessment.unavailable_reason = value.as_str(6)?.to_string(),
                7 => assessment
                    .data_impact
                    .push(KeyValue::decode(value.as_bytes()?)?),
                8 => assessment.mechanics_maturity_key_sha256 = value.as_str(8)?.to_string(),
                9 => assessment.mechanics_maturity_state = value.as_str(9)?.to_string(),
                10 => assessment.authority_support_key_sha256 = value.as_str(10)?.to_string(),
                11 => assessment.authority_support_state = value.as_str(11)?.to_string(),
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

/// Durable point-in-time status, separate from the incremental event stream.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JobSummary {
    pub job_id: String,
    pub plan_id: String,
    pub plan_sha256: Vec<u8>,
    pub state: String,
    pub terminal: bool,
    pub current_step_id: String,
    pub completed_steps: u64,
    pub total_steps: u64,
    pub last_sequence: u64,
    pub stopped_reason: String,
}
