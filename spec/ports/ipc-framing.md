# Port: local IPC endpoint and framing
status: draft (platform notes informative)
source: crates/arkforge-ipc/src/{framing,wire,lib}.rs; crates/arkforge-platform/src/{lib,platform}.rs
requirements: AF-IPC-001..018 (`requirements/ipc.md`)

## Purpose
A local, authenticated-by-OS byte stream between the daemon and its clients,
carrying length-prefixed Protobuf frames. Two endpoints exist per daemon: the
**public** one (read-only API) and the **controller** one (holds the pairing
secret).

## Operations
| op | notes |
|---|---|
| `bind(runtimeDir, kind)` | creates the endpoint; fails if another daemon already serves it (`RUNTIME_ALREADY_RUNNING`) |
| `accept()` | one connection; the first frame must be `Hello` |
| `connect(runtimeDir, kind)` | client side; `DAEMON_UNAVAILABLE` when nothing serves |
| `read_frame()` | `Ok(frame)`, `Ok(end-of-stream)` between frames, `PB_TRUNCATED` inside one |
| `write_frame(bytes)` | refuses > 16 MiB before writing |

## Framing
4-byte big-endian length + body (AF-IPC-001/002). One message per frame. The
length is checked before allocation.

## Handshake and sessions
`Hello{protocol_major=1, protocol_minor=0, session_kind}` → `HelloAck`.
Major mismatch is refused in the ack; minor is additive. `SESSION_KIND_PUBLIC`
may only call the read-only API (AF-IPC-004). The controller secret is
inherited from the process that starts the daemon (stdin/handle), never read
from a file or the environment in clear.

## Deadlines
A connected-but-not-serving peer is bounded by a handshake timeout on the
client (`DAEMON_UNAVAILABLE` after it); stream reads have no global timeout —
`watchJob` is long-lived.

## Crash / retry
A client reconnects and resumes `watchJob` with `from_sequence`; requests are
idempotent by `request_id` only where the API says so (`submitStepPermit`
retransmits the same permit bytes; `startExecution` is not idempotent and a
second call for the same plan is refused). A controller therefore persists the
`startExecution` job/plan correlation before submitting any permit and, after a
controller restart, resumes `watchJob` for that exact job (AF-REC-008). A
process-local job or receipt cache is never recovery evidence.

## Error classes
`DAEMON_UNAVAILABLE`, `RUNTIME_ALREADY_RUNNING`, `PROTOCOL_REFUSED`,
`SESSION_NOT_PERMITTED`, `IPC_IO_FAILED`, `IPC_RESPONSE_INVALID`,
`IPC_RESPONSE_MISMATCH`, plus the `PB_*` decoder classes.

## Recovery assessment references (informative)

`planSupersedingRecovery` accepts optional `RecoveryJobRequest` fields 2 and 3:
the already materialized candidate plan id and digest. Without those references,
the API reports the source job's coverage assessment. With them, it checks the
stored candidate's exact profile contract, complete writable target mapping,
content-bound write/verification pairs, reset and postflight. It never executes
either plan or accepts caller-supplied device facts.

`SupersedingRecoveryAssessment` fields 9–13 add the verified source journal
identity and candidate plan/profile/artifact digests. An optional
`ClosedControlEvidence` describes exactly one expired initial `enter-updater`
dispatch, correlated through admission, intent, consumption and dispatch.
Cancellation, another intent, any checkpoint or semantic receipt excludes this
narrow proof. It does not classify the old control as not executed. Missing
historical private write actions remain unbounded. Older messages without
source evidence do not authorize the standalone recovery path.

The standalone authority independently checks the old public plan, spent permit
and target lineage against fresh strong device observations. It durably seals
the assessment, reserves one replacement epoch, and correlates the new job
before issuing new permits. Each admission rechecks the source and candidate.
The proof expires with the plan or after four hours, whichever occurs first.
Another unresolved job, changed facts, missing correlation, cancellation or a
spent reservation prevents further execution. A failed replacement does not
automatically create another epoch.

Only the replacement's complete semantic receipts and successful postflight
produce an immutable `SupersedingRecoveryEpoch` relation. This releases the
old job's lane obstruction without changing its journal or permitting its
original plan to run again. Explicit readback skips retain their typed reason;
they never become full-hash verification. No Profile coverage declaration or
wire/digest fixture is changed by this implementation.

## Platform notes (informative)
- Unix: a Unix domain socket under the runtime directory, mode owner-only.
- Windows: byte-mode Named Pipe, `PIPE_REJECT_REMOTE_CLIENTS`,
  `FILE_FLAG_FIRST_PIPE_INSTANCE`, explicit DACL with only the current logon
  SID; clients connect with identification-level SQOS; a cross-account
  connection is refused with Win32 error 5. The pipe is published during
  `bind` so `status` can distinguish "connected" from "serving".
- Both share framing and schema; there is exactly one business API.

## Conformance hooks
`conformance/v1/protobuf` pins message bytes, decoder refusals and the frame
format. The socket round trip is covered by
`crates/arkforge-platform/tests/local_roundtrip.rs` and the Swift SDK's
`CrossLanguageGoldenTests` (a second implementation of this port already
exists).
