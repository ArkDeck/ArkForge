# Tasks — CHG-2026-CLI

> Status: implementing by explicit maintainer request. Native rescue is the first
> vertical; normal direct-flash authority follows separately.

## TASK-CLI-001 — One typed command tree

- Status: implementing (`rescue` subtree and its human/JSON help are implemented;
  the normal-flash/read-only tree remains pending)
- Build a new `arkforge` binary from one typed command definition.
- Generate parser metadata, human help, JSON help and shell completions from the
  same tree.
- Implement global output, error envelope and exit-code contracts.
- Add parse-only tests for every example and every invalid option relation.
- Acceptance: CLI-AC-01..04.

## TASK-CLI-002 — Consolidate current read-only commands

- Status: blocked
- Move public-socket discover/inspect/assess/job queries behind the new handlers.
- Move offline import/inspect and signing verification behind the new handlers.
- Add missing read APIs: probe, watch, reconcile and recovery guide.
- Delete old binary entry points after their behavior is available under `arkforge`;
  ArkForge is unreleased, so no wrappers or deprecated syntax are retained.
- Acceptance: CLI-AC-05..07.

## TASK-CLI-003 — Direct CLI authority

- Status: blocked
- Add an authority-side crate/boundary that can mint exact StepPermit bytes;
  keep all minting symbols unreachable from `arkforged`.
- Implement a persistent local authority supervisor, dedicated runtime pairing,
  local target binding, permit durability, event watching and typed managed-control
  receipts. Pairing secret remains in supervisor/daemon memory.
- Implement the HDC control port for enter Loader, exact rebind and build
  postflight without leaking paths/endpoints/connect keys/argv.
- Add the independent `AuthoritySupportKey` gate and seal it into executable plans;
  do not overload the existing mechanics `evidence_set_digest`.
- Implement `--detach` only as a presentation choice; the supervisor remains the
  authority. Define command disconnect, explicit cancel, same-epoch retransmit and
  supervisor/daemon epoch-rotation behavior.
- Acceptance: CLI-AC-08..14.

## TASK-CLI-004 — Normal `flash plan/apply`

- Status: blocked
- Implement exact artifact/profile/device/intent materialization.
- Derive acknowledgement tokens from the sealed effect set.
- Require plan digest and the exact token set at apply.
- Render ordered events and terminal outcome in human, JSON and JSONL modes.
- Preserve no-replay classification across CLI/daemon death.
- Acceptance: CLI-AC-15..19.

## TASK-CLI-005 — Separate native RockUSB rescue domain

- Status: software-complete; real-device campaign and release evidence remain pending
- Add `RescuePlan`, `RescueReceipt`, a separate store namespace and a closed
  native rescue executor that reuses `NativeRockUsbPort`/`RockUsbProtocol`.
- Seal the running ArkForge build digest, exact USB observation, profile, layout
  and image facts into each plan.
- Implement semantic read commands and two-phase write/reset.
- Forbid external device tools, device-tool subprocesses, arbitrary USB requests,
  raw LBA write and automatic normal fallback.
- Ensure normal receipt/maturity readers cannot decode rescue evidence.
- Acceptance: CLI-AC-20..27.

## TASK-CLI-006 — Hardware campaign and release gate

- Status: blocked
- Run the full CLI-authority DAYU200 campaign: normal-to-Loader, nine writes,
  read-domain-aware verification, reset, exact HDC postflight and receipts.
- Run a controlled native rescue campaign for each closed action.
- Run CLI/daemon crash, cable detach, multi-device ambiguity and cancel cases.
- Publish only the exact CLI authority support key against the already exact
  mechanics maturity key after maintainer review. Rescue remains separately classified.
- Acceptance: CLI-AC-28..32.

## Delivery order

~~~text
CLI-001 typed tree
  └─ CLI-002 read-only consolidation
       ├─ CLI-003 direct authority ── CLI-004 normal flash ──┐
       └─ CLI-005 explicit rescue ──────────────────────────┤
                                                            └─ CLI-006 hardware release gate
~~~

Normal flash and rescue may be implemented in parallel after the read-only
surface lands, but neither may be called supported before its own hardware and
negative-path acceptance is recorded.

## Old binary removal

Delete `arkforge-cli`, `arkforge-inspect` and `arkforge-signing` in the same change
that lands their canonical `arkforge` handlers. There is no release compatibility
gate because ArkForge has not shipped.
