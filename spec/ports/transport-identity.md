# Port: device transport (discover, open exact, wait for rebind)
status: draft
source: crates/arkforge-transport/src/lib.rs DeviceTransport, crates/arkforge-transport/src/usb.rs, crates/arkforge-transport/src/replay.rs
requirements: AF-TRN-001..022 (`requirements/transport.md`)

## Purpose
Turn raw USB facts into typed observations with explicit identity strength,
open exactly the observed device, and decide a rebind by the profile's rules.
Typed protocol requests are constructed only by protocol modules; nothing
deserialized from IPC ever becomes a USB setup packet.

## Operations
| op | input | output |
|---|---|---|
| `discover(filter{modes[], providerIds[], minimumIdentityStrength?}, deadlineEpochMs)` | typed filter | `[DeviceObservation]` (stable and transient) |
| `open_exact(observation, identityEvidencePolicy)` | a prior observation | a `TransportSession` with a session digest, or `TRANSPORT_NO_DEVICE` / `TRANSPORT_AMBIGUOUS{count}` |
| `wait_for_rebind(expectation, previous)` | `RebindExpectation` + the pre-transition observation | `RebindOutcome` (AF-CONF-REBIND-*) |

A `TransportSession` exposes the protocol-level operations the provider needs
(for RockUSB: test-unit-ready, read capacity, read/write LBA, reset, read
partition table) and its `sessionDigest`.

## Ownership and lifetime
An observation is a value. A session is owned by the caller for one step; it
is closed (claim released) before the step's receipt is recorded. The session
digest is part of the admission snapshot and changes on any re-open.

## Deadlines
`deadlineEpochMs` and the rebind `toleranceWindowMs` are wall-clock budgets
from the profile/plan; the port polls until the deadline and returns what it
saw. It never extends a budget.

## Idempotency and effects
`discover` and `wait_for_rebind` are read-only (enumeration only). `open_exact`
claims an interface (no device effect). Session operations have exactly the
effect the protocol defines and the provider declared.

## Crash / retry
No state survives a crash; the engine decides from the journal whether a
session operation may have happened.

## Error classes
`TRANSPORT_NO_DEVICE`, `TRANSPORT_AMBIGUOUS{count}`, `TRANSPORT_CLOSED`,
`TRANSPORT_UNSUPPORTED{detail}` (replay transport asked for something not
recorded), `TRANSPORT_EVIDENCE{detail}`, `TRANSPORT_CBOR`.

## Conformance hooks
`evaluate_rebind` is pure and pinned by `conformance/v1/rebind`. The replay
transport over a transcript is the reference mock; a port's replay transport
MUST produce the same observation digests for the same transcript
(AF-CONF-PLAN-003).
