# Durable engine: job state machine, journal, crash windows
status: normative
area: ENG, JRN, CRASH
rationale: architecture.md §13, §14.1, AD-017
conformance: state-machine, journal, crash

The engine's promise is narrow and absolute: an external effect is dispatched
at most once per permit, and after any process death the journal alone says
whether that effect may have happened. Everything else (resumption, retries,
recovery) is built on top and is forbidden from weakening it.

## Job state machine

The normative edge table is `state-machines/job.yaml` and fixture
AF-CONF-STATEMACHINE-001. The Mermaid diagram in architecture.md §13.1 is a
reviewed picture of the same table; any future mismatch is a spec failure.

### AF-ENG-001 — sixteen states with fixed wire spellings
status: normative
tests: [AF-CONF-STATEMACHINE-001]

The job states and their spellings are the `job-state` vocabulary. A state not
in the table MUST be refused wherever a state is parsed.

### AF-ENG-002 — terminal states
status: normative
tests: [AF-CONF-STATEMACHINE-001]

`succeeded`, `confirmedFailed`, `cancelledSafe`, `recoveryAssessable` are
terminal: they have no successors and a job in one of them accepts no
transition.

### AF-ENG-003 — exactly one state may dispatch
status: normative
tests: [AF-CONF-STATEMACHINE-001]

An external effect MAY be dispatched only from `stepIntentDurable`. In every
other state an implementation MUST refuse to start a dispatch.

### AF-ENG-004 — the legal-edge set is closed
status: normative
tests: [AF-CONF-STATEMACHINE-001]

Exactly the 29 edges listed in AF-CONF-STATEMACHINE-001 are legal. An
implementation MUST refuse any other `(from, to)` with `ILLEGAL_TRANSITION`
and MUST NOT assign a state without passing through that check.

### AF-ENG-005 — outcomeUnknown never returns to a dispatching state
status: normative
tests: [AF-CONF-STATEMACHINE-002]

From `outcomeUnknown` the only successors are `reconciling` and
`recoveryAssessable`. There is no path from `outcomeUnknown`, `reconciling` or
`recoveryAssessable` to `preflight`, `awaitingPermit`, `stepIntentDurable` or
`dispatching`.

### AF-ENG-006 — reconciling may conclude but may not dispatch
status: normative
tests: [AF-CONF-STATEMACHINE-003]

### AF-ENG-007 — recoveryAssessable is terminal and is not success
status: normative
tests: [AF-CONF-STATEMACHINE-004]

### AF-ENG-008 — cancellation edges
status: normative
tests: [AF-CONF-STATEMACHINE-005]

`cancelledSafe` is reachable only from `preflight`, `awaitingPermit`,
`dispatching`, `checkpointed` and `rebindWait`. `stepIntentDurable` MUST NOT claim safety.
`dispatching → cancelledSafe` is legal only when the implementation can prove
the work never left its queue (AF-ENG-014).

## Daemon lifecycle (what the reference daemon actually does)

### AF-ENG-010 — every public step goes through admission
status: draft
source: crates/arkforged/src/jobs.rs request_admission, submit_permit
tests: []

For each public step in order the daemon: takes a fresh observation; writes
`stepAdmissionRequested` (buffered); publishes `STEP_ADMISSION_REQUESTED` with
the snapshot; waits for `submitStepPermit`. This applies to read-only steps as
well. There is deliberately no unadmitted read-only dispatch state: uniform
admission binds exact device, provider, toolchain and artifact facts even when
the declared workflow effect is read-only.

### AF-ENG-011 — record order around a dispatch
status: normative
tests: [AF-CONF-CRASH-005..011]

On an accepted permit the daemon MUST append, in this order, each durable before
the next: `stepPermitAccepted` → `stepIntentRecorded` → `permitConsuming` →
`externalDispatchStarted` → (dispatch happens) → `transportEvidenceRecorded` (buffered) →
`semanticReceiptRecorded` → `permitConsumed` → `stepCheckpointed`. No device
access may occur before `externalDispatchStarted` has returned from a durable append,
and every step after the dispatch MUST assume the device may have been touched.
Every transaction record carries the same `jobId`, `planId`, `stepId`,
`attemptId` and `permitId` context.

### AF-ENG-012 — state moves follow the records
status: draft
source: crates/arkforged/src/jobs.rs
tests: []

`awaitingPermit` on permit verification; `stepIntentDurable` after
`stepIntentRecorded`; `dispatching` after `externalDispatchStarted`; `receiptDurable`
after `permitConsumed`; `checkpointed` after `stepCheckpointed`; then
`preflight` for a same-mode next step, `rebindWait` across a sealed mode change,
or `postflight → succeeded` after the last step. `outcomeUnknown` alone moves
to the state of that name. `confirmedNoEffect` and `confirmedPartialEffect`
first make their ActionReceipt and checkpoint durable, then conclude
`confirmedFailed` while preserving the exact action disposition.

### AF-ENG-013 — a step the authority performs is a managed control request
status: draft
source: crates/arkforged/src/jobs.rs submit_permit (managed_control_for)
tests: []

When the step's primary action lowers to a managed device control action, the
daemon writes `permitConsuming` with `controlRequestId`, `controlAction` and
`controlDeadlineEpochMs` facts, publishes `MANAGED_CONTROL_REQUESTED`, and
waits for `submitManagedControlReceipt`. An unanswered request past its deadline
(120 000 ms after the permit) is classified `outcomeUnknown`, never retried
(`ports/device-control.md`).

### AF-ENG-014 — cancellation
status: draft
source: crates/arkforged/src/jobs.rs JobRegistry::cancel
tests: []

`cancelJob` MUST append `cancellationRequested` (durable) and then: if the job
is `outcomeUnknown`, answer `outcomeUnknown` and change nothing; if terminal,
answer `alreadyTerminal`; if a dispatch is pending but has not been handed to a
dispatcher, conclude `cancelledSafe` immediately; if a dispatch or control
request is in flight (or the state is `stepIntentDurable`/`dispatching`), answer
`queuedAtSafeBoundary` and conclude `cancelledSafe` after the in-flight step
reaches `checkpointed`; otherwise (before any permit) conclude `cancelledSafe`.
Killing the executing thread is not a cancellation.

### AF-ENG-015 — conclusion record
status: normative
tests: [AF-CONF-CRASH-012..016, AF-CONF-CRASH-021]

A job concludes by appending `outcomeClassified` with facts `outcome` (one of
`succeeded`, `confirmedFailed`, `cancelledSafe`, `recoveryAssessable`,
`outcomeUnknown`, or a receipt disposition), `reason` (free text) and
`eventSequence`. Only the four terminal outcomes conclude on replay; any other
value reads as `outcomeUnknown`.

### AF-ENG-016 — job events
status: normative
source: crates/arkforged/src/jobs.rs Job::publish
tests: [AF-CONF-PB-017, crates/arkforged/tests/admission_surface.rs]

Each published `JobEvent` carries the digest of the journal record it was
published from. The reference daemon numbers events with its own per-job
counter, not the journal sequence; a consumer MUST treat
`sequence` as monotonic per job and MUST NOT assume it equals the journal
sequence. After restart, the next event cursor MUST be greater than every
recovered event cursor; the count of reconstructed events is not a cursor.

### AF-ENG-017 — ActionReceipt events are durably replayable
status: normative
source: crates/arkforged/src/jobs.rs complete_dispatch, submit_control_receipt, recover_receipt_events
tests: [crates/arkforged/tests/admission_surface.rs a_job_dispatches_every_step_and_reaches_a_verdict_on_each, ArkDeck ArkForgeFlashSessionContractTests testPassiveObservationConsumesReconciledTerminalAndRestartedReceipt]

Before publishing an `ACTION_RECEIPT`, the daemon MUST put the exact canonical
AF-DIG-012 body and the reserved event cursor in the durable
`semanticReceiptRecorded` record as lower-case `receiptBodyHex` and decimal
`receiptEventSequence`. The live event's `journal_record_sha256` MUST identify
that semantic-receipt record, not a later consumption or checkpoint marker.

On restart, a receipt may be reconstructed only when its body is canonical and
model-valid, its job/plan/step/attempt/permit identities equal the surrounding
journal facts, and its recomputed digest equals `receiptDigest`. Invalid or
legacy metadata is not invented. Valid reconstructed receipts retain their
original cursor and are emitted before a later terminal classification. This is
event replay, never permission to dispatch, synthesize a missing checkpoint, or
infer job success.

## Journal records

### AF-JRN-001 — record model
status: normative
tests: [AF-CONF-JOURNAL-001]

A record is the map of `model/digest-bodies.cddl#journal-record`:
`schemaVersion` (1), `sequence` (1-based), `jobRevision`, `kind`, `fsyncPolicy`,
`atEpochMs`, `subject`, `facts` (text → text map), `previousDigest`,
`recordDigest`.

### AF-JRN-002 — record digest and chain
status: normative
tests: [AF-CONF-JOURNAL-001]

`recordDigest = SHA-256("arkforge/v1/journal-record\0" || cbor(body without
recordDigest))`. `previousDigest` of record *n* MUST equal `recordDigest` of
record *n−1*; record 1 links to 32 zero bytes. Sequence numbers are contiguous
from 1.

### AF-JRN-003 — twenty-one record kinds
status: normative
tests: [AF-CONF-JOURNAL-002]

The kinds and their spellings are the `journal-record-kind` vocabulary. A kind
outside it MUST be refused on replay (`JOURNAL_RECORD_MALFORMED{kind}`).

### AF-JRN-004 — facts
status: normative
tests: [AF-CONF-JOURNAL-001]

`facts` keys are OpaqueIds and values are text. The engine-defined keys are
`jobId`, `planId`, `stepId`, `attemptId`, `permitId`, `receiptDigest`,
`receiptBodyHex` and `receiptEventSequence`; a port MUST spell them exactly so,
because recovery reads them. Every record in one admit → dispatch → receipt →
checkpoint transaction repeats the first five keys; the final three receipt
keys begin at `semanticReceiptRecorded` and only `receiptDigest` is repeated in
the compact consumption/checkpoint markers.

### AF-JRN-005 — fsync policy is a function of kind
status: normative
tests: [AF-CONF-JOURNAL-002]

`preflightObserved`, `stepAdmissionRequested`, `transportEvidenceRecorded`,
`rebindObserved`, `readOnlyObservationRecorded` are `buffered`; every other
kind is `durable`. The rule: a record whose loss would let the daemon dispatch
twice, or forget that it dispatched once, is durable.

### AF-JRN-006 — a misdeclared policy is tampering
status: normative
tests: [AF-CONF-JOURNAL-008]

A record whose `fsyncPolicy` is not the policy its kind requires MUST be refused
on replay (`JOURNAL_FSYNC_POLICY_MISDECLARED`).

### AF-JRN-010 — file format
status: normative
tests: [AF-CONF-JOURNAL-003, AF-CONF-JOURNAL-012]

A journal file begins with the 8 ASCII bytes `ARKFJRN1`. Each record is one
frame: a 4-byte big-endian unsigned length followed by exactly that many bytes
of the canonical record. A file whose first 8 bytes are not the magic (including
a shorter file that is not empty) MUST be refused (`JOURNAL_NOT_A_JOURNAL`); an
empty file is a new journal.

### AF-JRN-011 — frame length bound
status: normative
tests: [AF-CONF-JOURNAL-010, AF-CONF-JOURNAL-011]

A frame length of 0 or greater than 1 048 576 MUST be refused before any
allocation (`JOURNAL_FRAME_LENGTH_INVALID`). Length and body MUST be written in
one write call so that a crash leaves a short file, never a valid length
pointing at absent bytes.

### AF-JRN-012 — torn tail
status: normative
tests: [AF-CONF-JOURNAL-004]

On open, a trailing incomplete frame (fewer bytes than its length declares, or
fewer than 4 bytes where a length should be) is a torn write: the complete
frames before it MUST be replayed, the tail MUST be truncated away and reported
(`tornTailBytes`). There is no third outcome: a prefix is accepted or the file
is refused; a middle record is never silently dropped.

### AF-JRN-013 — tampered record
status: normative
tests: [AF-CONF-JOURNAL-005]

A record whose recomputed digest differs from its `recordDigest` MUST be refused
(`JOURNAL_RECORD_TAMPERED`).

### AF-JRN-014 — broken sequence
status: normative
tests: [AF-CONF-JOURNAL-006]

### AF-JRN-015 — broken chain
status: normative
tests: [AF-CONF-JOURNAL-007]

A record that is self-consistent but links to a digest other than its
predecessor's MUST be refused (`JOURNAL_CHAIN_BROKEN`). Sequence is checked
before chain, chain before digest.

### AF-JRN-016 — unknown schema version fails closed
status: normative
tests: [AF-CONF-JOURNAL-009]

### AF-JRN-017 — missing field is named, never defaulted
status: normative
tests: [AF-CONF-JOURNAL-013]

### AF-JRN-018 — durability before effect
status: normative
tests: []

`append` of a durable record MUST NOT return before the record is on stable
storage (see `ports/durability.md` for what "stable" promises). If `append` did
not return, no external effect may follow. Buffered records are flushed behind
the next durable one; correctness never depends on a shutdown flush.

The implementation MUST encode and size-check the complete frame before it
advances its in-memory chain, and MUST commit that chain only after the complete
frame write succeeds. A write or synchronization error poisons the open handle:
no later append is allowed until `open` verifies or truncates the uncertain tail.

### AF-JRN-019 — one journal per job
status: draft
source: crates/arkforged/src/jobs.rs JobRegistry::start
tests: []

The reference daemon keeps one file `<jobId>.journal` per job under its runtime
directory. The engine's derivation functions nonetheless filter by `jobId` so
that a combined journal would still derive per job: an explicit `jobId` fact is
authoritative; without one, `subject` identifies the job. A one-job journal
also accepts historical step-scoped records that have neither association.

## Crash windows

### AF-CRASH-001 — every truncation point is enumerated
status: normative
tests: [AF-CONF-JOURNAL-004]

An implementation MUST pass the exhaustive torn-tail table: for every byte
length of the fixture file, the open result is the one recorded.

### AF-CRASH-002 — disposition is derived from the journal, not from memory
status: normative
tests: [AF-CONF-CRASH-001..020]

After a restart the crash disposition for a job MUST be derived from its
replayed records alone. The rows below are `state-machines/crash-disposition.yaml`.

### AF-CRASH-R-001 — no job
status: normative
tests: [AF-CONF-CRASH-001, AF-CONF-CRASH-002]

No `jobCreated` record for the job → nothing happened; plan and start again.

### AF-CRASH-R-002 — safe to cancel
status: normative
tests: [AF-CONF-CRASH-003, AF-CONF-CRASH-004, AF-CONF-CRASH-017]

A job exists and no record for it mentions a `permitId` (or the newest such
permit is `unseen`) → no external intent exists; the job is safe to cancel.

### AF-CRASH-R-003 — dispatch forbidden until intent durable
status: normative
tests: [AF-CONF-CRASH-005]

Newest permit is `acceptedIntentNotDurable` → dispatch is forbidden; the same
permit id MAY be re-recorded as the same intent, a second intent MUST NOT be
created; otherwise let the permit expire.

### AF-CRASH-R-004 — outcome unknown
status: normative
tests: [AF-CONF-CRASH-006..008]

Newest permit is `intentDurable` or `consumingOutcomeUnknown` → whether the
device was touched is unknown; reconcile, never replay. From the journal alone,
"about to dispatch" and "dispatched" are indistinguishable, so the engine's
derivation treats both as unknown.

### AF-CRASH-R-005 — durable receipt with checkpoint missing
status: normative
tests: [AF-CONF-CRASH-009, AF-CONF-CRASH-010]

Newest permit has a durable `semanticReceiptRecorded` (whether or not the
following `permitConsumed` marker was appended) and no `stepCheckpointed`
names it → the permit is settled and MUST NOT dispatch again, but the job is not
terminal. Replay the receipt only if AF-ENG-017 validates it, classify the job
`outcomeUnknown`, and use read-only reconcile. A restart MUST NOT synthesize the
missing checkpoint or infer success from the receipt marker alone.

### AF-CRASH-R-006 — replay from checkpoint
status: normative
tests: [AF-CONF-CRASH-011, AF-CONF-CRASH-019]

Newest permit is `consumed` and checkpointed → replay validated receipt events
to the authority; do not re-execute. In the absence of a durable terminal
`outcomeClassified`, checkpoint count alone MUST NOT be converted to success:
the last settled receipt could describe semantic success or confirmed failure.

### AF-CRASH-R-007 — concluded
status: normative
tests: [AF-CONF-CRASH-012..016, AF-CONF-CRASH-021]

The first `outcomeClassified` record whose `outcome` is a terminal state
concludes the job in that state. A later terminal classification MUST NOT
rewrite that durable result.

### AF-CRASH-R-008 — the newest permit decides
status: normative
tests: [AF-CONF-CRASH-018, AF-CONF-CRASH-019]

With several permits, the disposition is derived from the permit named by the
newest record that carries a `permitId`.

### AF-CRASH-R-009 — other jobs' records are ignored
status: normative
tests: [AF-CONF-CRASH-020]

If a record carries a `jobId` fact, that fact is authoritative even when its
`subject` happens to equal another job id. Without the fact, `subject` is used.
In a one-job journal, legacy untagged step-scoped records belong to the sole
`jobCreated` job.

### AF-CRASH-010 — no disposition permits a new external effect
status: normative
tests: [AF-CONF-CRASH-001..020]

Every row's `permitsExternalEffect` is false. Even AF-CRASH-R-003 permits only
finishing the record of the intent, never a dispatch.

### AF-CRASH-011 — restart policy of the reference daemon
status: draft
source: crates/arkforged/src/jobs.rs recover_job
tests: []

On restart the reference daemon does not resume any job. It preserves the first
durable terminal classification (or an earlier non-terminal outcome when no
terminal classification followed); otherwise it calls the same
`CrashDisposition::derive` reducer as the engine. `safeToCancel` and an accepted
permit without a durable intent conclude `cancelledSafe`; `intentDurable` and
`consumingOutcomeUnknown` conclude `outcomeUnknown`. A durable receipt without
a checkpoint and a partial checkpoint also conclude `outcomeUnknown` until the
daemon can reconstruct the receipt/remaining continuation; neither is ever
called safe or replayed. Only a legacy journal with every declared step
checkpointed is migrated to `succeeded`.

The chosen result is appended once as `outcomeClassified` (subject = job id), so
a second restart is byte-stable. `every_durable_restart_prefix_is_classified_without_replaying_dispatch`
exercises every boundary above. A port MAY implement actual resumption for
AF-CRASH-R-005/006, but MUST NOT re-dispatch under a pre-restart permit.
