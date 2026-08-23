//! Possible-effect-set identity and the four read-only reconcile verdicts.

use crate::cbor_repr::diag;
use crate::json::Json;
use crate::suites::case_id;
use crate::tree::{Case, Tree};
use arkforge_authority_api::{EffectSetCompleteness, PossibleEffectSet};
use arkforge_core::digest::{CanonicalCbor, Domain, decode_canonical, sha256};
use arkforge_core::effect::{ByteRange, DataImpact, DataImpactState, EffectSet, PersistentEffect};
use arkforge_core::ids::{ActionId, PartitionId};
use arkforge_engine::superseding::{EffectObservation, ReconcileVerdict, reconcile};

const SUITE: &str = "reconcile";

fn effect(partition: &str, start: u64) -> PersistentEffect {
    PersistentEffect::WritePartition {
        partition: PartitionId::new(partition).unwrap(),
        range: ByteRange::new(start, 4096).unwrap(),
        content: sha256(format!("content:{partition}").as_bytes()),
    }
}

fn verdict_json(verdict: ReconcileVerdict) -> Json {
    match verdict {
        ReconcileVerdict::Succeeded => Json::object(vec![("verdict", Json::str("succeeded"))]),
        ReconcileVerdict::ConfirmedNotExecuted => {
            Json::object(vec![("verdict", Json::str("confirmedNotExecuted"))])
        }
        ReconcileVerdict::ConfirmedPartial { present, absent } => Json::object(vec![
            ("verdict", Json::str("confirmedPartial")),
            ("present", Json::Unsigned(present as u64)),
            ("absent", Json::Unsigned(absent as u64)),
        ]),
        ReconcileVerdict::StillUnknown { reason } => Json::object(vec![
            ("verdict", Json::str("stillUnknown")),
            ("reason", Json::str(reason)),
        ]),
    }
}

pub fn populate(tree: &mut Tree) {
    let system = effect("system", 4096);
    let vendor = effect("vendor", 8192);
    let possible = PossibleEffectSet {
        effects: EffectSet {
            persistent: vec![system.clone(), vendor.clone()],
            transient: Vec::new(),
            data_impact: DataImpact {
                userdata: DataImpactState::Preserved,
                calibration: DataImpactState::Preserved,
                non_volatile_config: DataImpactState::Preserved,
                secure_storage: DataImpactState::Preserved,
            },
        },
        completeness: EffectSetCompleteness::Bounded,
        source_action_ids: vec![
            ActionId::new("ACT-system").unwrap(),
            ActionId::new("ACT-vendor").unwrap(),
        ],
    };
    let body = possible.to_canonical_bytes().unwrap();
    tree.case(
        &Case {
            id: case_id("RECONCILE", 1),
            suite: SUITE,
            title: "bounded possible-effect set has one canonical digest".to_string(),
            requirements: vec!["AF-REC-003", "AF-DIG-008"],
            kind: "digest",
            description: "Encode the conservative union of unresolved effects and hash it in the possible-effect-set domain before any read-only reconcile begins.".to_string(),
            input: Json::object(vec![("bodyFile", Json::str("possible-effect-set.cbor"))]),
            expected: Json::object(vec![
                (
                    "domain",
                    Json::str(
                        String::from_utf8_lossy(Domain::PossibleEffectSet.as_bytes())
                            .replace('\0', "\\0"),
                    ),
                ),
                ("digest", Json::str(possible.digest().unwrap().to_hex())),
                ("diag", Json::str(diag(&decode_canonical(&body).unwrap()))),
                ("permitsRecoveryAssessment", Json::Bool(true)),
            ]),
        },
        vec![("possible-effect-set.cbor", body)],
    );

    let cases = [
        (
            "all declared effects are present",
            vec![
                (system.clone(), EffectObservation::Present),
                (vendor.clone(), EffectObservation::Present),
            ],
        ),
        (
            "all declared effects are absent",
            vec![
                (system.clone(), EffectObservation::Absent),
                (vendor.clone(), EffectObservation::Absent),
            ],
        ),
        (
            "present and absent effects are a settled partial execution",
            vec![
                (system.clone(), EffectObservation::Present),
                (vendor.clone(), EffectObservation::Absent),
            ],
        ),
        (
            "one indeterminate read keeps the original outcome unknown",
            vec![
                (system.clone(), EffectObservation::Present),
                (vendor.clone(), EffectObservation::Indeterminate),
            ],
        ),
        ("no observations never proves no effect", Vec::new()),
    ];
    for (index, (title, observations)) in cases.into_iter().enumerate() {
        let labels = observations
            .iter()
            .map(|(effect, observation)| {
                let partition = effect.partition().unwrap().as_str();
                let result = match observation {
                    EffectObservation::Present => "present",
                    EffectObservation::Absent => "absent",
                    EffectObservation::Indeterminate => "indeterminate",
                };
                Json::object(vec![
                    ("effect", Json::str(partition)),
                    ("observation", Json::str(result)),
                ])
            })
            .collect();
        tree.case(
            &Case {
                id: case_id("RECONCILE", index as u32 + 2),
                suite: SUITE,
                title: title.to_string(),
                requirements: vec!["AF-REC-002"],
                kind: "derive",
                description: "Reduce read-only observations only. No verdict authorizes replay, and an indeterminate or empty observation set remains stillUnknown.".to_string(),
                input: Json::object(vec![("observations", Json::Array(labels))]),
                expected: verdict_json(reconcile(&observations)),
            },
            Vec::new(),
        );
    }
}
