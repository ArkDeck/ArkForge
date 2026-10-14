# Durable private plans after daemon restart

Status: implementation regression record; not hardware acceptance.

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

## Local targeted checks

Native Windows x86_64 release, static CRT, two Cargo jobs, fixed session cache.
Owner-only ACL tests run in the host environment: the restricted shell cannot
reopen a directory after its DACL is restricted to the host owner. The same
storage tests pass on the host without changing the ACL implementation.

The targeted fmt, clippy, daemon build and test commands exited 0. The test run
passed 234 tests, with one opt-in child helper reported ignored (its parent
executes it explicitly). The added interrupted-write recovery test passed
separately. Private local
logs are `durable-plan-layered-checks.log` and
`durable-plan-layered-checks.json`; the earlier architecture failure is retained
in `durable-plan-final-checks.log`, and the final recovery regression is in
`durable-write-recovery-test.log`. Checks cover fmt, clippy, daemon build,
engine, daemon, conformance and standalone tests. macOS and Swift validation run
in CI; no local macOS or Swift execution is claimed.

## CI

Pending for this commit. CI results will be recorded in the pull request after
the exact head completes. No new hardware result or production-support claim
is made by these regression tests.
