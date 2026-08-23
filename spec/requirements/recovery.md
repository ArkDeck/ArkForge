# Unknown outcomes, reconcile, superseding recovery
status: draft
area: REC
rationale: architecture.md §14, §20.2
conformance: crash (dispositions); no reconcile fixture yet (see gaps)

### AF-REC-001 — never replay
status: normative
tests: [AF-CONF-STATEMACHINE-002, AF-CONF-CRASH-006..009]

After an `outcomeUnknown`, an implementation MUST NOT re-send the original
intent, re-use the original permit, treat a new `startExecution` of the same
plan as a retry, rewrite the original outcome, or rename a complete re-flash as
a retry. The original job and its journal stay immutable.

### AF-REC-002 — reconcile is read-only
status: normative
tests: []
gap: reconcile fixture requires a provider transcript; planned for v1.1.

`reconcileJob` MUST NOT request a mutating or destructive permit and MUST NOT
send erase, write, load-agent or reboot. It performs only the read-only
observations the profile declares and returns one of the `reconcile-verdict`
vocabulary: `succeeded`, `confirmedNotExecuted`, `confirmedPartial`,
`stillUnknown`, `nothingToReconcile`. Insufficient evidence keeps
`outcomeUnknown`.

### AF-REC-003 — possible effect set
status: normative
tests: []

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

### AF-REC-007 — receipt dispositions that are not success
status: draft
source: crates/arkforged/src/jobs.rs complete_dispatch
tests: []

A dispatch disposition of `confirmedNoEffect`, `confirmedPartialEffect` or
`outcomeUnknown` moves the job to `outcomeUnknown` and records the disposition
verbatim in the `outcomeClassified.outcome` fact. The state collapses the three
(ISSUES SI-006); the record preserves the distinction for reconcile.
