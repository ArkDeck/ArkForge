
impl JobSummary {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_string(&mut out, 1, &self.job_id);
        wire::write_string(&mut out, 2, &self.plan_id);
        wire::write_bytes(&mut out, 3, &self.plan_sha256);
        wire::write_string(&mut out, 4, &self.state);
        wire::write_bool(&mut out, 5, self.terminal);
        wire::write_string(&mut out, 6, &self.current_step_id);
        wire::write_uint64(&mut out, 7, self.completed_steps);
        wire::write_uint64(&mut out, 8, self.total_steps);
        wire::write_uint64(&mut out, 9, self.last_sequence);
        wire::write_string(&mut out, 10, &self.stopped_reason);
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut summary = JobSummary::default();
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => summary.job_id = value.as_str(1)?.to_string(),
                2 => summary.plan_id = value.as_str(2)?.to_string(),
                3 => summary.plan_sha256 = value.as_bytes()?.to_vec(),
                4 => summary.state = value.as_str(4)?.to_string(),
                5 => summary.terminal = value.as_bool()?,
                6 => summary.current_step_id = value.as_str(6)?.to_string(),
                7 => summary.completed_steps = value.as_u64()?,
                8 => summary.total_steps = value.as_u64()?,
                9 => summary.last_sequence = value.as_u64()?,
                10 => summary.stopped_reason = value.as_str(10)?.to_string(),
                _ => {}
            }
        }
        Ok(summary)
    }
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
    pub topology_sha256: Vec<u8>,
    pub descriptor_sha256: Vec<u8>,
    pub serial_sha256: Vec<u8>,
    pub serial_evidence_kind: String,
    pub protocol_identity: Vec<KeyValue>,
    pub identity_strength: String,
    pub malformed_descriptor: bool,
    pub transport_session_sha256: Vec<u8>,
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
        wire::write_bytes(&mut out, 14, &self.topology_sha256);
        wire::write_bytes(&mut out, 15, &self.descriptor_sha256);
        wire::write_bytes(&mut out, 16, &self.serial_sha256);
        wire::write_string(&mut out, 17, &self.serial_evidence_kind);
        for fact in &self.protocol_identity {
            wire::write_message(&mut out, 18, &fact.encode());
        }
        wire::write_string(&mut out, 19, &self.identity_strength);
        wire::write_bool(&mut out, 20, self.malformed_descriptor);
        wire::write_bytes(&mut out, 21, &self.transport_session_sha256);
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
                14 => snapshot.topology_sha256 = value.as_bytes()?.to_vec(),
                15 => snapshot.descriptor_sha256 = value.as_bytes()?.to_vec(),
                16 => snapshot.serial_sha256 = value.as_bytes()?.to_vec(),
                17 => snapshot.serial_evidence_kind = value.as_str(17)?.to_string(),
                18 => snapshot
                    .protocol_identity
                    .push(KeyValue::decode(value.as_bytes()?)?),
                19 => snapshot.identity_strength = value.as_str(19)?.to_string(),
                20 => snapshot.malformed_descriptor = value.as_bool()?,
                21 => snapshot.transport_session_sha256 = value.as_bytes()?.to_vec(),
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
        match self
            .observed_at_epoch_ms
            .checked_add(self.snapshot_lifetime_ms)
        {
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
                4 => request.action = decode_enum(4, &value, ManagedControlAction::from_wire)?,
                5 => request.permit_id = value.as_str(5)?.to_string(),
                6 => request
                    .expected_facts
                    .push(KeyValue::decode(value.as_bytes()?)?),
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
