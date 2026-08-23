//! StepAdmissionSnapshot digest body and the freshness verdict table.

use crate::cbor_repr::diag;
use crate::json::Json;
use crate::suites::case_id;
use crate::tree::{Case, Tree};
use arkforge_authority_api::{
    ContinuityBreak, CurrentFacts, FreshnessVerdict, StepAdmissionSnapshot, evaluate_freshness,
};
use arkforge_core::digest::{CanonicalCbor, Domain, decode_canonical, sha256};

const SUITE: &str = "admission";

fn snapshot(with_session: bool) -> StepAdmissionSnapshot {
    StepAdmissionSnapshot {
        captured_at_epoch_ms: 1_770_000_000_000,
        freshness_deadline_epoch_ms: 1_770_000_120_000,
        device_facts_digest: sha256(b"device-facts"),
        transport_session_digest: with_session.then(|| sha256(b"session")),
        provider_facts_digest: sha256(b"provider-facts"),
        toolchain_facts_digest: sha256(b"toolchain-facts"),
        artifact_facts_digest: sha256(b"artifact-facts"),
    }
}

fn current(snapshot: &StepAdmissionSnapshot, now: u64) -> CurrentFacts {
    CurrentFacts {
        now_epoch_ms: now,
        device_facts_digest: snapshot.device_facts_digest,
        transport_session_digest: snapshot.transport_session_digest,
        saw_detach_since_snapshot: false,
        provider_facts_digest: snapshot.provider_facts_digest,
        toolchain_facts_digest: snapshot.toolchain_facts_digest,
        artifact_facts_digest: snapshot.artifact_facts_digest,
    }
}

fn verdict_json(verdict: &FreshnessVerdict) -> Json {
    match verdict {
        FreshnessVerdict::Fresh => Json::object(vec![
            ("verdict", Json::str("fresh")),
            ("permitsDispatch", Json::Bool(true)),
            ("retryableWithoutDeviceBlame", Json::Bool(false)),
        ]),
        FreshnessVerdict::StaleSnapshot { elapsed_ms } => Json::object(vec![
            ("verdict", Json::str("staleSnapshot")),
            ("elapsedMs", Json::Unsigned(*elapsed_ms)),
            ("permitsDispatch", Json::Bool(false)),
            ("retryableWithoutDeviceBlame", Json::Bool(true)),
        ]),
        FreshnessVerdict::ContinuityBroken(reason) => Json::object(vec![
            ("verdict", Json::str("continuityBroken")),
            (
                "reason",
                Json::str(match reason {
                    ContinuityBreak::SessionChanged => "sessionChanged",
                    ContinuityBreak::DetachObserved => "detachObserved",
                    ContinuityBreak::DeviceFactsChanged => "deviceFactsChanged",
                    ContinuityBreak::ProviderFactsChanged => "providerFactsChanged",
                    ContinuityBreak::ToolchainFactsChanged => "toolchainFactsChanged",
                    ContinuityBreak::ArtifactFactsChanged => "artifactFactsChanged",
                }),
            ),
            ("permitsDispatch", Json::Bool(false)),
            ("retryableWithoutDeviceBlame", Json::Bool(false)),
        ]),
    }
}

pub fn populate(tree: &mut Tree) {
    let mut number = 0u32;

    for (with_session, title) in [
        (true, "snapshot with a transport session digest"),
        (
            false,
            "snapshot without a transport session (null, not absent)",
        ),
    ] {
        number += 1;
        let snapshot = snapshot(with_session);
        let body = snapshot.to_canonical_bytes().unwrap();
        let digest = snapshot.digest().unwrap();
        tree.case(
            &Case {
                id: case_id("ADMISSION", number),
                suite: SUITE,
                title: title.to_string(),
                requirements: vec!["AF-AUTH-001", "AF-AUTH-002"],
                kind: "digest",
                description: format!(
                    "digest = SHA-256({:?} || body). An absent transport session is \
                     encoded as CBOR null under its key; the key is never omitted.",
                    String::from_utf8_lossy(Domain::AdmissionSnapshot.as_bytes())
                ),
                input: Json::object(vec![
                    (
                        "capturedAtEpochMs",
                        Json::Unsigned(snapshot.captured_at_epoch_ms),
                    ),
                    (
                        "freshnessDeadlineEpochMs",
                        Json::Unsigned(snapshot.freshness_deadline_epoch_ms),
                    ),
                    ("deviceFactsDigest", Json::str("sha256(\"device-facts\")")),
                    (
                        "transportSessionDigest",
                        if with_session {
                            Json::str("sha256(\"session\")")
                        } else {
                            Json::Null
                        },
                    ),
                    (
                        "providerFactsDigest",
                        Json::str("sha256(\"provider-facts\")"),
                    ),
                    (
                        "toolchainFactsDigest",
                        Json::str("sha256(\"toolchain-facts\")"),
                    ),
                    (
                        "artifactFactsDigest",
                        Json::str("sha256(\"artifact-facts\")"),
                    ),
                ]),
                expected: Json::object(vec![
                    ("domain", Json::str("arkforge/v1/admission-snapshot\\0")),
                    ("bodySha256", Json::str(sha256(&body).to_hex())),
                    ("digest", Json::str(digest.to_hex())),
                    ("diag", Json::str(diag(&decode_canonical(&body).unwrap()))),
                ]),
            },
            vec![("body.cbor", body)],
        );
    }

    // Freshness: continuity first, wall clock as the backstop, in a fixed
    // order of precedence.
    let base = snapshot(true);
    let inside = base.freshness_deadline_epoch_ms - 1;
    struct FreshCase {
        title: &'static str,
        mutate: fn(&mut CurrentFacts),
        now: u64,
    }
    let cases: Vec<FreshCase> = vec![
        FreshCase {
            title: "continuity holds and the deadline has not passed: fresh",
            mutate: |_| {},
            now: inside,
        },
        FreshCase {
            title: "now == deadline is stale (boundary is exclusive)",
            mutate: |_| {},
            now: base.freshness_deadline_epoch_ms,
        },
        FreshCase {
            title: "deadline passed but continuity holds: stale snapshot, not a device fault",
            mutate: |_| {},
            now: base.freshness_deadline_epoch_ms + 5_000,
        },
        FreshCase {
            title: "a detach was observed: continuity broken (checked first)",
            mutate: |c| c.saw_detach_since_snapshot = true,
            now: inside,
        },
        FreshCase {
            title: "transport session digest changed",
            mutate: |c| c.transport_session_digest = Some(sha256(b"other session")),
            now: inside,
        },
        FreshCase {
            title: "transport session vanished (Some -> None) counts as changed",
            mutate: |c| c.transport_session_digest = None,
            now: inside,
        },
        FreshCase {
            title: "device facts digest changed",
            mutate: |c| c.device_facts_digest = sha256(b"other device"),
            now: inside,
        },
        FreshCase {
            title: "provider facts digest changed",
            mutate: |c| c.provider_facts_digest = sha256(b"other provider"),
            now: inside,
        },
        FreshCase {
            title: "toolchain facts digest changed",
            mutate: |c| c.toolchain_facts_digest = sha256(b"other toolchain"),
            now: inside,
        },
        FreshCase {
            title: "artifact facts digest changed",
            mutate: |c| c.artifact_facts_digest = sha256(b"other artifact"),
            now: inside,
        },
        FreshCase {
            title: "precedence: detach outranks a changed device digest",
            mutate: |c| {
                c.saw_detach_since_snapshot = true;
                c.device_facts_digest = sha256(b"other device");
            },
            now: inside,
        },
        FreshCase {
            title: "precedence: a broken continuity outranks an expired deadline",
            mutate: |c| c.device_facts_digest = sha256(b"other device"),
            now: base.freshness_deadline_epoch_ms + 5_000,
        },
        FreshCase {
            title: "precedence: session change outranks device facts change",
            mutate: |c| {
                c.transport_session_digest = Some(sha256(b"other session"));
                c.device_facts_digest = sha256(b"other device");
            },
            now: inside,
        },
    ];
    for case in cases {
        number += 1;
        let mut facts = current(&base, case.now);
        (case.mutate)(&mut facts);
        let verdict = evaluate_freshness(&base, &facts);
        tree.case(
            &Case {
                id: case_id("ADMISSION", number),
                suite: SUITE,
                title: case.title.to_string(),
                requirements: vec!["AF-AUTH-003", "AF-AUTH-004", "AF-AUTH-005"],
                kind: "derive",
                description: "evaluate_freshness(snapshot, currentFacts). Digests are \
                              compared for equality only; their preimages are labels here."
                    .to_string(),
                input: Json::object(vec![
                    ("snapshot", Json::str(case_id("ADMISSION", 1))),
                    (
                        "current",
                        Json::object(vec![
                            ("nowEpochMs", Json::Unsigned(facts.now_epoch_ms)),
                            (
                                "sawDetachSinceSnapshot",
                                Json::Bool(facts.saw_detach_since_snapshot),
                            ),
                            (
                                "transportSessionDigest",
                                match facts.transport_session_digest {
                                    Some(d) => Json::str(d.to_hex()),
                                    None => Json::Null,
                                },
                            ),
                            (
                                "deviceFactsDigest",
                                Json::str(facts.device_facts_digest.to_hex()),
                            ),
                            (
                                "providerFactsDigest",
                                Json::str(facts.provider_facts_digest.to_hex()),
                            ),
                            (
                                "toolchainFactsDigest",
                                Json::str(facts.toolchain_facts_digest.to_hex()),
                            ),
                            (
                                "artifactFactsDigest",
                                Json::str(facts.artifact_facts_digest.to_hex()),
                            ),
                        ]),
                    ),
                ]),
                expected: verdict_json(&verdict),
            },
            Vec::new(),
        );
    }
}
