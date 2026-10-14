# Durable private plans after daemon restart

Status: implementation regressions and published-runtime restart validation;
complete Windows flash acceptance remains blocked.

The Windows native flash campaign exposed that journals survived daemon restart
while the private execution plans existed only in memory. Recovery could still
show an unknown job but could not recover its sealed action records or coverage
reference. Re-lowering the plan would violate AF-PROJ-014.

The daemon now supplies an immutable filesystem store to the engine. It commits
the canonical envelope preimage and original private action records before
materialization returns. On startup the engine validates the envelope digest,
all private/public bindings, and both projection digests before loading any
plan. Incompatible, truncated, oversized, renamed, or tampered committed files
refuse startup. Unpublished temporary files are never adopted. Existing plan
identifiers cannot be overwritten with different bytes.

Platform permissions, atomic publication and directory synchronization remain
in `arkforged`; `arkforge-engine` depends only on Core and the authority API.
An initial local architecture check caught a platform dependency in the engine;
the implementation was moved behind the storage port without changing the guard.

Restart restores plan data, not dispatch queues, device sessions, or permits.
The daemon regression persists a real provider-produced fixture plan, leaves a
managed control unresolved, restarts twice, verifies the original action bytes
and recovery contract, and confirms the job stays `outcomeUnknown` with no
dispatch, preparation or reconcile work. The journal does not change on the
second restart or on recovery-guide reads. A same-id/version profile with a
different digest cannot supply recovery coverage for the original envelope.

A separate interrupted-write fixture proves that the restarted service can
still assess complete-overwrite coverage from the exact stored plan. Removing
only that fixture's plan bytes makes the same journal fail closed with
`effectsUnbounded`; no provider lowering backfills the missing file.

The older campaign job predates this store. Its absent private bytes remain
absent; this change does not reconstruct them, rewrite its outcome, infer
success, or authorize another device write. A separate recovery assessment must
still establish every required fact before any destructive dispatch.

## Published Runtime restart validation

[PR #13](https://github.com/ArkDeck/ArkForge/pull/13) was published as
`48a6bc8d32e5ac745b31fc793bae2a044edf1cb1`. An unsigned native Windows build
from that clean revision was used only for the existing named campaign.
The build exited 0 in 15.224 seconds. Its SHA-256 values are:

- CLI: `385b09a21711137a4625f978cd5731083288523a07112cefef45507b2fb0e04d`.
- Daemon: `82737dcbc149ce09c2540798a30151904a8583b18bc746ecce4ece1dd57308ce`.

The Runtime observed the connected DAYU200 in `hdc-normal` with
`serialAndTopology` identity. Typed `flash plan` used the existing immutable
OpenHarmony 7.0.0.43 archive, SHA-256
`781d2eaebe2f8d10a25d13102a38259b134210480b2153786032a3bdea675fed`, and published
profile `org.openharmony.dayu200@1.0.0`. Planning exited 0 and materialized a
23-step full-restore plan. It did not apply that plan or create a job.

The Runtime, not the caller, wrote an 18,594-byte private plan file with SHA-256
`b8ecd2e1e572c19aea0a3992e2770610a54a69ec971b528d6a70d3fdd7fe9787`.
The sealed plan digest is
`7409f50cfa33120c15c448b202d6799dbdab761a7367395d3c3600dbc1894a1d`.
After a typed daemon stop and restart in the same runtime directory, pairing
epoch advanced from 7 to 8, startup validation succeeded, and the private file
was byte-for-byte unchanged. Both original journal hashes were also unchanged.
The original unknown job stayed `outcomeUnknown`, with zero active jobs, zero
new jobs and zero device writes during this validation. Restart did not
recreate the execution queue or replay the original intent.

A fresh typed `job recover` assessment for the old unknown job exited 3 with
`nothingToRecover` and produced no recovery plan or apply command. This refusal
does not mean that the old outcome became known: the old private plan predates
the store, its bytes remain missing, and the closed managed-control fallback
cannot supply a superseding plan. The successful new plan-storage check does
not repair those historical bytes or prove recovery eligibility for that job.

The operator has only the current board available. Under
[AF-PROJ-014](../../../spec/requirements/plan.md#af-proj-014--no-re-lowering-after-sealing)
and [AF-REC-001](../../../spec/requirements/recovery.md#af-rec-001--never-replay),
no replacement private plan was fabricated and no old intent was replayed.
The task-owned Runtime was stopped through the typed CLI after the assessment;
the final status confirms `runtime.running=false`. Original records remain
intact. Complete Windows automated flash acceptance has **not passed**;
neither this plan-only validation nor the regression fixtures are hardware
qualification, production support or `REAL_DEVICE_PASS`.

Private local records under `windows-inbox-winusb-20261014/` include
`published-durable-plans-build.log` and `.json`,
`flash-plan-durable-first.json`, `durable-plan-before-restart.json`,
`runtime-durable-plans-restarted.json`, `durable-plan-after-restart.json`,
`recovery-plan-durable-plans.json`, `runtime-durable-restart-stopped.json` and
`runtime-durable-final-status.json`. Raw device identifiers, private plan bytes
and host paths are not exported into this record.

## Local targeted checks

Native Windows x86_64 release, static CRT, two Cargo jobs, fixed session cache.
Owner-only ACL tests run in the host environment: the restricted shell cannot
reopen a directory after its DACL is restricted to the host owner. The same
storage tests pass on the host without changing the ACL implementation.

The targeted fmt, clippy, daemon build and test commands exited 0. The test run
passed 234 tests, with one opt-in child helper reported ignored (its parent
executes it explicitly). The added interrupted-write recovery test passed
separately, for 235 passing targeted tests in total. Private local
logs are `durable-plan-layered-checks.log` and
`durable-plan-layered-checks.json`; the earlier architecture failure is retained
in `durable-plan-final-checks.log`, and the final recovery regression is in
`durable-write-recovery-test.log`. Checks cover fmt, clippy, daemon build,
engine, daemon, conformance and standalone tests. macOS and Swift validation run
in CI; no local macOS or Swift execution is claimed.

The documentation follow-up checks local record values, CI run identities,
relative links and `git diff --check`. It does not repeat Rust tests or perform
additional device execution for prose changes.

## CI

All checks passed on PR #13 head
`4082eecd24f4ad0694c7f1b5354d461ab0fc2067` before merge:

- Windows: [PR run 38543424215](https://github.com/ArkDeck/ArkForge/actions/runs/38543424215)
  and [push run 38543326689](https://github.com/ArkDeck/ArkForge/actions/runs/38543326689).
- macOS Rust and Swift: [PR run 38543424484](https://github.com/ArkDeck/ArkForge/actions/runs/38543424484)
  and [push run 38543326809](https://github.com/ArkDeck/ArkForge/actions/runs/38543326809).

The documentation follow-up's own CI results are recorded in its pull request.
CI success does not change the incomplete hardware-acceptance result above.
