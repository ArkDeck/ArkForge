//! # arkforge-transport
//!
//! Device observation, identity and rebind.
//!
//! architecture.md 11. Two rules shape everything here:
//!
//! - a typed USB request is constructed by a protocol module, never
//!   deserialized from IPC — there is no constructor that takes arbitrary setup
//!   packet bytes;
//! - a rebind never picks the first match. Zero candidates, several candidates,
//!   or a drop in identity strength all stop the operation.
//!
//! AF-V1 ships one transport: transcript replay. It exists so the read-only
//! vertical and the provider contract tests run without hardware, and it is
//! typed `ToolchainKind::Replay` so no plan built on it can be executable.

#![forbid(unsafe_code)]

pub mod replay;
pub mod transcript;

use arkforge_core::digest::{
    digest_canonical, CanonicalCbor, CborError, CborValue, Domain, Sha256Digest,
};
use arkforge_core::effect::DeviceMode;
use arkforge_core::ids::{ObservationId, OpaqueId};
use arkforge_core::profile::RebindTolerance;
use core::fmt;

/// How much the observation proves about *which* device this is.
///
/// `Ord` is strength order, so "identity must not weaken across a rebind" is a
/// `>=` (architecture.md 11.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IdentityEvidenceStrength {
    /// Only that something of the right shape is present.
    ClassOnly,
    /// A stable-looking serial, with no protocol confirmation.
    SerialAsserted,
    /// Serial plus topology: the device is on the port it was on.
    SerialAndTopology,
    /// The protocol itself answered with a device-unique identifier.
    ProtocolConfirmed,
}

impl IdentityEvidenceStrength {
    pub fn as_str(self) -> &'static str {
        match self {
            IdentityEvidenceStrength::ClassOnly => "classOnly",
            IdentityEvidenceStrength::SerialAsserted => "serialAsserted",
            IdentityEvidenceStrength::SerialAndTopology => "serialAndTopology",
            IdentityEvidenceStrength::ProtocolConfirmed => "protocolConfirmed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "classOnly" => Some(IdentityEvidenceStrength::ClassOnly),
            "serialAsserted" => Some(IdentityEvidenceStrength::SerialAsserted),
            "serialAndTopology" => Some(IdentityEvidenceStrength::SerialAndTopology),
            "protocolConfirmed" => Some(IdentityEvidenceStrength::ProtocolConfirmed),
            _ => None,
        }
    }
}

impl CanonicalCbor for IdentityEvidenceStrength {
    fn to_cbor(&self) -> CborValue {
        CborValue::text(self.as_str())
    }
}

/// What the device said its serial is, and how much that is worth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SerialEvidence {
    /// The device reports no serial at all — common in loader modes.
    Absent,
    /// A serial string was read from the descriptor.
    Descriptor { digest: Sha256Digest },
    /// A unique identifier came back from the protocol itself.
    ProtocolUnique { digest: Sha256Digest },
}

impl SerialEvidence {
    pub fn digest(&self) -> Option<Sha256Digest> {
        match self {
            SerialEvidence::Absent => None,
            SerialEvidence::Descriptor { digest } | SerialEvidence::ProtocolUnique { digest } => {
                Some(*digest)
            }
        }
    }
}

impl CanonicalCbor for SerialEvidence {
    fn to_cbor(&self) -> CborValue {
        match self {
            SerialEvidence::Absent => CborValue::map(vec![
                ("kind", CborValue::text("absent")),
                ("digest", CborValue::Null),
            ]),
            SerialEvidence::Descriptor { digest } => CborValue::map(vec![
                ("kind", CborValue::text("descriptor")),
                ("digest", digest.to_cbor()),
            ]),
            SerialEvidence::ProtocolUnique { digest } => CborValue::map(vec![
                ("kind", CborValue::text("protocolUnique")),
                ("digest", digest.to_cbor()),
            ]),
        }
    }
}

/// One typed fact the protocol answered with.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProtocolIdentityFact {
    pub key: OpaqueId,
    pub value: String,
}

impl CanonicalCbor for ProtocolIdentityFact {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("key", self.key.to_cbor()),
            ("value", CborValue::text(self.value.clone())),
        ])
    }
}

/// A provider that could plausibly drive this device.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProviderCandidateRef {
    pub provider_id: OpaqueId,
    pub confidence: OpaqueId,
}

impl CanonicalCbor for ProviderCandidateRef {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("providerId", self.provider_id.to_cbor()),
            ("confidence", self.confidence.to_cbor()),
        ])
    }
}

/// One observation of one device at one moment.
///
/// VID/PID may exist inside a Transport or a Profile, but never form a stable
/// target on their own (architecture.md 11.2), which is why this structure
/// carries digests of topology and descriptors rather than the raw numbers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceObservation {
    pub observation_id: ObservationId,
    pub observed_at_epoch_ms: u64,
    pub mode: DeviceMode,
    pub topology_digest: Sha256Digest,
    pub descriptor_digest: Sha256Digest,
    pub serial_evidence: SerialEvidence,
    pub protocol_identity: Vec<ProtocolIdentityFact>,
    pub provider_candidates: Vec<ProviderCandidateRef>,
    pub identity_strength: IdentityEvidenceStrength,
    /// True when the descriptor read back malformed. Inside a rebind tolerance
    /// window this is evidence of a transition in progress, not a fault
    /// (architecture.md 11.3, #1068).
    pub malformed_descriptor: bool,
}

impl DeviceObservation {
    pub fn facts_digest(&self) -> Result<Sha256Digest, CborError> {
        digest_canonical(Domain::DeviceFacts, self)
    }

    /// Whether this observation is stable enough to compare identities against.
    pub fn is_stable(&self) -> bool {
        !self.malformed_descriptor
    }
}

impl CanonicalCbor for DeviceObservation {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("observationId", self.observation_id.to_cbor()),
            (
                "observedAtEpochMs",
                CborValue::Unsigned(self.observed_at_epoch_ms),
            ),
            ("mode", self.mode.to_cbor()),
            ("topologyDigest", self.topology_digest.to_cbor()),
            ("descriptorDigest", self.descriptor_digest.to_cbor()),
            ("serialEvidence", self.serial_evidence.to_cbor()),
            (
                "protocolIdentity",
                CborValue::array(self.protocol_identity.iter().map(|f| f.to_cbor()).collect()),
            ),
            (
                "providerCandidates",
                CborValue::array(
                    self.provider_candidates
                        .iter()
                        .map(|c| c.to_cbor())
                        .collect(),
                ),
            ),
            ("identityStrength", self.identity_strength.to_cbor()),
            (
                "malformedDescriptor",
                CborValue::Bool(self.malformed_descriptor),
            ),
        ])
    }
}

/// A discovery filter. Typed, closed, and constructed here — an IPC peer
/// selects among these, it does not describe a USB request.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TypedDiscoveryFilter {
    pub modes: Vec<DeviceMode>,
    pub provider_ids: Vec<OpaqueId>,
    pub minimum_identity_strength: Option<IdentityEvidenceStrength>,
}

impl TypedDiscoveryFilter {
    pub fn accepts(&self, observation: &DeviceObservation) -> bool {
        if !self.modes.is_empty() && !self.modes.contains(&observation.mode) {
            return false;
        }
        if !self.provider_ids.is_empty()
            && !observation
                .provider_candidates
                .iter()
                .any(|candidate| self.provider_ids.contains(&candidate.provider_id))
        {
            return false;
        }
        if let Some(floor) = self.minimum_identity_strength {
            if observation.identity_strength < floor {
                return false;
            }
        }
        true
    }
}

/// What a rebind must produce for the operation to continue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebindExpectation {
    pub from_mode: DeviceMode,
    pub to_mode: DeviceMode,
    /// Mode names that count as `to_mode`. A Profile fact, passed in — the
    /// transport does not invent equivalences.
    pub to_mode_aliases: Vec<DeviceMode>,
    /// Digest over the identity set the authority admitted.
    pub allowed_identity_set_digest: Sha256Digest,
    pub serial_policy: SerialPolicy,
    pub topology_policy: TopologyPolicy,
    pub identity_strength_floor: IdentityEvidenceStrength,
    pub tolerance: RebindTolerance,
    pub deadline_epoch_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerialPolicy {
    /// The serial must be byte-identical across the transition.
    MustMatch,
    /// The serial legitimately changes across this transition, so it carries no
    /// identity weight here. DAYU200 changes its USB serial between Loader and
    /// HDC-normal.
    MayChange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopologyPolicy {
    /// The device must reappear at the same port.
    MustMatch,
    /// Any port is acceptable, e.g. after a physical re-plug the operator was
    /// told to perform.
    MayChange,
}

/// Why a rebind stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebindOutcome {
    Settled(Box<DeviceObservation>),
    NoCandidate,
    /// More than one device satisfies the expectation. Never pick one
    /// (architecture.md 11.3).
    Ambiguous { count: usize },
    IdentityWeakened {
        before: IdentityEvidenceStrength,
        after: IdentityEvidenceStrength,
    },
    SerialChanged,
    TopologyChanged,
    ExpectedModeNotReached { observed: Option<DeviceMode> },
    ToleranceWindowExhausted { transient_observations: usize },
}

impl RebindOutcome {
    pub fn settled(&self) -> Option<&DeviceObservation> {
        match self {
            RebindOutcome::Settled(observation) => Some(observation),
            _ => None,
        }
    }
}

/// Evaluates a stream of observations against a rebind expectation.
///
/// Separated from any transport so the decision can be tested against recorded
/// hardware behaviour, which is where the transient-tolerance rules came from.
pub fn evaluate_rebind(
    expectation: &RebindExpectation,
    previous: &DeviceObservation,
    observations: &[DeviceObservation],
) -> RebindOutcome {
    let mut transient = 0usize;
    let mut window_start: Option<u64> = None;

    for observation in observations {
        let elapsed = match window_start {
            None => {
                window_start = Some(observation.observed_at_epoch_ms);
                0
            }
            Some(start) => observation.observed_at_epoch_ms.saturating_sub(start),
        };

        // Transitional noise is evidence, not failure — until the window ends.
        if !observation.is_stable() {
            if expectation.tolerance.tolerate_transient_malformed
                && elapsed <= expectation.tolerance.tolerance_window_ms
            {
                transient += 1;
                continue;
            }
            return RebindOutcome::ToleranceWindowExhausted {
                transient_observations: transient,
            };
        }

        let mode_matches = observation.mode == expectation.to_mode
            || expectation.to_mode_aliases.contains(&observation.mode);
        if !mode_matches {
            if elapsed <= expectation.tolerance.tolerance_window_ms {
                transient += 1;
                continue;
            }
            return RebindOutcome::ExpectedModeNotReached {
                observed: Some(observation.mode.clone()),
            };
        }

        // A settled observation in the expected mode: now the identity rules
        // apply, and only between stable observations.
        if observation.identity_strength < expectation.identity_strength_floor
            || observation.identity_strength < previous.identity_strength
        {
            return RebindOutcome::IdentityWeakened {
                before: previous.identity_strength,
                after: observation.identity_strength,
            };
        }
        if expectation.serial_policy == SerialPolicy::MustMatch
            && observation.serial_evidence.digest() != previous.serial_evidence.digest()
        {
            return RebindOutcome::SerialChanged;
        }
        if expectation.topology_policy == TopologyPolicy::MustMatch
            && observation.topology_digest != previous.topology_digest
        {
            return RebindOutcome::TopologyChanged;
        }

        // Uniqueness: any other stable observation that also matches makes the
        // result ambiguous, and ambiguity is a stop.
        let matches = observations
            .iter()
            .filter(|other| {
                other.is_stable()
                    && (other.mode == expectation.to_mode
                        || expectation.to_mode_aliases.contains(&other.mode))
            })
            .filter(|other| other.descriptor_digest != observation.descriptor_digest)
            .count();
        if matches > 0 {
            return RebindOutcome::Ambiguous { count: matches + 1 };
        }

        return RebindOutcome::Settled(Box::new(observation.clone()));
    }

    if transient > 0 {
        RebindOutcome::ToleranceWindowExhausted {
            transient_observations: transient,
        }
    } else {
        RebindOutcome::NoCandidate
    }
}

/// A device transport.
///
/// Synchronous by decision (AFD-0002): AF-V1 is a read-only vertical with no
/// async runtime in the dependency set. The trait's shape matches
/// architecture.md 11.1 otherwise.
pub trait DeviceTransport: fmt::Debug + Send + Sync {
    fn transport_id(&self) -> &OpaqueId;

    fn discover(
        &self,
        filter: &TypedDiscoveryFilter,
        deadline_epoch_ms: u64,
    ) -> Result<Vec<DeviceObservation>, TransportError>;

    /// Opens exactly the device the observation names. There is no "open the
    /// first one" entry point.
    fn open_exact(
        &self,
        observation: &DeviceObservation,
    ) -> Result<Box<dyn TransportSession>, TransportError>;

    fn wait_for_rebind(
        &self,
        expectation: &RebindExpectation,
        previous: &DeviceObservation,
    ) -> Result<RebindOutcome, TransportError>;
}

/// An open session against one device.
///
/// The session digest is the continuity fact freshness rests on: while it is
/// unchanged and no detach was observed, the device under the handle is the
/// device the permit admitted (architecture.md 8.3).
pub trait TransportSession: fmt::Debug + Send {
    fn session_digest(&self) -> Sha256Digest;

    fn observation(&self) -> &DeviceObservation;

    /// Re-reads identity on the same open handle.
    fn reread_identity(&mut self) -> Result<DeviceObservation, TransportError>;

    /// Whether a detach or re-enumeration has been seen since the session
    /// opened.
    fn saw_detach(&self) -> bool;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    NoDevice,
    Ambiguous(usize),
    Closed,
    /// The transport was asked for something it does not implement — a replay
    /// transport asked to perform an action the transcript never recorded.
    Unsupported(String),
    Evidence(String),
    Cbor(CborError),
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportError::NoDevice => f.write_str("no device matched the typed filter"),
            TransportError::Ambiguous(count) => {
                write!(f, "{count} devices matched; refusing to choose")
            }
            TransportError::Closed => f.write_str("transport session is closed"),
            TransportError::Unsupported(detail) => write!(f, "unsupported by this transport: {detail}"),
            TransportError::Evidence(detail) => write!(f, "transport evidence problem: {detail}"),
            TransportError::Cbor(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for TransportError {}

#[cfg(test)]
mod tests {
    use super::*;
    use arkforge_core::digest::sha256;

    fn mode(name: &str) -> DeviceMode {
        DeviceMode::new(name).unwrap()
    }

    fn observation(
        id: &str,
        at: u64,
        mode_name: &str,
        strength: IdentityEvidenceStrength,
    ) -> DeviceObservation {
        DeviceObservation {
            observation_id: ObservationId::new(id).unwrap(),
            observed_at_epoch_ms: at,
            mode: mode(mode_name),
            topology_digest: sha256(b"port-1"),
            descriptor_digest: sha256(id.as_bytes()),
            serial_evidence: SerialEvidence::Descriptor {
                digest: sha256(b"serial-a"),
            },
            protocol_identity: vec![],
            provider_candidates: vec![],
            identity_strength: strength,
            malformed_descriptor: false,
        }
    }

    fn expectation() -> RebindExpectation {
        RebindExpectation {
            from_mode: mode("hdc-normal"),
            to_mode: mode("rockusb-loader"),
            to_mode_aliases: vec![mode("loader")],
            allowed_identity_set_digest: sha256(b"allowed"),
            serial_policy: SerialPolicy::MayChange,
            topology_policy: TopologyPolicy::MustMatch,
            identity_strength_floor: IdentityEvidenceStrength::SerialAndTopology,
            tolerance: RebindTolerance {
                require_disconnect: true,
                tolerance_window_ms: 30_000,
                tolerate_transient_malformed: true,
            },
            deadline_epoch_ms: 1_000_000,
        }
    }

    #[test]
    fn a_transient_malformed_observation_does_not_fail_the_rebind() {
        let previous = observation("OBS-0", 0, "hdc-normal", IdentityEvidenceStrength::SerialAndTopology);
        let mut malformed = observation("OBS-1", 1_000, "rockusb-loader", IdentityEvidenceStrength::ClassOnly);
        malformed.malformed_descriptor = true;
        let settled = observation(
            "OBS-2",
            5_000,
            "rockusb-loader",
            IdentityEvidenceStrength::SerialAndTopology,
        );
        let outcome = evaluate_rebind(&expectation(), &previous, &[malformed, settled]);
        assert!(outcome.settled().is_some(), "{outcome:?}");
    }

    #[test]
    fn a_declared_alias_counts_as_the_expected_mode() {
        let previous = observation("OBS-0", 0, "hdc-normal", IdentityEvidenceStrength::SerialAndTopology);
        let settled = observation("OBS-1", 500, "loader", IdentityEvidenceStrength::SerialAndTopology);
        let outcome = evaluate_rebind(&expectation(), &previous, &[settled]);
        assert!(outcome.settled().is_some(), "{outcome:?}");
    }

    #[test]
    fn two_matching_devices_are_ambiguous_rather_than_first_match() {
        let previous = observation("OBS-0", 0, "hdc-normal", IdentityEvidenceStrength::SerialAndTopology);
        let first = observation("OBS-1", 500, "rockusb-loader", IdentityEvidenceStrength::SerialAndTopology);
        let second = observation("OBS-2", 600, "rockusb-loader", IdentityEvidenceStrength::SerialAndTopology);
        let outcome = evaluate_rebind(&expectation(), &previous, &[first, second]);
        assert_eq!(outcome, RebindOutcome::Ambiguous { count: 2 });
    }

    #[test]
    fn a_weaker_identity_after_the_rebind_stops_the_operation() {
        let previous = observation(
            "OBS-0",
            0,
            "hdc-normal",
            IdentityEvidenceStrength::ProtocolConfirmed,
        );
        let settled = observation(
            "OBS-1",
            500,
            "rockusb-loader",
            IdentityEvidenceStrength::SerialAndTopology,
        );
        assert!(matches!(
            evaluate_rebind(&expectation(), &previous, &[settled]),
            RebindOutcome::IdentityWeakened { .. }
        ));
    }

    #[test]
    fn a_changed_serial_is_allowed_only_where_the_policy_says_so() {
        let previous = observation("OBS-0", 0, "hdc-normal", IdentityEvidenceStrength::SerialAndTopology);
        let mut settled = observation(
            "OBS-1",
            500,
            "rockusb-loader",
            IdentityEvidenceStrength::SerialAndTopology,
        );
        settled.serial_evidence = SerialEvidence::Descriptor {
            digest: sha256(b"serial-b"),
        };

        // DAYU200 changes its serial across this transition, so MayChange.
        assert!(evaluate_rebind(&expectation(), &previous, std::slice::from_ref(&settled))
            .settled()
            .is_some());

        let mut strict = expectation();
        strict.serial_policy = SerialPolicy::MustMatch;
        assert_eq!(
            evaluate_rebind(&strict, &previous, &[settled]),
            RebindOutcome::SerialChanged
        );
    }

    #[test]
    fn a_device_moved_to_another_port_is_refused_when_topology_must_match() {
        let previous = observation("OBS-0", 0, "hdc-normal", IdentityEvidenceStrength::SerialAndTopology);
        let mut settled = observation(
            "OBS-1",
            500,
            "rockusb-loader",
            IdentityEvidenceStrength::SerialAndTopology,
        );
        settled.topology_digest = sha256(b"port-2");
        assert_eq!(
            evaluate_rebind(&expectation(), &previous, &[settled]),
            RebindOutcome::TopologyChanged
        );
    }

    #[test]
    fn nothing_appearing_is_no_candidate_not_success() {
        let previous = observation("OBS-0", 0, "hdc-normal", IdentityEvidenceStrength::SerialAndTopology);
        assert_eq!(
            evaluate_rebind(&expectation(), &previous, &[]),
            RebindOutcome::NoCandidate
        );
    }

    #[test]
    fn transient_noise_past_the_window_exhausts_the_tolerance() {
        let previous = observation("OBS-0", 0, "hdc-normal", IdentityEvidenceStrength::SerialAndTopology);
        let mut early = observation("OBS-1", 1_000, "rockusb-loader", IdentityEvidenceStrength::ClassOnly);
        early.malformed_descriptor = true;
        let mut late = observation("OBS-2", 100_000, "rockusb-loader", IdentityEvidenceStrength::ClassOnly);
        late.malformed_descriptor = true;
        assert_eq!(
            evaluate_rebind(&expectation(), &previous, &[early, late]),
            RebindOutcome::ToleranceWindowExhausted {
                transient_observations: 1
            }
        );
    }

    #[test]
    fn a_filter_with_an_identity_floor_rejects_weak_observations() {
        let filter = TypedDiscoveryFilter {
            modes: vec![mode("rockusb-loader")],
            provider_ids: vec![],
            minimum_identity_strength: Some(IdentityEvidenceStrength::SerialAndTopology),
        };
        let weak = observation("OBS-1", 0, "rockusb-loader", IdentityEvidenceStrength::ClassOnly);
        let strong = observation(
            "OBS-2",
            0,
            "rockusb-loader",
            IdentityEvidenceStrength::ProtocolConfirmed,
        );
        assert!(!filter.accepts(&weak));
        assert!(filter.accepts(&strong));
    }
}
