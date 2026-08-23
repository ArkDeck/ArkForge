# Authority boundary: admission, freshness, StepPermit
status: normative
area: AUTH
rationale: architecture.md §8, §15.2
conformance: admission, permit, crash

ArkForge asks; an authority decides. The daemon can verify a permit and can
never mint one. A permit is single-use, bound to one exact dispatch, endorsed by
a pairing secret that rotates on every restart, and carried on the wire as the
exact bytes the authority signed.

## Admission snapshot and freshness

### AF-AUTH-001 — admission snapshot digest body
status: normative
tests: [AF-CONF-ADMISSION-001, AF-CONF-ADMISSION-002]

The snapshot digested under `"arkforge/v1/admission-snapshot\0"` is
`model/digest-bodies.cddl#admission-snapshot`. `transportSessionDigest` is CBOR
`null` when no session is open; the key is never omitted.

### AF-AUTH-002 — what the snapshot carries on the wire
status: normative
tests: [AF-CONF-PB-008]

The IPC `StepAdmissionSnapshot` carries the raw inputs to
`admittedDeviceFactsDigest` (topology, descriptor, serial digest and kind,
protocol identity, identity strength, malformed flag, transport session digest)
so the authority recomputes the digest and checks the facts against its own
binding. A snapshot that is merely echoed back proves nothing.

### AF-AUTH-003 — continuity is checked before the clock
status: normative
tests: [AF-CONF-ADMISSION-003..015]

Freshness is evaluated in this order, first failure wins: detach observed →
transport session digest differs (including present→absent) → device facts →
provider facts → toolchain facts → artifact facts → wall-clock deadline.

### AF-AUTH-004 — stale is not broken
status: normative
tests: [AF-CONF-ADMISSION-004, AF-CONF-ADMISSION-005]

When continuity holds and `now >= freshnessDeadlineEpochMs`, the verdict is
`staleSnapshot`: the daemon MUST take a new snapshot and ask again, MUST NOT
consume the authority's answer, and MUST NOT charge a destructive budget or
blame the device.

### AF-AUTH-005 — broken continuity means zero dispatch
status: normative
tests: [AF-CONF-ADMISSION-006..015]

Any `continuityBroken` verdict forbids dispatch under the pending permit; the
permit is left unconsumed and the job reports the break.

### AF-AUTH-006 — snapshot lifetime
status: draft
source: crates/arkforged/src/jobs.rs SNAPSHOT_LIFETIME_MS, StepAdmissionSnapshot::is_fresh_at
tests: []

The reference daemon publishes `snapshotLifetimeMs = 60000` and treats a
snapshot as fresh while `now < observedAtEpochMs + snapshotLifetimeMs`; an
overflowing sum is never fresh. The value is a per-step budget, not a global
constant a spec revision may tighten without hardware evidence
(architecture.md §8.3).

## Permit body and tag

### AF-AUTH-010 — permit signing body
status: normative
tests: [AF-CONF-PERMIT-001..003]

The signing body is `model/digest-bodies.cddl#permit-signing-body`: a
deterministic CBOR map of exactly sixteen keys (`permitId`, `authorityNamespace`,
`controllerSessionId`, `jobId`, `planId`, `planDigest`, `stepId`, `attemptId`,
`publicStepDigest`, `privateActionDigest`, `effectSetDigest`, `authorityBinding`,
`admittedDeviceFactsDigest`, `issuedAtEpochMs`, `expiresAtEpochMs`, `singleUse`).
The integrity tag is NOT in the body.

### AF-AUTH-011 — integrity tag
status: normative
tests: [AF-CONF-PERMIT-001..003]

`integrityTag = HMAC-SHA-256(pairingSecret, signingBody)` (AF-DIG-002), carried
beside the body together with the `pairingEpoch` it was minted under.

### AF-AUTH-012 — the bytes on the wire are the bytes that were signed
status: normative
tests: [AF-CONF-PERMIT-027, AF-CONF-PB-009]

A permit travels as `permit_cbor` (the exact signing body) plus
`integrity_tag` plus `pairing_epoch`. It MUST NOT be re-encoded field by field
by any intermediary; retransmission replays the stored bytes (AF-AUTH-023).

### AF-AUTH-013 — verification
status: normative
tests: [AF-CONF-PERMIT-004..006]

A permit verifies for a dispatch intent only if the tag recomputed over the
received body under the current pairing secret equals the carried tag.

### AF-AUTH-014 — any signed field change voids the tag
status: normative
tests: [AF-CONF-PERMIT-005]

Altering any field of the body after minting MUST make verification fail;
there is no field the daemon may "fix up".

### AF-AUTH-015 — pairing epoch
status: normative
tests: [AF-CONF-PERMIT-007]

A permit whose `pairingEpoch` differs from the verifier's current epoch MUST be
refused (`PERMIT_STALE_PAIRING_EPOCH`). The epoch rotates whenever either
process restarts; an unconsumed permit from an earlier epoch can never be
consumed for the first time — admission has to run again.

### AF-AUTH-016 — expiry boundary
status: normative
tests: [AF-CONF-PERMIT-008, AF-CONF-PERMIT-009]

A permit is expired when `now >= expiresAtEpochMs`; `now == expiresAtEpochMs - 1`
is valid.

### AF-AUTH-017 — single use survives restarts
status: normative
tests: [AF-CONF-PERMIT-010, AF-CONF-CRASH-010, AF-CONF-CRASH-011]

A permit whose id the durable journal has already mentioned in any record
other than admission MUST NOT be dispatched again; if a receipt is durable the
original receipt is returned. "Already seen" is read from the journal, never
from memory (AF-AUTH-P-001).

### AF-AUTH-018 — `singleUse` must be true
status: normative
tests: [AF-CONF-PERMIT-011]

A permit with `singleUse = false` MUST be refused even when its tag verifies.

### AF-AUTH-019 — context binding
status: normative
tests: [AF-CONF-PERMIT-012..023]

Authenticity is not authorization for a different dispatch. Every one of
`planDigest`, `privateActionDigest`, `authorityNamespace`, `controllerSessionId`,
`jobId`, `planId`, `stepId`, `attemptId`, `publicStepDigest`, `effectSetDigest`,
`authorityBinding` (all four fields) and `admittedDeviceFactsDigest` MUST equal
the pending dispatch's value; a mismatch MUST name the field.

### AF-AUTH-020 — check order
status: normative
tests: [AF-CONF-PERMIT-024..026]

Checks run in this order, first failure reported: already consumed → not
single use → stale pairing epoch → integrity tag → expiry → plan digest →
private action digest → authority namespace → controller session → job → plan id
→ step → attempt → public step digest → effect set digest → authority binding →
admitted device facts digest.

### AF-AUTH-021 — decoding is strict and typed
status: normative
tests: [AF-CONF-PERMIT-028..048]

Every body field is required and typed (text, 32-byte bytes, unsigned, bool,
nested map). A missing field, wrong type, short digest or out-of-grammar
identifier MUST be refused naming the field (`PERMIT_DECODE_FIELD`). Nothing is
defaulted.

### AF-AUTH-022 — non-canonical bytes are refused
status: normative
tests: [AF-CONF-PERMIT-044, AF-CONF-PERMIT-049]

If re-encoding the decoded permit does not reproduce the received bytes, the
permit MUST be refused (`PERMIT_DECODE_NOT_CANONICAL`) — including bytes that
carry an extra unknown key.

### AF-AUTH-023 — minting and retransmission
status: normative
tests: []

Only an authority adapter mints. The authority MUST persist the complete permit
(body, tag, epoch) before returning it and MUST replay the stored bytes on
retransmission; deterministic re-derivation is forbidden because two
byte-different copies of "the same" permit is the ambiguity the tag exists to
remove. The daemon MUST NOT reference the minting code path (architecture guard).

### AF-AUTH-024 — a refusal is an answer
status: draft
source: crates/arkforged/src/jobs.rs JobRegistry::submit_permit
tests: []

`SubmitStepPermitRequest.refusal` (non-empty, no permit) concludes the pending
admission: nothing was recorded that could touch the device, so the job becomes
`cancelledSafe` with the refusal as reason. Silence is not a refusal; the daemon
keeps waiting until the snapshot goes stale.

### AF-AUTH-025 — the daemon never calls out
status: normative
tests: []

Every authority interaction is client-initiated: the daemon *asks* on the
`watchJob` stream (`STEP_ADMISSION_REQUESTED`, `MANAGED_CONTROL_REQUESTED`) and
the authority calls back in (`submitStepPermit`, `submitManagedControlReceipt`).
The daemon MUST NOT open outbound connections to the authority.

## Permit ledger

### AF-AUTH-P-001 — permit disposition from the journal
status: normative
tests: [AF-CONF-CRASH-001..020]

For each permit id the journal mentions, the disposition is the last of:
`stepPermitAccepted` → `acceptedIntentNotDurable`; `stepIntentRecorded` →
`intentDurable`; `permitConsuming` or `externalDispatchStarted` →
`consumingOutcomeUnknown`; `permitConsumed` → `consumed{receiptDigest}`. A
permit never mentioned is `unseen`. Only `intentDurable` permits a fresh
dispatch; `unseen` is the only state admission may treat as new.

### AF-AUTH-P-002 — unresolved permits
status: normative
tests: [AF-CONF-CRASH-005..009]

`consumingOutcomeUnknown` and `acceptedIntentNotDurable` are *unresolved*: each
is a possible external effect to reconcile, never to retry. `intentDurable` is
not unresolved by the ledger's definition; see `state-machines/crash-disposition.yaml`
and ISSUES SI-003 for how the two readers of this state differ.
