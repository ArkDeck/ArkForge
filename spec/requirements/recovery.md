# Unknown outcomes, reconcile, superseding recovery
status: draft
area: REC
rationale: architecture.md §14, §20.2
conformance: crash, reconcile, action-receipt

### AF-REC-001 — never replay
status: normative
tests: [AF-CONF-STATEMACHINE-002, AF-CONF-CRASH-006..009]

After an `outcomeUnknown`, an implementation MUST NOT re-send the original
intent, re-use the original permit, treat a new `startExecution` of the same
plan as a retry, rewrite the original outcome, or rename a complete re-flash as
a retry. The original job and its journal stay immutable.

### AF-REC-002 — reconcile is read-only
status: normative
tests: [AF-CONF-RECONCILE-002..006]

`reconcileJob` MUST NOT request a mutating or destructive permit and MUST NOT
send erase, write, load-agent or reboot. It performs only the read-only
observations the profile declares and returns one of the `reconcile-verdict`
vocabulary: `succeeded`, `confirmedNotExecuted`, `confirmedPartial`,
`stillUnknown`, `nothingToReconcile`. Insufficient evidence keeps
`outcomeUnknown`.

### AF-REC-003 — possible effect set
status: normative
tests: [AF-CONF-RECONCILE-001]

Every unresolved action maps to a conservative `PossibleEffectSet`
(`model/digest-bodies.cddl#possible-effect-set`): `{effects, completeness,
sourceActionIds}` with `completeness` ∈ {`bounded`, `unbounded`}. Optional and
conditional effects are included unless durable evidence proves they did not
happen. `unbounded` makes recovery ineligible.

### AF-REC-004 — superseding recovery is a distinct plan
status: normative
tests: [AF-CONF-PLAN-009]

A recovery plan MUST have a new plan id and digest, `executionPurpose =
supersedingRecovery`, cover every effect in the uncertain set, include
per-effect verification and postflight, and reference no permit or intent of the
superseded job. Whether it is admitted is the authority's decision; ArkForge
only reports `eligible` / `ineligible{blockerCode, blockerReason}`.

### AF-REC-005 — coverage declaration is published data
status: normative
tests: [AF-CONF-PLAN-002]

`recovery.supportsCompleteOverwrite`, `coveredEffects` and `unsupportedStates`
come from the DeviceProfile and are digested under
`"arkforge/v1/recovery-coverage\0"`; a caller flag cannot widen them.

### AF-REC-006 — recovery guide
status: draft
source: proto/arkforge.proto RecoveryGuide
tests: []

`getRecoveryGuide` always reports `originalOutcomeImmutable = true` and
`automaticReplayForbidden = true`, lists typed operator actions, and says whether
complete overwrite is supported with the contract id/version/digest.

### AF-REC-007 — settled non-success dispositions are confirmed failures
status: normative
source: crates/arkforged/src/jobs.rs complete_dispatch
tests: [AF-CONF-RECEIPT-003]

A dispatch disposition of `confirmedNoEffect` or `confirmedPartialEffect` is a
settled ActionReceipt: write the receipt, consume the permit, checkpoint, then
conclude `confirmedFailed` with the exact disposition in `actionDisposition`.
Only `outcomeUnknown` moves directly from `dispatching` to `outcomeUnknown`
without a settled receipt. No disposition permits redispatch.

### AF-REC-008 — controller restart reuses the exact daemon job
status: normative
tests: []

After `startExecution` returns, a controller MUST durably correlate its attempt
with the returned daemon job id, ArkForge plan id/digest, execution purpose,
artifact identity, target binding and toolchain identity before it may submit a
permit. A recovered controller with that correlation MUST observe the exact job;
it MUST NOT call `startExecution` as a retry or derive authority from a
process-local receipt cache. A terminal semantic receipt MUST become durable on
the controller side before the controller records its own successful outcome.

A controller crash before the correlation is durable may leave an orphan daemon
job, but no external effect: no permit was yet eligible for submission. A crash
after correlation preserves one effect lineage. Recovery may correlate a
canonical receipt to the original intent or perform an independent read-only
reconcile; it never creates a replacement attempt implicitly.

If both controller and daemon restart after a terminal receipt was emitted, the
daemon MUST replay that receipt from AF-ENG-017 durable metadata before its
terminal classification. The controller MUST validate the replayed receipt
against its stored correlation and persist it before completing the original
intent. An earlier `outcomeUnknown` observation is not final when the same
finite event poll later contains a durable `succeeded` or `confirmedFailed`;
passive recovery consumes the complete poll and uses the latest classification.
