//! The golden transcripts must stay what they claim to be.
//!
//! AF-V1 acceptance seeds the transcript library from the GJ-4 campaigns
//! ECAMP-96EFFF15 / ECAMP-31E041BC (architecture.md 16.5, 22 AF-V1). These
//! files are *derived* from published receipts, not captured from the wire, and
//! this suite is what keeps that distinction from eroding: every digest in them
//! is recomputed from its declared basis, so a hand-edited value that looked
//! like a capture would fail here.

use arkforge_core::digest::{sha256, Sha256Digest};
use arkforge_transport::replay::TranscriptTransport;
use arkforge_transport::transcript::{self, RecordKind, RecordStatus, Transcript, TranscriptProvenance};
use arkforge_transport::{IdentityEvidenceStrength, TypedDiscoveryFilter};

const ECAMP_A: &str = include_str!("../../../transcripts/dayu200-gj4-ecamp-96effff15.yaml");
const ECAMP_B: &str = include_str!("../../../transcripts/dayu200-gj4-ecamp-31e041bc.yaml");

/// The documented derivation rule, restated here so the test is the check and
/// not a copy of the generator.
fn derived(transcript_id: &str, field: &str, basis: &str) -> Sha256Digest {
    sha256(format!("arkforge/derived-transcript/v1|{transcript_id}|{field}|{basis}").as_bytes())
}

fn campaigns() -> Vec<Transcript> {
    vec![
        transcript::parse(ECAMP_A).expect("ECAMP-96EFFF15 must parse"),
        transcript::parse(ECAMP_B).expect("ECAMP-31E041BC must parse"),
    ]
}

#[test]
fn both_campaigns_carry_the_thirteen_step_receipt_chain() {
    for transcript in campaigns() {
        assert_eq!(
            transcript.records.len(),
            13,
            "{} must carry the published 13-step chain",
            transcript.id
        );
        let steps: Vec<&str> = transcript
            .records
            .iter()
            .filter_map(|record| record.semantic_value("step"))
            .collect();
        assert_eq!(
            steps,
            vec![
                "verify-image-bundle / hash-images",
                "flash intent confirmed by campaign reservation",
                "campaign reservation verified before first mutation",
                "enter-loader-mode",
                "wait-loader-disconnect",
                "wait-loader-reconnect",
                "rebind-loader-identity",
                "flash-partitions",
                "verify-flash-readback",
                "reboot-device",
                "wait-for-hdc",
                "rebind-and-verify-build",
                "capture-post-flash-diagnostics",
            ],
            "{}",
            transcript.id
        );
        assert!(transcript
            .records
            .iter()
            .all(|record| record.status == RecordStatus::Ok));
    }
}

#[test]
fn every_digest_is_reproducible_from_its_declared_basis() {
    for transcript in campaigns() {
        let id = transcript.id.as_str();
        for record in &transcript.records {
            if let (Some(action), Some(response)) = (&record.action, record.response_digest) {
                assert_eq!(
                    response,
                    derived(id, "response", action.as_str()),
                    "{id} record {} response digest is not derived from its action",
                    record.sequence
                );
            }
            if let Some(observation) = &record.observation {
                let mode = observation.mode.as_str();
                assert_eq!(
                    observation.topology_digest,
                    derived(id, "topology", mode),
                    "{id} record {} topology digest",
                    record.sequence
                );
                assert_eq!(
                    observation.descriptor_digest,
                    derived(id, "descriptor", mode),
                    "{id} record {} descriptor digest",
                    record.sequence
                );
                assert_eq!(
                    observation.serial_evidence.digest(),
                    Some(derived(id, "serial", mode)),
                    "{id} record {} serial digest",
                    record.sequence
                );
            }
        }
    }
}

#[test]
fn the_transcripts_do_not_claim_to_be_captures() {
    for transcript in campaigns() {
        assert_eq!(
            transcript.provenance,
            TranscriptProvenance::DerivedFromPublishedReceipts
        );
        assert!(
            !transcript.provenance.supports_protocol_claims(),
            "a derived transcript may never back a protocol claim"
        );
    }
}

#[test]
fn the_two_campaigns_are_distinct_recordings_of_the_same_chain() {
    let transcripts = campaigns();
    assert_ne!(transcripts[0].id, transcripts[1].id);
    // Same shape…
    assert_eq!(transcripts[0].records.len(), transcripts[1].records.len());
    // …different identities, so a test that accidentally replayed the wrong
    // campaign would not silently pass.
    assert_ne!(
        transcripts[0].digest().unwrap(),
        transcripts[1].digest().unwrap()
    );
}

#[test]
fn the_readback_step_records_a_typed_skip_not_a_verification() {
    // AD-006: on this board the nine writes land through `wlx` while the `rl`
    // read face stops early, so the readback step is a typed skip and the
    // boot-side build check is the authority.
    for transcript in campaigns() {
        let readback = transcript
            .records
            .iter()
            .find(|record| record.semantic_value("step") == Some("verify-flash-readback"))
            .expect("the chain includes a readback step");
        assert_eq!(readback.semantic_value("readDomain"), Some("windowed"));
        assert_eq!(
            readback.semantic_value("readback"),
            Some("skipped-lba-read-window")
        );
        assert_eq!(readback.semantic_value("verificationOutcome"), Some("typedSkip"));
        assert!(readback
            .semantic_value("readDomainDetail")
            .unwrap()
            .contains("blind past the window"));

        let build = transcript
            .records
            .iter()
            .find(|record| record.semantic_value("step") == Some("rebind-and-verify-build"))
            .expect("the chain includes the boot-side build check");
        assert_eq!(
            build.semantic_value("const.ohos.fullname"),
            Some("OpenHarmony-7.0.0.36")
        );
        assert_eq!(build.semantic_value("const.product.model"), Some("ohos"));
    }
}

#[test]
fn the_nine_writes_are_recorded_as_one_destructive_step_over_nine_partitions() {
    for transcript in campaigns() {
        let flash = transcript
            .records
            .iter()
            .find(|record| record.semantic_value("step") == Some("flash-partitions"))
            .expect("the chain includes the flash step");
        assert_eq!(flash.semantic_value("partitionCount"), Some("9"));
        assert_eq!(flash.semantic_value("writeFace"), Some("wlx"));
        assert_eq!(
            flash.semantic_value("dataImpact"),
            Some("userdata-overwritten")
        );
    }
}

#[test]
fn the_mode_transition_is_recorded_with_its_disconnect_and_reconnect() {
    for transcript in campaigns() {
        let kinds: Vec<RecordKind> = transcript.records.iter().map(|record| record.kind).collect();
        let detach = kinds
            .iter()
            .position(|kind| *kind == RecordKind::Detach)
            .expect("a mode transition disconnects");
        let attach = kinds
            .iter()
            .position(|kind| *kind == RecordKind::Attach)
            .expect("and reconnects");
        assert!(detach < attach, "the disconnect precedes the reconnect");
    }
}

#[test]
fn the_replay_transport_can_drive_a_read_only_pass_over_a_campaign() {
    let transcript = transcript::parse(ECAMP_A).unwrap();
    let transport = TranscriptTransport::new(transcript);

    let filter = TypedDiscoveryFilter {
        modes: vec![],
        provider_ids: vec![],
        minimum_identity_strength: Some(IdentityEvidenceStrength::ProtocolConfirmed),
    };
    let observations =
        arkforge_transport::DeviceTransport::discover(&transport, &filter, 0).unwrap();
    assert_eq!(
        observations.len(),
        1,
        "the campaign records one pre-mutation observation of the bound target"
    );
    assert_eq!(observations[0].mode.as_str(), "hdc-normal");

    let session =
        arkforge_transport::DeviceTransport::open_exact(&transport, &observations[0]).unwrap();
    assert!(!session.saw_detach());

    // The campaign recorded these actions; anything else is unsupported rather
    // than invented.
    for action in [
        "verify-image-bundle",
        "enter-loader-mode",
        "flash-partitions",
        "verify-flash-readback",
        "reboot-device",
    ] {
        assert!(transport.invocation(action, 0).is_ok(), "{action}");
    }
    assert!(transport.invocation("erase-partition", 0).is_err());
}
