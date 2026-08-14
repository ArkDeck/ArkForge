//! Cross-implementation vectors for the StepPermit signing body and tag.
//!
//! A second implementation of the permit — ArkDeck's, in Swift — has to produce
//! byte-identical canonical CBOR and the same HMAC-SHA256 tag, or the two sides
//! will disagree about which permits are valid without either being obviously
//! wrong. These vectors are what makes that checkable rather than hoped for.
//!
//! The published copy lives at
//! `docs/openspec/chg-arkdeck-arkforge-authority/permit-vectors.md`, and this
//! test is what keeps it honest: change the encoding and this fails.

use arkforge_authority_api::authority_side::mint_integrity_tag;
use arkforge_authority_api::{
    ControllerPairingSecret, PairingEpoch, PermitIntegrityTag, StepPermit,
};
use arkforge_core::digest::sha256;
use arkforge_core::ids::{AttemptId, ControllerSessionId, JobId, OpaqueId, PermitId, PlanId, StepId};
use arkforge_core::{AuthorityBindingRef, AuthorityNamespace};

/// The secret every vector uses. A published test vector's key is published by
/// definition; this one exists only in this file and in the document it backs.
const SECRET: &[u8] = b"arkforge-arkdeck-permit-vector-secret";
const EPOCH: PairingEpoch = PairingEpoch(1);

fn permit(step: &str, attempt: &str, private_action: &[u8]) -> StepPermit {
    StepPermit {
        permit_id: PermitId::new(format!("PERMIT-{step}")).unwrap(),
        authority_namespace: AuthorityNamespace::new("arkdeck").unwrap(),
        controller_session_id: ControllerSessionId::new("SESSION-VECTOR").unwrap(),
        job_id: JobId::new("JOB-VECTOR").unwrap(),
        plan_id: PlanId::new("PLAN-VECTOR").unwrap(),
        plan_digest: sha256(b"plan-vector"),
        step_id: StepId::new(step).unwrap(),
        attempt_id: AttemptId::new(attempt).unwrap(),
        public_step_digest: sha256(b"public-step-vector"),
        private_action_digest: sha256(private_action),
        effect_set_digest: sha256(b"effect-set-vector"),
        authority_binding: AuthorityBindingRef {
            authority_namespace: AuthorityNamespace::new("arkdeck").unwrap(),
            binding_id: OpaqueId::new("BINDING-VECTOR").unwrap(),
            binding_revision: 3,
            stable_identity_digest: sha256(b"stable-identity-vector"),
        },
        admitted_device_facts_digest: sha256(b"admitted-facts-vector"),
        issued_at_epoch_ms: 1_770_000_000_000,
        expires_at_epoch_ms: 1_770_000_060_000,
        single_use: true,
        integrity_tag: PermitIntegrityTag {
            epoch: EPOCH,
            tag: sha256(b""),
        },
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// `(step, attempt, private action preimage, body sha256, tag)`.
///
/// The body itself is long, so its SHA-256 is pinned rather than its bytes; a
/// Swift implementation that produces a different encoding produces a different
/// body digest, which is the thing being detected.
const VECTORS: [(&str, &str, &[u8], &str, &str); 3] = [
    (
        "STEP-ENSURE-MODE",
        "ATTEMPT-1",
        b"enter-loader",
        "bae9c1e8d669e6850eb967524885bed0632b6adbb9de5fb3bea971250fb5cd51",
        "d0a4dbc07944f6a802a4f157574f89ddc1cca5f9eb89c7b5c26d99884ea37ae0",
    ),
    (
        "STEP-WRITE-SYSTEM",
        "ATTEMPT-1",
        b"write-partition:system",
        "fbdfcab7a865c5ae6400ab64594c1780e71a06a47da43ab6232674e1cdaa2d2e",
        "db38ba9d9a8fbac7840a89b6a9434938b25ed18bc357b4f27e39751d21be1523",
    ),
    (
        "STEP-RESET",
        "ATTEMPT-2",
        b"reset-device",
        "cea82597e94d8a47092ef11c7ff91af63e6d9a890292f5407eeeab60960d65f8",
        "86805a7585615edaed931ac3ac005e445529ba5de7495292d6fae10e2d9029ec",
    ),
];

#[test]
fn the_published_permit_vectors_still_hold() {
    let secret = ControllerPairingSecret::new(EPOCH, SECRET.to_vec());
    let mut observed = Vec::new();
    for (step, attempt, preimage, _, _) in VECTORS {
        let mut permit = permit(step, attempt, preimage);
        permit.integrity_tag = mint_integrity_tag(&permit, &secret).unwrap();
        let body = permit.signing_body().unwrap();
        observed.push((
            step,
            hex(sha256(&body).as_bytes()),
            hex(permit.integrity_tag.tag.as_bytes()),
        ));
    }

    let expected: Vec<(&str, String, String)> = VECTORS
        .iter()
        .map(|(step, _, _, body, tag)| (*step, body.to_string(), tag.to_string()))
        .collect();

    assert_eq!(
        observed, expected,
        "\nthe permit encoding changed. Regenerate \
         docs/openspec/chg-arkdeck-arkforge-authority/permit-vectors.md from the \
         observed column, and treat it as a breaking change for any second \
         implementation.\n"
    );
}
