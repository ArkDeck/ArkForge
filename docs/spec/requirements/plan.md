# Plans, projection and effects
status: normative
area: PLAN, PROJ, EFF
rationale: architecture.md §5, §6, §15.4, AFD-0004
conformance: plan

A plan is an immutable description of what will be done to one device with one
artifact under one authority binding. It exists in two halves: the **public
plan** (steps and effects the authority admits and audits) and the **private
execution plan** (provider-shaped actions that never leave the daemon). The two
are bound by digests so that neither can change without the other noticing.

## PlanAssessment versus executable plan

### AF-PLAN-001 — an incomplete combination yields an assessment, never a plan
status: normative
tests: [AF-CONF-PLAN-004]

Materialization MUST return a PlanAssessment (no plan id, no plan digest) and
MUST NOT return an executable plan when any of the following holds: the
mechanics maturity of the exact combination is not `productionVerified` or
`hardwareCampaign`; the authority-support state is not `productionVerified` or
`hardwareCampaign`; any data-impact axis is `unknown`; any execution-relevant
artifact unknown is open; any private action lacks a public projection; any
semantic target is outside the profile allowlist.

### AF-PLAN-002 — every unknown names the evidence that would close it
status: normative
tests: [AF-CONF-PLAN-004]

A PlanAssessment MUST carry one `evidenceRequirements` entry per `unknowns`
entry, and `availability` MUST be `unavailable` with a reason whenever
`unknowns` is non-empty.

### AF-PLAN-003 — a public caller receives at most an assessment
status: draft
source: crates/arkforge-ipc/src/lib.rs SessionKind::may_call; crates/arkforged/src/service.rs
tests: []

`materializePlan` on a public session MUST return an assessment even when the
daemon could build an executable plan; executable plans are controller-only.

## Public steps

### AF-PLAN-010 — public step digest
status: normative
tests: [AF-CONF-PLAN-006]

`publicStepDigest = SHA-256("arkforge/v1/public-step\0" || cbor(public-step))`
with the body of `model/digest-bodies.cddl#public-step`. Optional fields
(`semanticTarget`, `contentDigest`, `expectedModeBefore`, `expectedModeAfter`)
are encoded as CBOR `null` when absent; the key is never omitted.

### AF-PLAN-011 — step self-consistency
status: normative
tests: [AF-CONF-PLAN-006]

A step's `effect` MUST NOT be below the minimum its `kind` implies
(`model/vocabularies.yaml#flash-step-kind`): `eraseTarget`/`writeTarget` are
`destructive`; `ensureMode`/`loadEphemeralAgent`/`awaitRebind`/`reboot` are at
least `transient`; probe/validate/verify/postflight are at least `readOnly`.
`writeTarget` MUST carry a `contentDigest`; `writeTarget`, `eraseTarget` and
`verifyTarget` MUST carry a `partition` or `rawRegion` semantic target.

### AF-PLAN-012 — step order is execution order
status: normative
tests: [AF-CONF-PLAN-006]

`publicSteps` is ordered. The daemon executes steps in array order and never
reorders, skips or merges them.

## The sealed plan

### AF-PLAN-020 — plan digest preimage
status: normative
tests: [AF-CONF-PLAN-009]

`planDigest = SHA-256("arkforge/v1/plan\0" || cbor(plan-body))` with the body
of `model/digest-bodies.cddl#plan-body`. The body does not contain the plan
digest itself, the per-action binding list, or the private plan; it contains
`providerExecutionPlanDigest` and `publicProjectionDigest`, which cover them.

### AF-PLAN-021 — maturity and authority support are sealed
status: normative
tests: [AF-CONF-PLAN-009]

`maturity` (state, blocker, campaign) and `authoritySupport` (key digest, state,
campaign, blocker) are part of the plan body. Two plans identical in every other
field but sealed under `hardwareCampaign` and `productionVerified` MUST have
different digests, so that a campaign's permits and receipts can never be
presented as production evidence.

### AF-PLAN-022 — a stored plan is re-verified before use
status: normative
tests: [AF-CONF-PLAN-009]

Before a stored plan is executed, continued, or returned, the implementation
MUST recompute its digest from its contents and refuse the plan on mismatch
(`PLAN_DIGEST_MISMATCH` / store corruption). A caller's expected digest MUST
also match the stored one.

### AF-PLAN-023 — expiry
status: normative
tests: []
gap: no fixture; boundary is by construction `now >= expiresAtEpochMs`.

A plan with `expiresAtEpochMs <= createdAtEpochMs` MUST NOT seal. A plan is
expired at `now >= expiresAtEpochMs` (wall clock, milliseconds); starting an
expired plan MUST be refused with `PLAN_EXPIRED`.

### AF-PLAN-024 — execution purpose
status: normative
tests: [AF-CONF-PLAN-009]

`executionPurpose` is `primaryFlash` or `supersedingRecovery`. A
`supersedingRecovery` plan is a new plan with a new id and digest; it MUST NOT
reference the permits or intents of the plan whose outcome it supersedes.

## Projection (public ↔ private)

### AF-PROJ-001 — private action digest
status: normative
tests: [AF-CONF-PLAN-005]

`privateActionDigest = SHA-256("arkforge/v1/private-action\0" || cbor(private-action))`
with the body of `model/digest-bodies.cddl#private-action`. The provider-shaped
`body` is part of the digest; the daemon stores the full record, the authority
sees only the digest.

### AF-PROJ-002 — every step has exactly one primary action
status: normative
tests: [AF-CONF-PLAN-005, AF-CONF-PLAN-007]

For each public step there MUST be exactly one private action with role
`primaryEffect` and that `stepId`. Any further actions for the step MUST have
role `readOnlyTransportSubAction` and MUST NOT declare a persistent effect. A
private action whose `stepId` matches no public step MUST be refused.

### AF-PROJ-003 — private facts stay inside public facts
status: normative
tests: []
gap: covered by Rust unit tests in `arkforge-core::projection`; no fixture yet.

A primary action's declared target MUST equal its step's semantic target; its
declared range MUST lie within the range the public effect set declares for
that target; its content digest MUST equal the step's `contentDigest`.

### AF-PROJ-010 — per-action binding order
status: normative
tests: [AF-CONF-PLAN-007]

The binding list is built by walking public steps in order; for each step the
primary action first, then its sub-actions in private-plan order. Each entry is
`{stepId, actionId, role, privateActionDigest}`.

### AF-PROJ-011 — provider execution plan digest
status: normative
tests: [AF-CONF-PLAN-007]

`providerExecutionPlanDigest = SHA-256("arkforge/v1/provider-execution-plan\0"
|| d1 || d2 || … )` over the binding list's private action digests in order
(AF-DIG-009).

### AF-PROJ-012 — public projection digest
status: normative
tests: [AF-CONF-PLAN-007]

`publicProjectionDigest = SHA-256("arkforge/v1/public-projection\0" ||
cbor(array of binding maps))` with the map of `model/digest-bodies.cddl#action-digest-binding`.

### AF-PROJ-013 — the public step binds its primary action
status: normative
tests: [AF-CONF-PLAN-006]

`publicStep.privateActionDigest` MUST equal the digest of the step's primary
action. A mismatch refuses the plan.

### AF-PROJ-014 — no re-lowering after sealing
status: normative
tests: []

After a plan is sealed the private plan MUST NOT be regenerated, re-lowered or
re-interpreted by a different provider build; the daemon executes the stored
action bytes whose digest the permit names (AF-AUTH-019).

## Effects

### AF-EFF-001 — effect set digest
status: normative
tests: [AF-CONF-PLAN-008]

`effectSetDigest = SHA-256("arkforge/v1/effect-set\0" || cbor(effect-set))`
with `{persistent: [...], transient: [...], dataImpact: {...}}` per
`model/digest-bodies.cddl#effect-set`. Effect lists keep the order the provider
declared them in.

### AF-EFF-002 — closed effect vocabulary
status: normative
tests: [AF-CONF-PLAN-008]

Persistent effects are exactly `erasePartition`, `writePartition`,
`writeRawRegion`, `replacePartitionTable`, `changeBootMetadata`; transient
effects are exactly `enterMode`, `loadEphemeralAgent`, `usbDetachReattach`,
`reboot`. Data impact has exactly four axes — `userdata`, `calibration`,
`nonVolatileConfig`, `secureStorage` — each `preserved`, `overwritten` or
`unknown`.

### AF-EFF-003 — ranges
status: normative
tests: []

A `ByteRange` is `{start, length}` in bytes with `length > 0` and
`start + length` not overflowing 64 bits. Wildcard or open-ended ranges do not
exist in the model.

### AF-EFF-010 — executability gate on effects
status: normative
tests: [AF-CONF-PLAN-004]

An effect set with any data-impact axis `unknown` MUST NOT seal into an
executable plan. Every public step with effect ≥ `mutating` MUST be covered by
at least one persistent or transient effect naming its semantic target.

### AF-EFF-011 — destructive means persistent
status: normative
tests: []

An effect set is destructive exactly when its persistent list is non-empty.
Steps with effect ≥ `mutating` require a StepPermit before dispatch
(AF-AUTH-010).
