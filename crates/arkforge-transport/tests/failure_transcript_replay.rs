//! Synthetic failure transcripts replay as the failure they recorded.
//!
//! These are not hardware evidence. `TranscriptProvenance::Synthetic` cannot
//! support a protocol claim. They pin the replay rule from
//! `spec/model/transcript.md`: an invocation returns its recorded status, and
//! a timeout with no later attach does not settle as a rebind.

use arkforge_core::digest::sha256;
use arkforge_core::effect::DeviceMode;
use arkforge_core::profile::RebindTolerance;
use arkforge_transport::replay::TranscriptTransport;
use arkforge_transport::transcript::{self, RecordKind, RecordStatus, TranscriptProvenance};
use arkforge_transport::{
    DeviceTransport, IdentityEvidenceStrength, RebindExpectation, RebindOutcome, SerialPolicy,
    TopologyPolicy, TypedDiscoveryFilter,
};

const MID_TRANSFER: &str =
    include_str!("../../../transcripts/synthetic/dayu200-mid-transfer-disconnect.yaml");
const LOADER_TIMEOUT: &str =
    include_str!("../../../transcripts/synthetic/dayu200-loader-switch-timeout.yaml");

fn loader_expectation() -> RebindExpectation {
    RebindExpectation {
        from_mode: DeviceMode::new("hdc-normal").unwrap(),
        to_mode: DeviceMode::new("rockusb-loader").unwrap(),
        to_mode_aliases: vec![DeviceMode::new("loader").unwrap()],
        allowed_identity_set_digest: sha256(b"synthetic-loader-switch"),
        serial_policy: SerialPolicy::MayChange,
        topology_policy: TopologyPolicy::MayChange,
        identity_strength_floor: IdentityEvidenceStrength::SerialAndTopology,
        tolerance: RebindTolerance {
            require_disconnect: true,
            tolerance_window_ms: 30_000,
            tolerate_transient_malformed: true,
        },
        deadline_epoch_ms: 60_000,
    }
}

#[test]
fn a_mid_transfer_disconnect_is_not_a_completed_write() {
    let transcript = transcript::parse(MID_TRANSFER).expect("synthetic disconnect must parse");
    assert_eq!(transcript.provenance, TranscriptProvenance::Synthetic);
    assert!(!transcript.provenance.supports_protocol_claims());

    let transport = TranscriptTransport::new(transcript);
    let record = transport
        .invocation("flash-partitions", 0)
        .expect("the disconnect was recorded");
    assert_eq!(record.status, RecordStatus::Disconnected);
    assert_eq!(record.request_bytes, Some(8_388_608));
    assert_eq!(record.semantic_value("transferredBytes"), Some("1048576"));
    assert_eq!(record.semantic_value("declaredBytes"), Some("8388608"));
    assert!(!transport.action_completed("flash-partitions", 0));
    assert!(!transport.action_completed("flash-partitions", 1));

    let found = transport
        .discover(&TypedDiscoveryFilter::default(), 0)
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].mode.as_str(), "rockusb-loader");
    assert!(transport.open_exact(&found[0]).is_ok());
}

#[test]
fn a_loader_switch_timeout_does_not_settle_a_rebind() {
    let transcript = transcript::parse(LOADER_TIMEOUT).expect("synthetic timeout must parse");
    assert_eq!(transcript.provenance, TranscriptProvenance::Synthetic);
    assert!(!transcript.provenance.supports_protocol_claims());
    assert!(
        transcript
            .records
            .iter()
            .any(|record| record.kind == RecordKind::Detach && record.status == RecordStatus::Ok)
    );
    assert!(
        transcript
            .records
            .iter()
            .all(|record| record.kind != RecordKind::Attach && record.kind != RecordKind::Rebind)
    );

    let transport = TranscriptTransport::new(transcript);
    let timed_out = transport
        .invocation("wait-loader-reconnect", 0)
        .expect("the timeout was recorded");
    assert_eq!(timed_out.status, RecordStatus::Timeout);
    assert_eq!(timed_out.duration_ms, 30_000);
    assert!(transport.action_completed("enter-loader-mode", 0));
    assert!(!transport.action_completed("wait-loader-reconnect", 0));

    let previous = transport
        .discover(&TypedDiscoveryFilter::default(), 0)
        .unwrap()
        .into_iter()
        .find(|observation| observation.mode.as_str() == "hdc-normal")
        .expect("the pre-switch observation is stable");
    let outcome = transport
        .wait_for_rebind(&loader_expectation(), &previous)
        .unwrap();
    assert_eq!(outcome, RebindOutcome::NoCandidate);
}
