//! Transcript-replay transport.
//!
//! Lets the read-only vertical and the provider contract tests run the whole
//! path — discover, open, re-read identity, rebind — with no device attached.
//!
//! This transport is not a simulator of hardware behaviour. It replays what a
//! transcript recorded and refuses anything the transcript does not contain, so
//! a test cannot accidentally assert against behaviour nobody observed.
//! Toolchains built on it are `ToolchainKind::Replay`, which
//! `MaturityState::permits_executable_plan` never accepts.

use crate::transcript::{RecordKind, RecordStatus, Transcript, TranscriptRecord};
use crate::{
    evaluate_rebind, DeviceObservation, DeviceTransport, RebindExpectation, RebindOutcome,
    TransportError, TransportSession, TypedDiscoveryFilter,
};
use arkforge_core::digest::{sha256, Sha256Digest};
use arkforge_core::ids::OpaqueId;

/// A transport backed by a recorded transcript.
#[derive(Debug)]
pub struct TranscriptTransport {
    id: OpaqueId,
    transcript: Transcript,
}

impl TranscriptTransport {
    pub fn new(transcript: Transcript) -> Self {
        TranscriptTransport {
            id: OpaqueId::new("arkforge.transport.replay").expect("literal identifier"),
            transcript,
        }
    }

    pub fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    /// The recorded result of one semantic action.
    ///
    /// `occurrence` is 0-based: a plan that writes nine partitions replays nine
    /// distinct records rather than the same one nine times.
    pub fn invocation(
        &self,
        action: &str,
        occurrence: usize,
    ) -> Result<&TranscriptRecord, TransportError> {
        self.transcript
            .invocations(action)
            .nth(occurrence)
            .ok_or_else(|| {
                TransportError::Unsupported(format!(
                    "transcript {} has no occurrence {occurrence} of action {action}",
                    self.transcript.id
                ))
            })
    }

    /// Stable observations, in recorded order.
    fn stable_observations(&self) -> Vec<DeviceObservation> {
        self.transcript
            .records
            .iter()
            .filter(|record| record.kind == RecordKind::Observation)
            .filter(|record| record.status == RecordStatus::Ok)
            .filter_map(|record| record.observation.clone())
            .collect()
    }

    fn rebind_observations(&self) -> Vec<DeviceObservation> {
        self.transcript
            .records
            .iter()
            .filter(|record| matches!(record.kind, RecordKind::Rebind | RecordKind::Attach))
            .filter_map(|record| record.observation.clone())
            .collect()
    }
}

impl DeviceTransport for TranscriptTransport {
    fn transport_id(&self) -> &OpaqueId {
        &self.id
    }

    fn discover(
        &self,
        filter: &TypedDiscoveryFilter,
        _deadline_epoch_ms: u64,
    ) -> Result<Vec<DeviceObservation>, TransportError> {
        Ok(self
            .stable_observations()
            .into_iter()
            .filter(|observation| filter.accepts(observation))
            .collect())
    }

    fn open_exact(
        &self,
        observation: &DeviceObservation,
    ) -> Result<Box<dyn TransportSession>, TransportError> {
        let known = self
            .stable_observations()
            .into_iter()
            .find(|candidate| candidate.descriptor_digest == observation.descriptor_digest);
        match known {
            Some(observation) => Ok(Box::new(ReplaySession {
                session_digest: sha256(
                    format!("replay-session:{}", observation.descriptor_digest).as_bytes(),
                ),
                observation,
                detached: false,
            })),
            None => Err(TransportError::NoDevice),
        }
    }

    fn wait_for_rebind(
        &self,
        expectation: &RebindExpectation,
        previous: &DeviceObservation,
    ) -> Result<RebindOutcome, TransportError> {
        let observations = self.rebind_observations();
        Ok(evaluate_rebind(expectation, previous, &observations))
    }
}

#[derive(Debug)]
struct ReplaySession {
    session_digest: Sha256Digest,
    observation: DeviceObservation,
    detached: bool,
}

impl TransportSession for ReplaySession {
    fn session_digest(&self) -> Sha256Digest {
        self.session_digest
    }

    fn observation(&self) -> &DeviceObservation {
        &self.observation
    }

    fn reread_identity(&mut self) -> Result<DeviceObservation, TransportError> {
        if self.detached {
            return Err(TransportError::Closed);
        }
        Ok(self.observation.clone())
    }

    fn saw_detach(&self) -> bool {
        self.detached
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript;
    use crate::IdentityEvidenceStrength;
    use arkforge_core::effect::DeviceMode;

    const SAMPLE: &str = r#"
schemaVersion: arkforge.transcript/v1
transcript:
  id: TEST-REPLAY
  provenance: synthetic
  source: "replay unit test"
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
    status: ok
    action: read-product-facts
    semantic:
      - key: const.product.model
        value: ohos
"#;

    fn transport() -> TranscriptTransport {
        TranscriptTransport::new(transcript::parse(SAMPLE).unwrap())
    }

    #[test]
    fn discovers_the_recorded_observation() {
        let transport = transport();
        let filter = TypedDiscoveryFilter {
            modes: vec![DeviceMode::new("hdc-normal").unwrap()],
            provider_ids: vec![],
            minimum_identity_strength: Some(IdentityEvidenceStrength::SerialAndTopology),
        };
        let found = transport.discover(&filter, 0).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].observation_id.as_str(), "OBS-1");
    }

    #[test]
    fn opening_a_device_the_transcript_does_not_contain_fails() {
        let transport = transport();
        let mut phantom = transport.discover(&TypedDiscoveryFilter::default(), 0).unwrap()[0].clone();
        phantom.descriptor_digest = sha256(b"a device that was never observed");
        assert_eq!(
            transport.open_exact(&phantom).unwrap_err(),
            TransportError::NoDevice
        );
    }

    #[test]
    fn an_action_the_transcript_never_recorded_is_unsupported_not_invented() {
        let transport = transport();
        assert!(matches!(
            transport.invocation("write-partition", 0),
            Err(TransportError::Unsupported(_))
        ));
        assert!(transport.invocation("read-product-facts", 0).is_ok());
        // Only one occurrence was recorded.
        assert!(transport.invocation("read-product-facts", 1).is_err());
    }

    #[test]
    fn the_session_digest_is_stable_for_one_device() {
        let transport = transport();
        let observation = transport.discover(&TypedDiscoveryFilter::default(), 0).unwrap()[0].clone();
        let first = transport.open_exact(&observation).unwrap().session_digest();
        let second = transport.open_exact(&observation).unwrap().session_digest();
        assert_eq!(first, second);
    }
}
