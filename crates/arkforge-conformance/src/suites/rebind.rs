//! Rebind evaluation: the pure rule that decides whether a re-enumerated device
//! is the admitted device in the expected mode (architecture.md §11.3).

use crate::json::Json;
use crate::suites::case_id;
use crate::tree::{Case, Tree};
use arkforge_core::digest::sha256;
use arkforge_core::effect::DeviceMode;
use arkforge_core::ids::{ObservationId, OpaqueId};
use arkforge_core::profile::RebindTolerance;
use arkforge_transport::{
    DeviceObservation, IdentityEvidenceStrength, ProtocolIdentityFact, RebindExpectation,
    RebindOutcome, SerialEvidence, SerialPolicy, TopologyPolicy, evaluate_rebind,
};

const SUITE: &str = "rebind";

fn mode(name: &str) -> DeviceMode {
    DeviceMode::new(name).unwrap()
}

struct Obs {
    id: &'static str,
    at: u64,
    mode: &'static str,
    topology: &'static str,
    descriptor: &'static str,
    serial: Option<&'static str>,
    strength: IdentityEvidenceStrength,
    malformed: bool,
}

fn observation(obs: &Obs) -> DeviceObservation {
    DeviceObservation {
        observation_id: ObservationId::new(obs.id).unwrap(),
        observed_at_epoch_ms: obs.at,
        mode: mode(obs.mode),
        topology_digest: sha256(obs.topology.as_bytes()),
        descriptor_digest: sha256(obs.descriptor.as_bytes()),
        serial_evidence: match obs.serial {
            Some(serial) => SerialEvidence::Descriptor {
                digest: sha256(serial.as_bytes()),
            },
            None => SerialEvidence::Absent,
        },
        protocol_identity: vec![ProtocolIdentityFact {
            key: OpaqueId::new("rockusb.mode").unwrap(),
            value: if obs.mode == "rockusb-loader" {
                "Loader"
            } else {
                "Normal"
            }
            .into(),
        }],
        provider_candidates: vec![],
        identity_strength: obs.strength,
        malformed_descriptor: obs.malformed,
    }
}

fn obs_json(obs: &Obs) -> Json {
    Json::object(vec![
        ("observationId", Json::str(obs.id)),
        ("observedAtEpochMs", Json::Unsigned(obs.at)),
        ("mode", Json::str(obs.mode)),
        (
            "topologyDigest",
            Json::str(format!("sha256(\"{}\")", obs.topology)),
        ),
        (
            "descriptorDigest",
            Json::str(format!("sha256(\"{}\")", obs.descriptor)),
        ),
        (
            "serialEvidence",
            match obs.serial {
                Some(serial) => Json::object(vec![
                    ("kind", Json::str("descriptor")),
                    ("digest", Json::str(format!("sha256(\"{serial}\")"))),
                ]),
                None => Json::object(vec![("kind", Json::str("absent"))]),
            },
        ),
        ("identityStrength", Json::str(obs.strength.as_str())),
        ("malformedDescriptor", Json::Bool(obs.malformed)),
    ])
}

fn outcome_json(outcome: &RebindOutcome) -> Json {
    match outcome {
        RebindOutcome::Settled(observation) => Json::object(vec![
            ("outcome", Json::str("settled")),
            (
                "observationId",
                Json::str(observation.observation_id.as_str()),
            ),
        ]),
        RebindOutcome::NoCandidate => Json::object(vec![("outcome", Json::str("noCandidate"))]),
        RebindOutcome::Ambiguous { count } => Json::object(vec![
            ("outcome", Json::str("ambiguous")),
            ("count", Json::Unsigned(*count as u64)),
        ]),
        RebindOutcome::IdentityWeakened { before, after } => Json::object(vec![
            ("outcome", Json::str("identityWeakened")),
            ("before", Json::str(before.as_str())),
            ("after", Json::str(after.as_str())),
        ]),
        RebindOutcome::SerialChanged => Json::object(vec![("outcome", Json::str("serialChanged"))]),
        RebindOutcome::TopologyChanged => {
            Json::object(vec![("outcome", Json::str("topologyChanged"))])
        }
        RebindOutcome::ExpectedModeNotReached { observed } => Json::object(vec![
            ("outcome", Json::str("expectedModeNotReached")),
            (
                "observed",
                match observed {
                    Some(mode) => Json::str(mode.as_str()),
                    None => Json::Null,
                },
            ),
        ]),
        RebindOutcome::ToleranceWindowExhausted {
            transient_observations,
        } => Json::object(vec![
            ("outcome", Json::str("toleranceWindowExhausted")),
            (
                "transientObservations",
                Json::Unsigned(*transient_observations as u64),
            ),
        ]),
    }
}

struct Scenario {
    title: &'static str,
    requirements: Vec<&'static str>,
    serial_policy: SerialPolicy,
    topology_policy: TopologyPolicy,
    floor: IdentityEvidenceStrength,
    tolerate_malformed: bool,
    window_ms: u64,
    aliases: Vec<&'static str>,
    previous: Obs,
    observations: Vec<Obs>,
}

pub fn populate(tree: &mut Tree) {
    use IdentityEvidenceStrength::*;
    let previous = || Obs {
        id: "OBS-PREV",
        at: 1_000,
        mode: "hdc-normal",
        topology: "port-A",
        descriptor: "desc-normal",
        serial: Some("SERIAL-1"),
        strength: SerialAndTopology,
        malformed: false,
    };
    let loader =
        |id: &'static str, at: u64, topology: &'static str, serial: Option<&'static str>| Obs {
            id,
            at,
            mode: "rockusb-loader",
            topology,
            descriptor: "desc-loader",
            serial,
            strength: SerialAndTopology,
            malformed: false,
        };

    let scenarios: Vec<Scenario> = vec![
        Scenario {
            title: "one stable observation in the expected mode settles",
            requirements: vec!["AF-TRN-010"],
            serial_policy: SerialPolicy::MayChange,
            topology_policy: TopologyPolicy::MayChange,
            floor: SerialAndTopology,
            tolerate_malformed: true,
            window_ms: 30_000,
            aliases: vec!["loader"],
            previous: previous(),
            observations: vec![loader("OBS-1", 2_000, "port-B", Some("SERIAL-2"))],
        },
        Scenario {
            title: "no observation at all: no candidate",
            requirements: vec!["AF-TRN-011"],
            serial_policy: SerialPolicy::MayChange,
            topology_policy: TopologyPolicy::MayChange,
            floor: SerialAndTopology,
            tolerate_malformed: true,
            window_ms: 30_000,
            aliases: vec![],
            previous: previous(),
            observations: vec![],
        },
        Scenario {
            title: "two stable candidates in the expected mode: ambiguous, never first-match",
            requirements: vec!["AF-TRN-012"],
            serial_policy: SerialPolicy::MayChange,
            topology_policy: TopologyPolicy::MayChange,
            floor: SerialAndTopology,
            tolerate_malformed: true,
            window_ms: 30_000,
            aliases: vec![],
            previous: previous(),
            observations: vec![
                loader("OBS-1", 2_000, "port-B", Some("SERIAL-2")),
                Obs {
                    descriptor: "desc-loader-other",
                    ..loader("OBS-2", 2_100, "port-C", Some("SERIAL-3"))
                },
            ],
        },
        Scenario {
            title: "transient malformed descriptors inside the window are evidence, then settle",
            requirements: vec!["AF-TRN-013"],
            serial_policy: SerialPolicy::MayChange,
            topology_policy: TopologyPolicy::MayChange,
            floor: SerialAndTopology,
            tolerate_malformed: true,
            window_ms: 30_000,
            aliases: vec![],
            previous: previous(),
            observations: vec![
                Obs {
                    malformed: true,
                    strength: ClassOnly,
                    ..loader("OBS-1", 2_000, "port-B", None)
                },
                Obs {
                    malformed: true,
                    strength: ClassOnly,
                    ..loader("OBS-2", 5_000, "port-B", None)
                },
                loader("OBS-3", 9_000, "port-B", Some("SERIAL-2")),
            ],
        },
        Scenario {
            title: "a malformed descriptor past the window exhausts tolerance",
            requirements: vec!["AF-TRN-013"],
            serial_policy: SerialPolicy::MayChange,
            topology_policy: TopologyPolicy::MayChange,
            floor: SerialAndTopology,
            tolerate_malformed: true,
            window_ms: 30_000,
            aliases: vec![],
            previous: previous(),
            observations: vec![
                Obs {
                    malformed: true,
                    strength: ClassOnly,
                    ..loader("OBS-1", 2_000, "port-B", None)
                },
                Obs {
                    malformed: true,
                    strength: ClassOnly,
                    ..loader("OBS-2", 40_000, "port-B", None)
                },
            ],
        },
        Scenario {
            title: "malformed descriptors are fatal when the profile does not tolerate them",
            requirements: vec!["AF-TRN-013"],
            serial_policy: SerialPolicy::MayChange,
            topology_policy: TopologyPolicy::MayChange,
            floor: SerialAndTopology,
            tolerate_malformed: false,
            window_ms: 30_000,
            aliases: vec![],
            previous: previous(),
            observations: vec![Obs {
                malformed: true,
                strength: ClassOnly,
                ..loader("OBS-1", 2_000, "port-B", None)
            }],
        },
        Scenario {
            title: "only the previous mode is seen inside the window, then nothing: window exhausted",
            requirements: vec!["AF-TRN-014"],
            serial_policy: SerialPolicy::MayChange,
            topology_policy: TopologyPolicy::MayChange,
            floor: SerialAndTopology,
            tolerate_malformed: true,
            window_ms: 30_000,
            aliases: vec![],
            previous: previous(),
            observations: vec![Obs {
                id: "OBS-1",
                at: 2_000,
                ..previous()
            }],
        },
        Scenario {
            title: "the wrong mode past the window: expected mode not reached",
            requirements: vec!["AF-TRN-014"],
            serial_policy: SerialPolicy::MayChange,
            topology_policy: TopologyPolicy::MayChange,
            floor: SerialAndTopology,
            tolerate_malformed: true,
            window_ms: 30_000,
            aliases: vec![],
            previous: previous(),
            observations: vec![
                Obs {
                    id: "OBS-1",
                    at: 2_000,
                    ..previous()
                },
                Obs {
                    id: "OBS-2",
                    at: 40_000,
                    ..previous()
                },
            ],
        },
        Scenario {
            title: "a profile alias of the expected mode settles",
            requirements: vec!["AF-TRN-015"],
            serial_policy: SerialPolicy::MayChange,
            topology_policy: TopologyPolicy::MayChange,
            floor: SerialAndTopology,
            tolerate_malformed: true,
            window_ms: 30_000,
            aliases: vec!["loader"],
            previous: previous(),
            observations: vec![Obs {
                mode: "loader",
                ..loader("OBS-1", 2_000, "port-B", Some("SERIAL-2"))
            }],
        },
        Scenario {
            title: "identity weaker than the floor stops",
            requirements: vec!["AF-TRN-016"],
            serial_policy: SerialPolicy::MayChange,
            topology_policy: TopologyPolicy::MayChange,
            floor: SerialAndTopology,
            tolerate_malformed: true,
            window_ms: 30_000,
            aliases: vec![],
            previous: previous(),
            observations: vec![Obs {
                strength: SerialAsserted,
                ..loader("OBS-1", 2_000, "port-B", Some("SERIAL-2"))
            }],
        },
        Scenario {
            title: "identity weaker than before the transition stops even above the floor",
            requirements: vec!["AF-TRN-016"],
            serial_policy: SerialPolicy::MayChange,
            topology_policy: TopologyPolicy::MayChange,
            floor: ClassOnly,
            tolerate_malformed: true,
            window_ms: 30_000,
            aliases: vec![],
            previous: Obs {
                strength: ProtocolConfirmed,
                ..previous()
            },
            observations: vec![loader("OBS-1", 2_000, "port-B", Some("SERIAL-2"))],
        },
        Scenario {
            title: "serial must match and changed: stop",
            requirements: vec!["AF-TRN-017"],
            serial_policy: SerialPolicy::MustMatch,
            topology_policy: TopologyPolicy::MayChange,
            floor: SerialAndTopology,
            tolerate_malformed: true,
            window_ms: 30_000,
            aliases: vec![],
            previous: previous(),
            observations: vec![loader("OBS-1", 2_000, "port-B", Some("SERIAL-2"))],
        },
        Scenario {
            title: "serial must match and did: settle",
            requirements: vec!["AF-TRN-017"],
            serial_policy: SerialPolicy::MustMatch,
            topology_policy: TopologyPolicy::MayChange,
            floor: SerialAndTopology,
            tolerate_malformed: true,
            window_ms: 30_000,
            aliases: vec![],
            previous: previous(),
            observations: vec![loader("OBS-1", 2_000, "port-B", Some("SERIAL-1"))],
        },
        Scenario {
            title: "topology must match and changed: stop",
            requirements: vec!["AF-TRN-017"],
            serial_policy: SerialPolicy::MayChange,
            topology_policy: TopologyPolicy::MustMatch,
            floor: SerialAndTopology,
            tolerate_malformed: true,
            window_ms: 30_000,
            aliases: vec![],
            previous: previous(),
            observations: vec![loader("OBS-1", 2_000, "port-B", Some("SERIAL-1"))],
        },
        Scenario {
            title: "precedence: identity is checked before serial and topology",
            requirements: vec!["AF-TRN-018"],
            serial_policy: SerialPolicy::MustMatch,
            topology_policy: TopologyPolicy::MustMatch,
            floor: ProtocolConfirmed,
            tolerate_malformed: true,
            window_ms: 30_000,
            aliases: vec![],
            previous: previous(),
            observations: vec![loader("OBS-1", 2_000, "port-B", Some("SERIAL-2"))],
        },
        Scenario {
            title: "precedence: serial is checked before topology",
            requirements: vec!["AF-TRN-018"],
            serial_policy: SerialPolicy::MustMatch,
            topology_policy: TopologyPolicy::MustMatch,
            floor: ClassOnly,
            tolerate_malformed: true,
            window_ms: 30_000,
            aliases: vec![],
            previous: previous(),
            observations: vec![loader("OBS-1", 2_000, "port-B", Some("SERIAL-2"))],
        },
        Scenario {
            title: "the same descriptor seen twice is one candidate, not two",
            requirements: vec!["AF-TRN-012"],
            serial_policy: SerialPolicy::MayChange,
            topology_policy: TopologyPolicy::MayChange,
            floor: SerialAndTopology,
            tolerate_malformed: true,
            window_ms: 30_000,
            aliases: vec![],
            previous: previous(),
            observations: vec![
                loader("OBS-1", 2_000, "port-B", Some("SERIAL-2")),
                loader("OBS-2", 2_500, "port-B", Some("SERIAL-2")),
            ],
        },
        Scenario {
            title: "a malformed observation exactly at the window edge is still tolerated; with no stable follow-up the window is exhausted",
            requirements: vec!["AF-TRN-013"],
            serial_policy: SerialPolicy::MayChange,
            topology_policy: TopologyPolicy::MayChange,
            floor: SerialAndTopology,
            tolerate_malformed: true,
            window_ms: 30_000,
            aliases: vec![],
            previous: previous(),
            observations: vec![
                Obs {
                    malformed: true,
                    strength: ClassOnly,
                    ..loader("OBS-1", 2_000, "port-B", None)
                },
                Obs {
                    malformed: true,
                    strength: ClassOnly,
                    ..loader("OBS-2", 32_000, "port-B", None)
                },
            ],
        },
    ];

    for (index, scenario) in scenarios.iter().enumerate() {
        let expectation = RebindExpectation {
            from_mode: mode("hdc-normal"),
            to_mode: mode("rockusb-loader"),
            to_mode_aliases: scenario.aliases.iter().map(|a| mode(a)).collect(),
            allowed_identity_set_digest: sha256(b"allowed-identity-set"),
            serial_policy: scenario.serial_policy,
            topology_policy: scenario.topology_policy,
            identity_strength_floor: scenario.floor,
            tolerance: RebindTolerance {
                require_disconnect: true,
                tolerance_window_ms: scenario.window_ms,
                tolerate_transient_malformed: scenario.tolerate_malformed,
            },
            deadline_epoch_ms: 1_000 + scenario.window_ms,
        };
        let previous = observation(&scenario.previous);
        let observations: Vec<DeviceObservation> =
            scenario.observations.iter().map(observation).collect();
        let outcome = evaluate_rebind(&expectation, &previous, &observations);
        tree.case(
            &Case {
                id: case_id("REBIND", index as u32 + 1),
                suite: SUITE,
                title: scenario.title.to_string(),
                requirements: scenario.requirements.clone(),
                kind: "derive",
                description: "evaluate_rebind(expectation, previous, observations) over the \
                              observations in the order they were made. The tolerance \
                              window is measured from the first observation's timestamp."
                    .to_string(),
                input: Json::object(vec![
                    (
                        "expectation",
                        Json::object(vec![
                            ("fromMode", Json::str("hdc-normal")),
                            ("toMode", Json::str("rockusb-loader")),
                            (
                                "toModeAliases",
                                Json::strs(scenario.aliases.iter().copied()),
                            ),
                            (
                                "serialPolicy",
                                Json::str(match scenario.serial_policy {
                                    SerialPolicy::MustMatch => "must-match",
                                    SerialPolicy::MayChange => "may-change",
                                }),
                            ),
                            (
                                "topologyPolicy",
                                Json::str(match scenario.topology_policy {
                                    TopologyPolicy::MustMatch => "must-match",
                                    TopologyPolicy::MayChange => "may-change",
                                }),
                            ),
                            ("identityStrengthFloor", Json::str(scenario.floor.as_str())),
                            ("toleranceWindowMs", Json::Unsigned(scenario.window_ms)),
                            (
                                "tolerateTransientMalformed",
                                Json::Bool(scenario.tolerate_malformed),
                            ),
                        ]),
                    ),
                    ("previous", obs_json(&scenario.previous)),
                    (
                        "observations",
                        Json::Array(scenario.observations.iter().map(obs_json).collect()),
                    ),
                ]),
                expected: outcome_json(&outcome),
            },
            Vec::new(),
        );
    }
}
