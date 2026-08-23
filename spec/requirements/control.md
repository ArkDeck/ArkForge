# Managed device control (HDC) port
status: draft
area: CTL
rationale: architecture.md §9, proto/arkforge.proto ManagedControl*
conformance: protobuf (AF-CONF-PB-013, AF-CONF-PB-014, AF-CONF-PB-018)

### AF-CTL-001 — four semantic actions
status: normative
tests: [AF-CONF-PB-018]

The daemon may ask for exactly `enter-updater`, `reboot-to-normal`,
`read-product-facts`, `read-build-facts` (wire 1–4). It receives no executable
path, endpoint, connect key, argv, shell or server lifecycle control.

### AF-CTL-002 — a control action is a permitted step
status: normative
tests: []

A managed control request names the permit id of the step it belongs to; the
daemon has verified that permit, and the authority checks it is its own before
acting.

### AF-CTL-003 — forbidden receipt facts
status: normative
tests: [AF-CONF-PB-018]

A control receipt carrying any of the fact keys `connectKey`,
`hdcExecutablePath`, `hdcEndpoint`, `argv`, `shell`, `serverLifecycleAction`
MUST be refused as a whole (`RECEIPT_CARRIES_FORBIDDEN_FACTS`); the daemon does
not drop the field and continue.

### AF-CTL-004 — `accepted = false` is not "nothing happened"
status: normative
tests: []

A receipt with `accepted = false` means the authority did not observe its
semantic success. A mode change may have taken effect unobserved, so the
daemon records `outcomeUnknown`, never a clean failure.

### AF-CTL-005 — expected facts
status: normative
tests: []

For a postflight read the request lists `expectedFacts`; an accepted receipt
that lacks any expected key MUST be refused (`CONTROL_FACTS_INCOMPLETE`).

### AF-CTL-006 — deadline
status: draft
source: crates/arkforged/src/jobs.rs submit_permit, expire_stale_controls
tests: []

The request carries `deadlineEpochMs` (reference: permit time + 120 000 ms).
Past it, an unanswered request is classified `outcomeUnknown`; the same request
id MUST NOT be re-issued.

### AF-CTL-007 — request/receipt correlation
status: normative
tests: []

A receipt MUST name the pending request id and the same action; a receipt for
another request (`WRONG_REQUEST`) or another action (`WRONG_CONTROL_ACTION`)
is refused without changing the job.

### AF-CTL-008 — standalone HDC binding
status: draft
source: crates/arkforge-standalone/src/hdc_control.rs
tests: []

When the standalone supervisor implements this port with a local `hdc`
executable, it MUST bind the executable by SHA-256 (`HDC_DIGEST_MISMATCH`
otherwise), run it with a closed argv vocabulary and no shell, and expose only
the four actions above.
