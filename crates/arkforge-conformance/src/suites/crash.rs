//! Crash dispositions and permit dispositions, derived from journal prefixes.
//!
//! architecture.md §13.3 as data: each case is a journal (the exact records a
//! daemon had durably written when it died) and the one row that applies.

use crate::json::Json;
use crate::suites::case_id;
use crate::suites::journal::{facts, id};
use crate::tree::{Case, Tree};
use arkforge_engine::JobState;
use arkforge_engine::journal::{Journal, JournalRecordKind};
use arkforge_engine::recovery::{CrashDisposition, PermitDisposition, PermitLedger};

const SUITE: &str = "crash";

struct Step {
    subject: &'static str,
    kind: JournalRecordKind,
    facts: Vec<(&'static str, &'static str)>,
}

fn step(kind: JournalRecordKind, facts: &[(&'static str, &'static str)]) -> Step {
    step_for("JOB-1", kind, facts)
}

fn step_for(
    subject: &'static str,
    kind: JournalRecordKind,
    facts: &[(&'static str, &'static str)],
) -> Step {
    Step {
        subject,
        kind,
        facts: facts.to_vec(),
    }
}

fn build(steps: &[Step]) -> Journal {
    let mut journal = Journal::new();
    let mut clock = 1_000u64;
    for step in steps {
        clock += 10;
        journal
            .append(step.kind, clock, 1, id(step.subject), facts(&step.facts))
            .unwrap();
    }
    journal
}

fn disposition_json(disposition: &CrashDisposition) -> Json {
    let (row, permit, state) = match disposition {
        CrashDisposition::NoJob => ("noJob", None, None),
        CrashDisposition::SafeToCancel => ("safeToCancel", None, None),
        CrashDisposition::DispatchForbiddenUntilIntentDurable { permit_id } => (
            "dispatchForbiddenUntilIntentDurable",
            Some(permit_id.clone()),
            None,
        ),
        CrashDisposition::OutcomeUnknown { permit_id } => {
            ("outcomeUnknown", Some(permit_id.clone()), None)
        }
        CrashDisposition::ReceiptDurableCheckpointMissing { permit_id } => (
            "receiptDurableCheckpointMissing",
            Some(permit_id.clone()),
            None,
        ),
        CrashDisposition::ReplayFromCheckpoint => ("replayFromCheckpoint", None, None),
        CrashDisposition::Concluded(state) => ("concluded", None, Some(state.as_str())),
    };
    let mut json = Json::object(vec![
        ("row", Json::str(row)),
        (
            "permitsExternalEffect",
            Json::Bool(disposition.permits_external_effect()),
        ),
    ]);
    if let Some(permit) = permit {
        json.push("permitId", Json::str(permit));
    }
    if let Some(state) = state {
        json.push("state", Json::str(state));
    }
    json
}

fn permit_disposition_json(disposition: &PermitDisposition) -> Json {
    let (name, receipt) = match disposition {
        PermitDisposition::Unseen => ("unseen", None),
        PermitDisposition::AcceptedIntentNotDurable => ("acceptedIntentNotDurable", None),
        PermitDisposition::IntentDurable => ("intentDurable", None),
        PermitDisposition::ConsumingOutcomeUnknown => ("consumingOutcomeUnknown", None),
        PermitDisposition::Consumed { receipt_digest } => {
            ("consumed", Some(receipt_digest.clone()))
        }
    };
    let mut json = Json::object(vec![
        ("disposition", Json::str(name)),
        (
            "permitsDispatch",
            Json::Bool(disposition.permits_dispatch()),
        ),
    ]);
    if let Some(receipt) = receipt {
        json.push("receiptDigest", Json::str(receipt));
    }
    json
}

pub fn populate(tree: &mut Tree) {
    let p1 = ("permitId", "PERMIT-1");
    let s1 = ("stepId", "STEP-1");
    let j1 = ("jobId", "JOB-1");

    let created = || step(JournalRecordKind::JobCreated, &[j1]);
    let accepted = || step(JournalRecordKind::StepPermitAccepted, &[j1, s1, p1]);
    let intent = || step(JournalRecordKind::StepIntentRecorded, &[j1, s1, p1]);
    let consuming = || step(JournalRecordKind::PermitConsuming, &[j1, p1]);
    let dispatched = || step(JournalRecordKind::ExternalDispatchStarted, &[j1, p1]);
    let receipt = || {
        step(
            JournalRecordKind::SemanticReceiptRecorded,
            &[j1, p1, ("receiptDigest", "abc123")],
        )
    };
    let consumed = || {
        step(
            JournalRecordKind::PermitConsumed,
            &[j1, p1, ("receiptDigest", "abc123")],
        )
    };
    let checkpointed = || step(JournalRecordKind::StepCheckpointed, &[j1, s1, p1]);
    let concluded = |outcome: &'static str| {
        step(
            JournalRecordKind::OutcomeClassified,
            &[j1, ("outcome", outcome)],
        )
    };

    let cases: Vec<(&str, Vec<&str>, Vec<Step>)> = vec![
        ("empty journal: no job", vec!["AF-CRASH-R-001"], vec![]),
        (
            "only a plan stored for another subject: still no job",
            vec!["AF-CRASH-R-001"],
            vec![step_for(
                "PLAN-1",
                JournalRecordKind::PlanStored,
                &[("planId", "PLAN-1")],
            )],
        ),
        (
            "job created, no permit ever mentioned: safe to cancel",
            vec!["AF-CRASH-R-002"],
            vec![created()],
        ),
        (
            "preflight observed, admission requested, no permit: safe to cancel",
            vec!["AF-CRASH-R-002"],
            vec![
                created(),
                step(JournalRecordKind::PreflightObserved, &[j1]),
                step(JournalRecordKind::StepAdmissionRequested, &[j1, s1]),
            ],
        ),
        (
            "permit accepted, intent not durable: dispatch forbidden",
            vec!["AF-CRASH-R-003"],
            vec![created(), accepted()],
        ),
        (
            "intent durable, nothing after: outcome unknown (about-to-dispatch is indistinguishable from dispatched)",
            vec!["AF-CRASH-R-004"],
            vec![created(), accepted(), intent()],
        ),
        (
            "permit consuming, no receipt: outcome unknown",
            vec!["AF-CRASH-R-004"],
            vec![created(), accepted(), intent(), consuming()],
        ),
        (
            "external dispatch started, no receipt: outcome unknown",
            vec!["AF-CRASH-R-004"],
            vec![created(), accepted(), intent(), consuming(), dispatched()],
        ),
        (
            "semantic receipt recorded before permitConsumed: receipt settled, checkpoint missing",
            vec!["AF-CRASH-R-005"],
            vec![
                created(),
                accepted(),
                intent(),
                consuming(),
                dispatched(),
                receipt(),
            ],
        ),
        (
            "permit consumed with receipt, no checkpoint: receipt settled, checkpoint missing",
            vec!["AF-CRASH-R-005"],
            vec![
                created(),
                accepted(),
                intent(),
                consuming(),
                dispatched(),
                receipt(),
                consumed(),
            ],
        ),
        (
            "checkpointed: replay to the authority, never re-execute",
            vec!["AF-CRASH-R-006"],
            vec![
                created(),
                accepted(),
                intent(),
                consuming(),
                dispatched(),
                receipt(),
                consumed(),
                checkpointed(),
            ],
        ),
        (
            "the first terminal classification concludes succeeded and is immutable",
            vec!["AF-CRASH-R-007"],
            vec![
                created(),
                accepted(),
                intent(),
                consuming(),
                receipt(),
                consumed(),
                checkpointed(),
                concluded("succeeded"),
                concluded("cancelledSafe"),
            ],
        ),
        (
            "concluded confirmedFailed",
            vec!["AF-CRASH-R-007"],
            vec![created(), concluded("confirmedFailed")],
        ),
        (
            "concluded cancelledSafe",
            vec!["AF-CRASH-R-007"],
            vec![
                created(),
                step(JournalRecordKind::CancellationRequested, &[j1]),
                concluded("cancelledSafe"),
            ],
        ),
        (
            "concluded recoveryAssessable",
            vec!["AF-CRASH-R-007"],
            vec![
                created(),
                accepted(),
                intent(),
                concluded("recoveryAssessable"),
            ],
        ),
        (
            "an outcomeClassified fact that is not a terminal state does not conclude",
            vec!["AF-CRASH-R-007"],
            vec![created(), accepted(), intent(), concluded("outcomeUnknown")],
        ),
        (
            "a cancellation request alone does not conclude anything",
            vec!["AF-CRASH-R-002"],
            vec![
                created(),
                step(JournalRecordKind::CancellationRequested, &[j1]),
            ],
        ),
        (
            "two permits: the newest decides, and it is unsettled",
            vec!["AF-CRASH-R-008"],
            vec![
                created(),
                accepted(),
                intent(),
                consuming(),
                dispatched(),
                receipt(),
                consumed(),
                checkpointed(),
                step(
                    JournalRecordKind::StepPermitAccepted,
                    &[j1, ("stepId", "STEP-2"), ("permitId", "PERMIT-2")],
                ),
                step(
                    JournalRecordKind::StepIntentRecorded,
                    &[j1, ("stepId", "STEP-2"), ("permitId", "PERMIT-2")],
                ),
            ],
        ),
        (
            "two permits: the newest is checkpointed, so replay from checkpoint",
            vec!["AF-CRASH-R-008"],
            vec![
                created(),
                accepted(),
                intent(),
                consuming(),
                dispatched(),
                receipt(),
                consumed(),
                checkpointed(),
                step(
                    JournalRecordKind::StepPermitAccepted,
                    &[j1, ("stepId", "STEP-2"), ("permitId", "PERMIT-2")],
                ),
                step(
                    JournalRecordKind::StepIntentRecorded,
                    &[j1, ("stepId", "STEP-2"), ("permitId", "PERMIT-2")],
                ),
                step(
                    JournalRecordKind::PermitConsuming,
                    &[j1, ("permitId", "PERMIT-2")],
                ),
                step(
                    JournalRecordKind::PermitConsumed,
                    &[j1, ("permitId", "PERMIT-2"), ("receiptDigest", "def456")],
                ),
                step(
                    JournalRecordKind::StepCheckpointed,
                    &[j1, ("stepId", "STEP-2"), ("permitId", "PERMIT-2")],
                ),
            ],
        ),
        (
            "an explicit other jobId overrides a coincidentally matching subject",
            vec!["AF-CRASH-R-009"],
            vec![
                created(),
                step(
                    JournalRecordKind::StepPermitAccepted,
                    &[("jobId", "JOB-2"), ("permitId", "PERMIT-OTHER")],
                ),
            ],
        ),
    ];

    for (index, (title, requirements, steps)) in cases.iter().enumerate() {
        let journal = build(steps);
        let disposition = CrashDisposition::derive(&journal, "JOB-1");
        let ledger = PermitLedger::from_journal(&journal);
        let ledger_json: Vec<Json> = ledger
            .iter()
            .map(|(permit, disposition)| {
                let mut json = Json::object(vec![("permitId", Json::str(permit.clone()))]);
                if let Json::Object(entries) = permit_disposition_json(disposition) {
                    for (k, v) in entries {
                        json.push(&k, v);
                    }
                }
                json
            })
            .collect();
        let unresolved: Vec<String> = ledger.unresolved().into_iter().cloned().collect();

        let records: Vec<Json> = steps
            .iter()
            .map(|s| {
                Json::object(vec![
                    ("subject", Json::str(s.subject)),
                    ("kind", Json::str(s.kind.as_str())),
                    (
                        "facts",
                        Json::Object(
                            s.facts
                                .iter()
                                .map(|(k, v)| ((*k).to_string(), Json::str(*v)))
                                .collect(),
                        ),
                    ),
                ])
            })
            .collect();
        let bytes: Vec<u8> = journal
            .records()
            .iter()
            .flat_map(|r| {
                let body = r.to_canonical_bytes().unwrap();
                let mut frame = (body.len() as u32).to_be_bytes().to_vec();
                frame.extend(body);
                frame
            })
            .collect();

        tree.case(
            &Case {
                id: case_id("CRASH", index as u32 + 1),
                suite: SUITE,
                title: title.to_string(),
                requirements: {
                    let mut r = vec!["AF-CRASH-002", "AF-AUTH-P-001"];
                    r.extend(requirements.iter().copied());
                    r
                },
                kind: "derive",
                description: "Replay the records with the supplied subject (atEpochMs \
                              1010, 1020, …; jobRevision 1), then \
                              derive the crash disposition for job JOB-1 and the permit \
                              ledger. `journal-frames.bin` holds the same records framed \
                              as on disk, without the file magic."
                    .to_string(),
                input: Json::object(vec![
                    ("jobId", Json::str("JOB-1")),
                    ("records", Json::Array(records)),
                ]),
                expected: Json::object(vec![
                    ("crashDisposition", disposition_json(&disposition)),
                    ("permitLedger", Json::Array(ledger_json)),
                    ("unresolvedPermits", Json::strs(unresolved)),
                ]),
            },
            vec![("journal-frames.bin", bytes)],
        );
    }

    // The terminal-state vocabulary the OutcomeClassified fact may carry.
    tree.case(
        &Case {
            id: case_id("CRASH", cases.len() as u32 + 1),
            suite: SUITE,
            title: "terminal outcomes an outcomeClassified record may name".to_string(),
            requirements: vec!["AF-CRASH-R-007"],
            kind: "table",
            description: "Only these `outcome` fact values conclude a job on replay.".to_string(),
            input: Json::object(vec![]),
            expected: Json::object(vec![(
                "terminal",
                Json::strs(
                    [
                        JobState::Succeeded,
                        JobState::ConfirmedFailed,
                        JobState::CancelledSafe,
                        JobState::RecoveryAssessable,
                    ]
                    .iter()
                    .map(|s| s.as_str()),
                ),
            )]),
        },
        Vec::new(),
    );
}
