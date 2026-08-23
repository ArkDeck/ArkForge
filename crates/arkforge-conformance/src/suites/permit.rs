//! StepPermit signing body, integrity tag and verification.
//!
//! Supersedes the digest-only table in
//! `docs/openspec/chg-arkdeck-arkforge-authority/permit-vectors.md`: the
//! complete signing-body bytes are published here, so a second implementation
//! that disagrees can diff bytes instead of running Rust.

use crate::cbor_repr::diag;
use crate::json::{Json, hex};
use crate::suites::case_id;
use crate::tree::{Case, Tree};
use arkforge_authority_api::authority_side::mint_integrity_tag;
use arkforge_authority_api::{
    ControllerPairingSecret, DispatchIntent, PairingEpoch, PermitDecodeError, PermitIntegrityTag,
    PermitVerificationError, StepPermit, verify_permit,
};
use arkforge_core::digest::{CborValue, decode_canonical, sha256};
use arkforge_core::ids::{
    AttemptId, ControllerSessionId, JobId, OpaqueId, PermitId, PlanId, StepId,
};
use arkforge_core::{AuthorityBindingRef, AuthorityNamespace};

const SUITE: &str = "permit";

/// The published vector secret (37 ASCII bytes, no terminator). Published by
/// definition; it exists only for these fixtures.
const SECRET: &[u8] = b"arkforge-arkdeck-permit-vector-secret";
const EPOCH: PairingEpoch = PairingEpoch(1);

struct Vector {
    step: &'static str,
    attempt: &'static str,
    private_action_preimage: &'static [u8],
}

const VECTORS: [Vector; 3] = [
    Vector {
        step: "STEP-ENSURE-MODE",
        attempt: "ATTEMPT-1",
        private_action_preimage: b"enter-loader",
    },
    Vector {
        step: "STEP-WRITE-SYSTEM",
        attempt: "ATTEMPT-1",
        private_action_preimage: b"write-partition:system",
    },
    Vector {
        step: "STEP-RESET",
        attempt: "ATTEMPT-2",
        private_action_preimage: b"reset-device",
    },
];

fn binding() -> AuthorityBindingRef {
    AuthorityBindingRef {
        authority_namespace: AuthorityNamespace::new("arkdeck").unwrap(),
        binding_id: OpaqueId::new("BINDING-VECTOR").unwrap(),
        binding_revision: 3,
        stable_identity_digest: sha256(b"stable-identity-vector"),
    }
}

fn permit(vector: &Vector) -> StepPermit {
    StepPermit {
        permit_id: PermitId::new(format!("PERMIT-{}", vector.step)).unwrap(),
        authority_namespace: AuthorityNamespace::new("arkdeck").unwrap(),
        controller_session_id: ControllerSessionId::new("SESSION-VECTOR").unwrap(),
        job_id: JobId::new("JOB-VECTOR").unwrap(),
        plan_id: PlanId::new("PLAN-VECTOR").unwrap(),
        plan_digest: sha256(b"plan-vector"),
        step_id: StepId::new(vector.step).unwrap(),
        attempt_id: AttemptId::new(vector.attempt).unwrap(),
        public_step_digest: sha256(b"public-step-vector"),
        private_action_digest: sha256(vector.private_action_preimage),
        effect_set_digest: sha256(b"effect-set-vector"),
        authority_binding: binding(),
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

fn minted(vector: &Vector) -> StepPermit {
    let secret = ControllerPairingSecret::new(EPOCH, SECRET.to_vec());
    let mut permit = permit(vector);
    permit.integrity_tag = mint_integrity_tag(&permit, &secret).unwrap();
    permit
}

fn intent_for(permit: &StepPermit, now_epoch_ms: u64) -> DispatchIntent {
    DispatchIntent {
        controller_session_id: permit.controller_session_id.clone(),
        job_id: permit.job_id.clone(),
        plan_id: permit.plan_id.clone(),
        plan_digest: permit.plan_digest,
        step_id: permit.step_id.clone(),
        attempt_id: permit.attempt_id.clone(),
        public_step_digest: permit.public_step_digest,
        private_action_digest: permit.private_action_digest,
        effect_set_digest: permit.effect_set_digest,
        authority_binding: permit.authority_binding.clone(),
        admitted_device_facts_digest: permit.admitted_device_facts_digest,
        now_epoch_ms,
    }
}

fn input_json(permit: &StepPermit, vector: &Vector) -> Json {
    Json::object(vec![
        (
            "pairingSecretAscii",
            Json::str(String::from_utf8_lossy(SECRET)),
        ),
        ("pairingEpoch", Json::Unsigned(EPOCH.0)),
        (
            "fields",
            Json::object(vec![
                ("permitId", Json::str(permit.permit_id.as_str())),
                ("authorityNamespace", Json::str("arkdeck")),
                ("controllerSessionId", Json::str("SESSION-VECTOR")),
                ("jobId", Json::str("JOB-VECTOR")),
                ("planId", Json::str("PLAN-VECTOR")),
                ("planDigest", Json::str("sha256(\"plan-vector\")")),
                ("stepId", Json::str(vector.step)),
                ("attemptId", Json::str(vector.attempt)),
                (
                    "publicStepDigest",
                    Json::str("sha256(\"public-step-vector\")"),
                ),
                (
                    "privateActionDigest",
                    Json::str(format!(
                        "sha256(\"{}\")",
                        String::from_utf8_lossy(vector.private_action_preimage)
                    )),
                ),
                (
                    "effectSetDigest",
                    Json::str("sha256(\"effect-set-vector\")"),
                ),
                (
                    "authorityBinding",
                    Json::object(vec![
                        ("authorityNamespace", Json::str("arkdeck")),
                        ("bindingId", Json::str("BINDING-VECTOR")),
                        ("bindingRevision", Json::Unsigned(3)),
                        (
                            "stableIdentityDigest",
                            Json::str("sha256(\"stable-identity-vector\")"),
                        ),
                    ]),
                ),
                (
                    "admittedDeviceFactsDigest",
                    Json::str("sha256(\"admitted-facts-vector\")"),
                ),
                ("issuedAtEpochMs", Json::Unsigned(permit.issued_at_epoch_ms)),
                (
                    "expiresAtEpochMs",
                    Json::Unsigned(permit.expires_at_epoch_ms),
                ),
                ("singleUse", Json::Bool(true)),
            ]),
        ),
        (
            "note",
            Json::str(
                "sha256(\"x\") means the SHA-256 of the ASCII bytes of x, carried as a \
                 32-byte CBOR byte string. The signing body does not contain the tag.",
            ),
        ),
    ])
}

fn verification_error_code(error: &PermitVerificationError) -> &'static str {
    match error {
        PermitVerificationError::IntegrityTagInvalid => "PERMIT_INTEGRITY_TAG_INVALID",
        PermitVerificationError::StalePairingEpoch { .. } => "PERMIT_STALE_PAIRING_EPOCH",
        PermitVerificationError::Expired { .. } => "PERMIT_EXPIRED",
        PermitVerificationError::ActionMismatch { .. } => "PERMIT_ACTION_MISMATCH",
        PermitVerificationError::PlanMismatch { .. } => "PERMIT_PLAN_MISMATCH",
        PermitVerificationError::ContextMismatch { .. } => "PERMIT_CONTEXT_MISMATCH",
        PermitVerificationError::AlreadyConsumed => "PERMIT_ALREADY_CONSUMED",
        PermitVerificationError::NotSingleUse => "PERMIT_NOT_SINGLE_USE",
        PermitVerificationError::Cbor(_) => "PERMIT_CBOR",
    }
}

fn decode_error_code(error: &PermitDecodeError) -> (&'static str, Option<String>) {
    match error {
        PermitDecodeError::NotAMap => ("PERMIT_DECODE_NOT_A_MAP", None),
        PermitDecodeError::Field(name) => ("PERMIT_DECODE_FIELD", Some((*name).to_string())),
        PermitDecodeError::NotCanonical => ("PERMIT_DECODE_NOT_CANONICAL", None),
        PermitDecodeError::Cbor(_) => ("PERMIT_DECODE_CBOR", None),
    }
}

type IntentMutation = fn(&mut DispatchIntent);

struct VerifyCase {
    title: &'static str,
    requirements: Vec<&'static str>,
    permit: StepPermit,
    secret_epoch: u64,
    intent: DispatchIntent,
    already_consumed: bool,
    mutation: String,
}

fn verify_cases(base: &StepPermit, secret: &ControllerPairingSecret) -> Vec<VerifyCase> {
    let now = 1_770_000_030_000u64; // inside the validity window
    let mut cases: Vec<VerifyCase> = Vec::new();
    let case = |title: &'static str,
                requirements: Vec<&'static str>,
                permit: StepPermit,
                secret_epoch: u64,
                intent: DispatchIntent,
                already_consumed: bool,
                mutation: &str| VerifyCase {
        title,
        requirements,
        permit,
        secret_epoch,
        intent,
        already_consumed,
        mutation: mutation.to_string(),
    };

    cases.push(case(
        "a freshly minted permit verifies for its own dispatch intent",
        vec!["AF-AUTH-013"],
        base.clone(),
        1,
        intent_for(base, now),
        false,
        "none",
    ));
    {
        let mut tampered = base.clone();
        tampered.issued_at_epoch_ms += 1;
        let intent = intent_for(&tampered, now);
        cases.push(case(
            "a signed field changed after minting: tag does not verify",
            vec!["AF-AUTH-013", "AF-AUTH-014"],
            tampered,
            1,
            intent,
            false,
            "issuedAtEpochMs + 1 without re-minting",
        ));
    }
    {
        let mut forged = base.clone();
        forged.integrity_tag.tag = sha256(b"not the tag");
        let intent = intent_for(&forged, now);
        cases.push(case(
            "wrong tag bytes",
            vec!["AF-AUTH-013"],
            forged,
            1,
            intent,
            false,
            "integrityTag replaced",
        ));
    }
    cases.push(case(
        "pairing epoch rotated after a restart: permit is void",
        vec!["AF-AUTH-015"],
        base.clone(),
        2,
        intent_for(base, now),
        false,
        "verifier secret epoch 2, permit epoch 1",
    ));
    cases.push(case(
        "now == expiresAtEpochMs is expired (boundary is exclusive)",
        vec!["AF-AUTH-016"],
        base.clone(),
        1,
        intent_for(base, base.expires_at_epoch_ms),
        false,
        "now = expiresAtEpochMs",
    ));
    cases.push(case(
        "now == expiresAtEpochMs - 1 is still valid",
        vec!["AF-AUTH-016"],
        base.clone(),
        1,
        intent_for(base, base.expires_at_epoch_ms - 1),
        false,
        "now = expiresAtEpochMs - 1",
    ));
    cases.push(case(
        "already consumed: return the original receipt, never dispatch",
        vec!["AF-AUTH-017"],
        base.clone(),
        1,
        intent_for(base, now),
        true,
        "ledger says consumed",
    ));
    {
        // A correctly minted permit with singleUse=false is refused even though
        // its tag verifies: the check is on the field, not the signature.
        let mut multi = permit(&VECTORS[0]);
        multi.single_use = false;
        multi.integrity_tag = mint_integrity_tag(&multi, secret).unwrap();
        let intent = intent_for(&multi, now);
        cases.push(case(
            "singleUse=false is refused even with a valid tag",
            vec!["AF-AUTH-018"],
            multi,
            1,
            intent,
            false,
            "singleUse=false, re-minted",
        ));
    }
    {
        let mut intent = intent_for(base, now);
        intent.plan_digest = sha256(b"another plan");
        cases.push(case(
            "plan digest of the pending dispatch differs",
            vec!["AF-AUTH-019"],
            base.clone(),
            1,
            intent,
            false,
            "intent.planDigest",
        ));
    }
    {
        let mut intent = intent_for(base, now);
        intent.private_action_digest = sha256(b"another action");
        cases.push(case(
            "private action digest of the pending dispatch differs",
            vec!["AF-AUTH-019"],
            base.clone(),
            1,
            intent,
            false,
            "intent.privateActionDigest",
        ));
    }
    let context_fields: [(&str, IntentMutation); 9] = [
        ("controllerSessionId", |i| {
            i.controller_session_id = ControllerSessionId::new("SESSION-OTHER").unwrap()
        }),
        ("jobId", |i| i.job_id = JobId::new("JOB-OTHER").unwrap()),
        ("planId", |i| i.plan_id = PlanId::new("PLAN-OTHER").unwrap()),
        ("stepId", |i| i.step_id = StepId::new("STEP-OTHER").unwrap()),
        ("attemptId", |i| {
            i.attempt_id = AttemptId::new("ATTEMPT-9").unwrap()
        }),
        ("publicStepDigest", |i| {
            i.public_step_digest = sha256(b"other")
        }),
        ("effectSetDigest", |i| {
            i.effect_set_digest = sha256(b"other")
        }),
        ("authorityBinding", |i| {
            i.authority_binding.binding_revision += 1
        }),
        ("admittedDeviceFactsDigest", |i| {
            i.admitted_device_facts_digest = sha256(b"other")
        }),
    ];
    for (field, mutate) in context_fields {
        let mut intent = intent_for(base, now);
        mutate(&mut intent);
        cases.push(case(
            "context mismatch: intent field differs",
            vec!["AF-AUTH-019"],
            base.clone(),
            1,
            intent,
            false,
            field,
        ));
    }
    {
        let mut intent = intent_for(base, now);
        intent.authority_binding.authority_namespace = AuthorityNamespace::new("other").unwrap();
        cases.push(case(
            "context mismatch: authority namespace differs",
            vec!["AF-AUTH-019"],
            base.clone(),
            1,
            intent,
            false,
            "authorityNamespace",
        ));
    }
    {
        // Order of checks: consumed outranks everything, then single-use, then
        // epoch, then tag. A consumed, expired, forged permit reports consumed.
        let mut forged = base.clone();
        forged.integrity_tag.tag = sha256(b"forged");
        let late = forged.expires_at_epoch_ms + 1;
        cases.push(case(
            "check order: already-consumed is reported before a bad tag or expiry",
            vec!["AF-AUTH-020"],
            forged.clone(),
            1,
            intent_for(&forged, late),
            true,
            "consumed + forged tag + expired",
        ));
        cases.push(case(
            "check order: stale epoch is reported before a bad tag",
            vec!["AF-AUTH-020"],
            forged.clone(),
            2,
            intent_for(&forged, now),
            false,
            "epoch 2 + forged tag",
        ));
        cases.push(case(
            "check order: a bad tag is reported before expiry",
            vec!["AF-AUTH-020"],
            forged.clone(),
            1,
            intent_for(&forged, late),
            false,
            "forged tag + expired",
        ));
    }
    cases
}

struct DecodeCase {
    title: &'static str,
    bytes: Vec<u8>,
    mutation: String,
}

fn decode_cases(good: &[u8]) -> Vec<DecodeCase> {
    let CborValue::Map(entries) = decode_canonical(good).unwrap() else {
        unreachable!("a permit body is a map")
    };
    let mut cases: Vec<DecodeCase> = vec![DecodeCase {
        title: "the published body decodes and re-encodes identically",
        bytes: good.to_vec(),
        mutation: "none".into(),
    }];
    for (key, _) in &entries {
        let CborValue::Text(name) = key else { continue };
        let without: Vec<(CborValue, CborValue)> =
            entries.iter().filter(|(k, _)| k != key).cloned().collect();
        cases.push(DecodeCase {
            title: "a missing field is a malformed permit, never a defaulted one",
            bytes: CborValue::Map(without).to_canonical_bytes().unwrap(),
            mutation: format!("remove {name}"),
        });
    }
    {
        // Same map, but one integer in non-shortest form: decodes as CBOR but
        // is not the deterministic encoding, so re-encoding differs.
        let mut bytes = good.to_vec();
        let key_bytes = CborValue::text("bindingRevision")
            .to_canonical_bytes()
            .unwrap();
        let position = bytes
            .windows(key_bytes.len())
            .position(|w| w == key_bytes.as_slice())
            .expect("bindingRevision key present");
        let value_at = position + key_bytes.len();
        assert_eq!(bytes[value_at], 0x03);
        bytes.splice(value_at..value_at + 1, [0x18, 0x03]);
        cases.push(DecodeCase {
            title: "non-shortest integer head inside the body is refused",
            bytes,
            mutation: "bindingRevision 03 -> 18 03".into(),
        });
    }
    cases.push(DecodeCase {
        title: "a body that is not a map is refused",
        bytes: CborValue::Array(vec![]).to_canonical_bytes().unwrap(),
        mutation: "empty array".into(),
    });
    let replace = |name: &str, value: CborValue| -> Vec<u8> {
        let mut edited = entries.clone();
        for (k, v) in edited.iter_mut() {
            if *k == CborValue::text(name) {
                *v = value.clone();
            }
        }
        CborValue::Map(edited).to_canonical_bytes().unwrap()
    };
    cases.push(DecodeCase {
        title: "a field of the wrong CBOR type is refused by name",
        bytes: replace("singleUse", CborValue::text("true")),
        mutation: "singleUse as text \"true\"".into(),
    });
    cases.push(DecodeCase {
        title: "a digest that is not exactly 32 bytes is refused by name",
        bytes: replace("planDigest", CborValue::Bytes(vec![0u8; 31])),
        mutation: "planDigest 31 bytes".into(),
    });
    cases.push(DecodeCase {
        title: "an identifier outside the OpaqueId grammar is refused by name",
        bytes: replace("jobId", CborValue::text("job id with spaces")),
        mutation: "jobId \"job id with spaces\"".into(),
    });
    {
        let mut extra = entries.clone();
        extra.push((CborValue::text("zzExtra"), CborValue::Unsigned(1)));
        cases.push(DecodeCase {
            title: "an unknown extra field changes the bytes, so the tag would not verify",
            bytes: CborValue::Map(extra).to_canonical_bytes().unwrap(),
            mutation: "add zzExtra=1".into(),
        });
    }
    cases
}

fn intent_json(intent: &DispatchIntent) -> Json {
    Json::object(vec![
        ("nowEpochMs", Json::Unsigned(intent.now_epoch_ms)),
        (
            "controllerSessionId",
            Json::str(intent.controller_session_id.as_str()),
        ),
        ("jobId", Json::str(intent.job_id.as_str())),
        ("planId", Json::str(intent.plan_id.as_str())),
        ("planDigest", Json::str(intent.plan_digest.to_hex())),
        ("stepId", Json::str(intent.step_id.as_str())),
        ("attemptId", Json::str(intent.attempt_id.as_str())),
        (
            "publicStepDigest",
            Json::str(intent.public_step_digest.to_hex()),
        ),
        (
            "privateActionDigest",
            Json::str(intent.private_action_digest.to_hex()),
        ),
        (
            "effectSetDigest",
            Json::str(intent.effect_set_digest.to_hex()),
        ),
        (
            "authorityBinding",
            Json::object(vec![
                (
                    "authorityNamespace",
                    Json::str(intent.authority_binding.authority_namespace.as_str()),
                ),
                (
                    "bindingId",
                    Json::str(intent.authority_binding.binding_id.as_str()),
                ),
                (
                    "bindingRevision",
                    Json::Unsigned(intent.authority_binding.binding_revision),
                ),
                (
                    "stableIdentityDigest",
                    Json::str(intent.authority_binding.stable_identity_digest.to_hex()),
                ),
            ]),
        ),
        (
            "admittedDeviceFactsDigest",
            Json::str(intent.admitted_device_facts_digest.to_hex()),
        ),
    ])
}

pub fn populate(tree: &mut Tree) {
    let mut number = 0u32;

    // ---- the three published vectors, now with full bytes -----------------
    for vector in &VECTORS {
        number += 1;
        let permit = minted(vector);
        let body = permit.signing_body().unwrap();
        let decoded = decode_canonical(&body).unwrap();
        let reparsed = StepPermit::from_canonical_bytes(&body).unwrap();
        assert_eq!(reparsed.signing_body().unwrap(), body);

        tree.case(
            &Case {
                id: case_id("PERMIT", number),
                suite: SUITE,
                title: format!("signing body and tag for {}", vector.step),
                requirements: vec!["AF-AUTH-010", "AF-AUTH-011", "AF-AUTH-012"],
                kind: "encode",
                description: "Build the signing body (deterministic CBOR map, no tag), \
                              then tag = HMAC-SHA-256(pairingSecret, body). Decoding \
                              `signing-body.cbor` and re-encoding MUST reproduce it byte \
                              for byte."
                    .to_string(),
                input: input_json(&permit, vector),
                expected: Json::object(vec![
                    ("signingBodySha256", Json::str(sha256(&body).to_hex())),
                    ("signingBodyLength", Json::Unsigned(body.len() as u64)),
                    ("integrityTag", Json::str(permit.integrity_tag.tag.to_hex())),
                    (
                        "integrityTagEpoch",
                        Json::Unsigned(permit.integrity_tag.epoch.0),
                    ),
                    ("diag", Json::str(diag(&decoded))),
                ]),
            },
            vec![
                ("signing-body.cbor", body),
                (
                    "integrity-tag.hex",
                    format!("{}\n", permit.integrity_tag.tag.to_hex()).into_bytes(),
                ),
            ],
        );
    }

    // ---- verification outcomes ------------------------------------------
    let secret = ControllerPairingSecret::new(EPOCH, SECRET.to_vec());
    let base = minted(&VECTORS[0]);
    for case in verify_cases(&base, &secret) {
        number += 1;
        let verifier =
            ControllerPairingSecret::new(PairingEpoch(case.secret_epoch), SECRET.to_vec());
        let outcome = verify_permit(&case.permit, &verifier, &case.intent, case.already_consumed);
        let expected = match &outcome {
            Ok(_) => Json::object(vec![("result", Json::str("accept"))]),
            Err(error) => {
                let mut obj = Json::object(vec![
                    ("result", Json::str("reject")),
                    ("code", Json::str(verification_error_code(error))),
                ]);
                if let PermitVerificationError::ContextMismatch { field } = error {
                    obj.push("field", Json::str(*field));
                }
                obj
            }
        };
        let body = case.permit.signing_body().unwrap();
        tree.case(
            &Case {
                id: case_id("PERMIT", number),
                suite: SUITE,
                title: case.title.to_string(),
                requirements: case.requirements.clone(),
                kind: "verify",
                description: "verify_permit(permit, secret, intent, alreadyConsumed). The \
                              permit's signing body is `signing-body.cbor`; the tag and \
                              epoch are in `input`. The intent is the dispatch about to \
                              happen."
                    .to_string(),
                input: Json::object(vec![
                    ("basedOn", Json::str(case_id("PERMIT", 1))),
                    ("mutation", Json::str(&case.mutation)),
                    (
                        "integrityTag",
                        Json::str(case.permit.integrity_tag.tag.to_hex()),
                    ),
                    (
                        "permitEpoch",
                        Json::Unsigned(case.permit.integrity_tag.epoch.0),
                    ),
                    (
                        "verifierSecretAscii",
                        Json::str(String::from_utf8_lossy(SECRET)),
                    ),
                    ("verifierEpoch", Json::Unsigned(case.secret_epoch)),
                    ("alreadyConsumed", Json::Bool(case.already_consumed)),
                    ("intent", intent_json(&case.intent)),
                ]),
                expected,
            },
            vec![("signing-body.cbor", body)],
        );
    }

    // ---- decoding from bytes ----------------------------------------------
    let good = base.signing_body().unwrap();
    for case in decode_cases(&good) {
        number += 1;
        let outcome = StepPermit::from_canonical_bytes(&case.bytes);
        let expected = match &outcome {
            Ok(parsed) => {
                let reencoded = parsed.signing_body().unwrap();
                Json::object(vec![
                    ("result", Json::str("accept")),
                    ("reencodesIdentically", Json::Bool(reencoded == case.bytes)),
                ])
            }
            Err(error) => {
                let (code, field) = decode_error_code(error);
                let mut obj = Json::object(vec![
                    ("result", Json::str("reject")),
                    ("code", Json::str(code)),
                ]);
                if let Some(field) = field {
                    obj.push("field", Json::str(field));
                }
                obj
            }
        };
        tree.case(
            &Case {
                id: case_id("PERMIT", number),
                suite: SUITE,
                title: case.title.to_string(),
                requirements: vec!["AF-AUTH-021", "AF-AUTH-022"],
                kind: "decode",
                description: "StepPermit::from_canonical_bytes(bytes). Every field is \
                              required and typed; the decoded value re-encoded MUST equal \
                              the input bytes, otherwise the bytes are not what a tag \
                              could have covered."
                    .to_string(),
                input: Json::object(vec![
                    ("basedOn", Json::str(case_id("PERMIT", 1))),
                    ("mutation", Json::str(&case.mutation)),
                    ("hex", Json::str(hex(&case.bytes))),
                ]),
                expected,
            },
            vec![("body.cbor", case.bytes)],
        );
    }
}
