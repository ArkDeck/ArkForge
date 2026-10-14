//! Read-only recovery assessments. These messages describe verified source
//! records and a candidate plan; they never carry a permit or caller facts.

use crate::messages::ManagedControlAction;
use crate::wire::{self, Reader, WireError, decode_enum};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosedControlEvidence {
    pub step_id: String,
    pub permit_id: String,
    pub request_id: String,
    pub action: ManagedControlAction,
    pub dispatch_record_sha256: String,
    pub deadline_epoch_ms: u64,
    pub dispatched_at_epoch_ms: u64,
    pub attempt_id: String,
    pub controller_session_id: String,
}

impl ClosedControlEvidence {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_string(&mut out, 1, &self.step_id);
        wire::write_string(&mut out, 2, &self.permit_id);
        wire::write_string(&mut out, 3, &self.request_id);
        wire::write_enum(&mut out, 4, self.action.wire_value());
        wire::write_string(&mut out, 5, &self.dispatch_record_sha256);
        wire::write_uint64(&mut out, 6, self.deadline_epoch_ms);
        wire::write_uint64(&mut out, 7, self.dispatched_at_epoch_ms);
        wire::write_string(&mut out, 8, &self.attempt_id);
        wire::write_string(&mut out, 9, &self.controller_session_id);
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut step_id = String::new();
        let mut permit_id = String::new();
        let mut request_id = String::new();
        let mut action = None;
        let mut dispatch_record_sha256 = String::new();
        let mut deadline_epoch_ms = 0;
        let mut dispatched_at_epoch_ms = 0;
        let mut attempt_id = String::new();
        let mut controller_session_id = String::new();
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => step_id = value.as_str(field)?.to_string(),
                2 => permit_id = value.as_str(field)?.to_string(),
                3 => request_id = value.as_str(field)?.to_string(),
                4 => action = Some(decode_enum(field, &value, ManagedControlAction::from_wire)?),
                5 => dispatch_record_sha256 = value.as_str(field)?.to_string(),
                6 => deadline_epoch_ms = value.as_u64()?,
                7 => dispatched_at_epoch_ms = value.as_u64()?,
                8 => attempt_id = value.as_str(field)?.to_string(),
                9 => controller_session_id = value.as_str(field)?.to_string(),
                _ => {}
            }
        }
        Ok(Self {
            step_id,
            permit_id,
            request_id,
            action: action.ok_or(WireError::MissingField {
                message: "ClosedControlEvidence",
                field: 4,
            })?,
            dispatch_record_sha256,
            deadline_epoch_ms,
            dispatched_at_epoch_ms,
            attempt_id,
            controller_session_id,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecoverySourceEvidence {
    pub plan_id: String,
    pub plan_sha256: String,
    pub journal_head_sha256: String,
    pub journal_sequence: u64,
    pub closed_control: Option<ClosedControlEvidence>,
}

impl RecoverySourceEvidence {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_string(&mut out, 1, &self.plan_id);
        wire::write_string(&mut out, 2, &self.plan_sha256);
        wire::write_string(&mut out, 3, &self.journal_head_sha256);
        wire::write_uint64(&mut out, 4, self.journal_sequence);
        if let Some(control) = &self.closed_control {
            wire::write_message(&mut out, 5, &control.encode());
        }
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut out = Self::default();
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => out.plan_id = value.as_str(field)?.to_string(),
                2 => out.plan_sha256 = value.as_str(field)?.to_string(),
                3 => out.journal_head_sha256 = value.as_str(field)?.to_string(),
                4 => out.journal_sequence = value.as_u64()?,
                5 => out.closed_control = Some(ClosedControlEvidence::decode(value.as_bytes()?)?),
                _ => {}
            }
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SupersedingRecoveryAssessment {
    pub job_id: String,
    pub eligible: bool,
    pub blocker_code: String,
    pub blocker_reason: String,
    pub covered_effects: Vec<String>,
    pub recovery_contract_id: String,
    pub recovery_contract_version: String,
    pub recovery_contract_sha256: String,
    pub source: Option<RecoverySourceEvidence>,
    pub candidate_plan_id: String,
    pub candidate_plan_sha256: String,
    pub candidate_profile_sha256: String,
    pub candidate_artifact_sha256: String,
}

impl SupersedingRecoveryAssessment {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_string(&mut out, 1, &self.job_id);
        wire::write_bool(&mut out, 2, self.eligible);
        wire::write_string(&mut out, 3, &self.blocker_code);
        wire::write_string(&mut out, 4, &self.blocker_reason);
        for effect in &self.covered_effects {
            wire::write_string(&mut out, 5, effect);
        }
        wire::write_string(&mut out, 6, &self.recovery_contract_id);
        wire::write_string(&mut out, 7, &self.recovery_contract_version);
        wire::write_string(&mut out, 8, &self.recovery_contract_sha256);
        if let Some(source) = &self.source {
            wire::write_message(&mut out, 9, &source.encode());
        }
        wire::write_string(&mut out, 10, &self.candidate_plan_id);
        wire::write_string(&mut out, 11, &self.candidate_plan_sha256);
        wire::write_string(&mut out, 12, &self.candidate_profile_sha256);
        wire::write_string(&mut out, 13, &self.candidate_artifact_sha256);
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, WireError> {
        let mut out = Self::default();
        let mut reader = Reader::new(input);
        while let Some((field, value)) = reader.next_field()? {
            match field {
                1 => out.job_id = value.as_str(field)?.to_string(),
                2 => out.eligible = value.as_bool()?,
                3 => out.blocker_code = value.as_str(field)?.to_string(),
                4 => out.blocker_reason = value.as_str(field)?.to_string(),
                5 => out.covered_effects.push(value.as_str(field)?.to_string()),
                6 => out.recovery_contract_id = value.as_str(field)?.to_string(),
                7 => out.recovery_contract_version = value.as_str(field)?.to_string(),
                8 => out.recovery_contract_sha256 = value.as_str(field)?.to_string(),
                9 => out.source = Some(RecoverySourceEvidence::decode(value.as_bytes()?)?),
                10 => out.candidate_plan_id = value.as_str(field)?.to_string(),
                11 => out.candidate_plan_sha256 = value.as_str(field)?.to_string(),
                12 => out.candidate_profile_sha256 = value.as_str(field)?.to_string(),
                13 => out.candidate_artifact_sha256 = value.as_str(field)?.to_string(),
                _ => {}
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assessment_round_trips_source_control_and_exact_candidate_references() {
        let assessment = SupersedingRecoveryAssessment {
            job_id: "JOB-OLD".into(),
            eligible: true,
            source: Some(RecoverySourceEvidence {
                plan_id: "PLAN-OLD".into(),
                plan_sha256: "01".repeat(32),
                journal_head_sha256: "02".repeat(32),
                journal_sequence: 11,
                closed_control: Some(ClosedControlEvidence {
                    step_id: "STEP-1".into(),
                    permit_id: "PERMIT-OLD".into(),
                    request_id: "REQUEST-OLD-control".into(),
                    action: ManagedControlAction::EnterUpdater,
                    dispatch_record_sha256: "03".repeat(32),
                    deadline_epoch_ms: 120_100,
                    dispatched_at_epoch_ms: 100,
                    attempt_id: "ATTEMPT-1".into(),
                    controller_session_id: "CLI-SESSION-5".into(),
                }),
            }),
            candidate_plan_id: "PLAN-NEW".into(),
            candidate_plan_sha256: "04".repeat(32),
            candidate_profile_sha256: "05".repeat(32),
            candidate_artifact_sha256: "06".repeat(32),
            ..Default::default()
        };
        assert_eq!(
            SupersedingRecoveryAssessment::decode(&assessment.encode()).unwrap(),
            assessment
        );
        let mut future = assessment.encode();
        wire::write_string(&mut future, 99, "ignored additive field");
        assert_eq!(
            SupersedingRecoveryAssessment::decode(&future).unwrap(),
            assessment
        );
    }

    #[test]
    fn missing_or_unknown_control_action_never_defaults_to_enter_updater() {
        assert!(matches!(
            ClosedControlEvidence::decode(&[]),
            Err(WireError::MissingField { field: 4, .. })
        ));
        let mut unknown = Vec::new();
        wire::write_enum(&mut unknown, 4, 99);
        assert!(matches!(
            ClosedControlEvidence::decode(&unknown),
            Err(WireError::UnknownEnumValue {
                field: 4,
                value: 99
            })
        ));
    }

    #[test]
    fn legacy_assessment_has_no_source_or_candidate_authority() {
        let mut legacy = Vec::new();
        wire::write_string(&mut legacy, 1, "JOB-OLD");
        wire::write_bool(&mut legacy, 2, true);
        wire::write_string(&mut legacy, 5, "system");
        let decoded = SupersedingRecoveryAssessment::decode(&legacy).unwrap();
        assert!(decoded.source.is_none());
        assert!(decoded.candidate_plan_sha256.is_empty());
        assert_eq!(decoded.encode(), legacy);
    }
}
