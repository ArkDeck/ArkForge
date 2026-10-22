//! Synthetic failure transcripts replay as the failure they recorded.
//!
//! These are not hardware evidence. `TranscriptProvenance::Synthetic` cannot
//! support a protocol claim. They pin the replay rule from
//! `docs/spec/model/transcript.md`: an invocation returns its recorded status, and
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
    include_str!("../../../test/transcripts/synthetic/dayu200-mid-transfer-disconnect.yaml");
const LOADER_TIMEOUT: &str =
    include_str!("../../../test/transcripts/synthetic/dayu200-loader-switch-timeout.yaml");
const READBACK_MISMATCH: &str =
    include_str!("../../../test/transcripts/synthetic/dayu200-readback-verify-mismatch.yaml");
const LOADER_IDENTITY: &str =
    include_str!("../../../test/transcripts/synthetic/dayu200-loader-identity-change.yaml");
const POWER_INTERRUPTION: &str =
    include_str!("../../../test/transcripts/synthetic/dayu200-power-interruption-journal.yaml");

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

#[test]
fn a_readback_mismatch_is_not_a_verified_image() {
    let transcript = transcript::parse(READBACK_MISMATCH).expect("synthetic mismatch must parse");
    assert_eq!(transcript.provenance, TranscriptProvenance::Synthetic);
    assert!(!transcript.provenance.supports_protocol_claims());

    let transport = TranscriptTransport::new(transcript);
    let readback = transport
        .invocation("verify-flash-readback", 0)
        .expect("the mismatch was recorded");
    assert_eq!(readback.status, RecordStatus::Failed);
    assert_eq!(readback.semantic_value("partition"), Some("ramdisk"));
    assert_eq!(
        readback.semantic_value("verificationOutcome"),
        Some("failed")
    );
    assert_eq!(
        readback.semantic_value("expectedDigest"),
        Some("11111111111111111111111111111111111111111111111111111111111111c3")
    );
    assert_eq!(
        readback.semantic_value("observedDigest"),
        Some("22222222222222222222222222222222222222222222222222222222222222c4")
    );
    assert_ne!(
        readback.semantic_value("expectedDigest"),
        readback.semantic_value("observedDigest")
    );
    assert!(!transport.action_completed("verify-flash-readback", 0));
}

#[test]
fn a_loader_identity_change_keeps_both_observations() {
    let transcript =
        transcript::parse(LOADER_IDENTITY).expect("synthetic identity change must parse");
    assert_eq!(transcript.provenance, TranscriptProvenance::Synthetic);
    assert!(!transcript.provenance.supports_protocol_claims());

    let transport = TranscriptTransport::new(transcript);
    let found = transport
        .discover(&TypedDiscoveryFilter::default(), 0)
        .unwrap();
    assert_eq!(found.len(), 2);
    assert!(
        found
            .iter()
            .all(|observation| observation.mode.as_str() == "rockusb-loader")
    );
    assert_ne!(found[0].descriptor_digest, found[1].descriptor_digest);
    assert_ne!(found[0].topology_digest, found[1].topology_digest);
    assert_ne!(found[0].observation_id, found[1].observation_id);
    assert!(transport.open_exact(&found[0]).is_ok());
    assert!(transport.open_exact(&found[1]).is_ok());

    let check = transport
        .invocation("confirm-loader-identity", 0)
        .expect("the identity refusal was recorded");
    assert_eq!(check.status, RecordStatus::Failed);
    assert_eq!(check.semantic_value("identityChanged"), Some("true"));
    assert!(!transport.action_completed("confirm-loader-identity", 0));
}

#[test]
fn a_power_interruption_recovers_from_the_journal_without_a_retry() {
    let transcript =
        transcript::parse(POWER_INTERRUPTION).expect("synthetic power interruption must parse");
    assert_eq!(transcript.provenance, TranscriptProvenance::Synthetic);
    assert!(!transcript.provenance.supports_protocol_claims());

    let transport = TranscriptTransport::new(transcript);
    let write = transport
        .invocation("flash-partitions", 0)
        .expect("the interrupted write was recorded");
    assert_eq!(write.status, RecordStatus::Disconnected);
    assert_eq!(write.semantic_value("outcome"), Some("unknown"));
    assert!(!transport.action_completed("flash-partitions", 0));
    assert!(transport.invocation("flash-partitions", 1).is_err());

    let recovery = transport
        .invocation("recover-journal", 0)
        .expect("the journal recovery was recorded");
    assert_eq!(recovery.status, RecordStatus::Ok);
    assert_eq!(recovery.semantic_value("journalRecovery"), Some("stopped"));
    assert_eq!(recovery.semantic_value("replay"), Some("refused"));
    assert_eq!(
        recovery.semantic_value("interruptedAction"),
        Some("flash-partitions")
    );
    assert!(transport.action_completed("recover-journal", 0));
}
