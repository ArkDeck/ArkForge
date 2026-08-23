//! The job state machine as a table: every state with its flags, and the full
//! legal-transition matrix. A port tests its own `may_transition_to` against
//! this table; the Mermaid diagram in architecture.md is a picture of it.

use crate::json::Json;
use crate::suites::case_id;
use crate::tree::{Case, Tree};
use arkforge_engine::JobState;

const SUITE: &str = "state-machine";

pub const ALL_STATES: [JobState; 16] = [
    JobState::Planned,
    JobState::AwaitingStart,
    JobState::Preflight,
    JobState::AwaitingPermit,
    JobState::StepIntentDurable,
    JobState::Dispatching,
    JobState::ReceiptDurable,
    JobState::Checkpointed,
    JobState::RebindWait,
    JobState::Postflight,
    JobState::Succeeded,
    JobState::ConfirmedFailed,
    JobState::CancelledSafe,
    JobState::OutcomeUnknown,
    JobState::Reconciling,
    JobState::RecoveryAssessable,
];

pub fn populate(tree: &mut Tree) {
    let states: Vec<Json> = ALL_STATES
        .iter()
        .map(|state| {
            let successors: Vec<&str> = ALL_STATES
                .iter()
                .filter(|next| state.may_transition_to(**next))
                .map(|next| next.as_str())
                .collect();
            Json::object(vec![
                ("state", Json::str(state.as_str())),
                ("terminal", Json::Bool(state.is_terminal())),
                (
                    "permitsExternalDispatch",
                    Json::Bool(state.permits_external_dispatch()),
                ),
                ("successors", Json::strs(successors)),
            ])
        })
        .collect();

    let mut matrix = Vec::new();
    for from in ALL_STATES {
        for to in ALL_STATES {
            if from.may_transition_to(to) {
                matrix.push(Json::Array(vec![
                    Json::str(from.as_str()),
                    Json::str(to.as_str()),
                ]));
            }
        }
    }
    let edge_count = matrix.len() as u64;

    tree.case(
        &Case {
            id: case_id("STATEMACHINE", 1),
            suite: SUITE,
            title: "job states, flags and the complete legal-edge set".to_string(),
            requirements: vec!["AF-ENG-001", "AF-ENG-002", "AF-ENG-003", "AF-ENG-004"],
            kind: "table",
            description: "Every (from, to) pair not listed in `expected.edges` is an \
                          illegal transition and MUST be refused. Exactly one state \
                          permits an external dispatch. Terminal states have no \
                          successors. No path leads from outcomeUnknown back to a \
                          state that can dispatch."
                .to_string(),
            input: Json::object(vec![(
                "stateCount",
                Json::Unsigned(ALL_STATES.len() as u64),
            )]),
            expected: Json::object(vec![
                ("states", Json::Array(states)),
                ("edgeCount", Json::Unsigned(edge_count)),
                ("edges", Json::Array(matrix)),
                (
                    "onlyDispatchingState",
                    Json::str(JobState::StepIntentDurable.as_str()),
                ),
            ]),
        },
        Vec::new(),
    );

    // Named invariants, each as a separate checkable case.
    let invariants: Vec<(&str, &str, Vec<&str>, Json)> = vec![
        (
            "no edge from outcomeUnknown reaches a dispatching state",
            "AF-ENG-005",
            vec!["AF-ENG-005"],
            Json::object(vec![
                ("from", Json::str("outcomeUnknown")),
                (
                    "forbiddenTargets",
                    Json::strs([
                        "preflight",
                        "awaitingPermit",
                        "stepIntentDurable",
                        "dispatching",
                    ]),
                ),
                (
                    "allowedTargets",
                    Json::strs(["reconciling", "recoveryAssessable"]),
                ),
            ]),
        ),
        (
            "reconciling may conclude but may not dispatch",
            "AF-ENG-006",
            vec!["AF-ENG-006"],
            Json::object(vec![
                ("from", Json::str("reconciling")),
                (
                    "allowedTargets",
                    Json::strs(["succeeded", "confirmedFailed", "outcomeUnknown"]),
                ),
                (
                    "forbiddenTargets",
                    Json::strs(["dispatching", "stepIntentDurable"]),
                ),
            ]),
        ),
        (
            "recoveryAssessable is terminal and is not success",
            "AF-ENG-007",
            vec!["AF-ENG-007"],
            Json::object(vec![
                ("state", Json::str("recoveryAssessable")),
                ("terminal", Json::Bool(true)),
                ("isSuccess", Json::Bool(false)),
            ]),
        ),
        (
            "cancel edges exist only at declared safe boundaries",
            "AF-ENG-008",
            vec!["AF-ENG-008"],
            Json::object(vec![
                (
                    "statesWithCancelledSafeEdge",
                    Json::strs(
                        ALL_STATES
                            .iter()
                            .filter(|s| s.may_transition_to(JobState::CancelledSafe))
                            .map(|s| s.as_str()),
                    ),
                ),
                ("stepIntentDurableMayCancel", Json::Bool(false)),
            ]),
        ),
    ];
    for (index, (title, _, requirements, expected)) in invariants.into_iter().enumerate() {
        tree.case(
            &Case {
                id: case_id("STATEMACHINE", index as u32 + 2),
                suite: SUITE,
                title: title.to_string(),
                requirements,
                kind: "table",
                description: "A named invariant over the edge set of AF-CONF-STATEMACHINE-001."
                    .to_string(),
                input: Json::object(vec![]),
                expected,
            },
            Vec::new(),
        );
    }
}
