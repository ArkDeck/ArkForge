# Port: managed device control (HDC)
status: draft
source: proto/arkforge.proto ManagedControl*; crates/arkforged/src/jobs.rs; crates/arkforge-standalone/src/hdc_control.rs; adapters/arkforge-arkdeck-adapter/src/control.rs
requirements: AF-CTL-001..008 (`requirements/control.md`)

## Purpose
Let the daemon *name* a semantic device-control action (enter updater, reboot
to normal, read product facts, read build facts) that only the authority's own
HDC channel can perform, and receive a typed receipt — without ever learning
a connect key, an executable path, an endpoint or argv.

## Operations (over the watchJob stream and the controller API)
| direction | message | contents |
|---|---|---|
| daemon → authority | `JobEvent{kind: MANAGED_CONTROL_REQUESTED, control_request}` | `{jobId, stepId, requestId, action, permitId, expectedFacts[], deadlineEpochMs}` |
| authority → daemon | `submitManagedControlReceipt` | `{jobId, requestId, action, accepted, facts[], evidenceSha256, failureReason}` |
| daemon → authority | `SubmitManagedControlReceiptResponse` | `{accepted, rejectionCode, rejectionMessage}` |

## Ownership and lifetime
One pending control request per job; it is recorded in the journal
(`permitConsuming` with `controlRequestId`, `controlAction`,
`controlDeadlineEpochMs`) before it is published, so a restart can see that the
permit was spent asking.

## Deadlines
`deadlineEpochMs` = permit time + 120 000 ms in the reference daemon. Past it
the daemon classifies `outcomeUnknown`; a receipt arriving later is refused
(`NoControlPending`).

## Idempotency and effects
`enter-updater` and `reboot-to-normal` change the device's mode; `read-*` do
not. A receipt with `accepted = false` does not mean no effect (AF-CTL-004).
The same request id is never re-issued.

## Error classes (rejection codes)
`RECEIPT_CARRIES_FORBIDDEN_FACTS`, `WRONG_REQUEST`, `WRONG_CONTROL_ACTION`,
`CONTROL_FACTS_INCOMPLETE`, `NO_CONTROL_PENDING`, `UNKNOWN_JOB`.

## Standalone implementation notes (informative)
The standalone supervisor implements this port with a local `hdc` binary bound
by SHA-256 (`HDC_DIGEST_MISMATCH`), a closed argv vocabulary, no shell, and
maps: `enter-updater` → `hdc -t <key> target boot loader`-class command;
`reboot-to-normal` → reboot; `read-product-facts` / `read-build-facts` →
`param get` of product model / `const.ohos.fullname`. Connect keys are digested
under `device-facts` before they appear in any fact (`SHA-256(domain ||
connectKey)`), never in clear. The ArkDeck adapter maps the same four actions
onto ArkDeck's typed HDC provider (`adapters/arkforge-arkdeck-adapter/src/control.rs`).
