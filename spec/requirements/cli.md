# Agent-native CLI contract
status: draft
area: CLI
rationale: docs/openspec/chg-agent-native-cli/{proposal,design,verification}.md
conformance: cli (process-level vectors, also executed by arkforge-cli tests)

The CLI is the process boundary a full port is verified through. What is
normative here is the *contract* (command tree, JSON envelopes, stable codes,
refusal semantics), not the Rust frontend.

### AF-CLI-001 — one command tree
status: normative
tests: [AF-CONF-CLI-001..004]

`status`, `device`, `artifact`, `flash`, `apply`, `watch`, `cancel`, `job`,
`rescue`, `daemon`, `config`, `signing`, `completion`, `help` form the top-level
tree; their subcommands are the machine-help index. A port MUST implement the same tree with
the same spellings; `arkforge help [path] --format json` MUST describe it as
`arkforge.command-help/v1` (index: `arkforge.command-help-index/v1`).

### AF-CLI-002 — structured output is pure
status: normative
tests: [AF-CONF-CLI-002]

With global `--output json` (or `jsonl` for streams; machine help itself uses
`help ... --format json`) stdout carries exactly one JSON
document per record, each with a `schema` member (`arkforge.<name>/v<n>`), no
colour, progress, prompt or log text. Human output goes to stdout only without
`--format`; diagnostics go to stderr.

### AF-CLI-003 — the error envelope
status: normative
tests: [AF-CONF-CLI-004]

A failure is `{"schema": "arkforge.command-result/v1", "ok": false, "command":
[...], "error": {"code", "message", "remediation", "retryable",
"required_acknowledgements", "next_commands", "facts"}}` with a non-zero exit
status. `code` is from `errors/registry.yaml` (surface `cli`). `facts` is an
object of already-established facts or `null`; `next_commands` are complete
argv strings the caller can run.

### AF-CLI-004 — no broad override exists
status: normative
tests: []

There is no `--yes`, `--force` or equivalent. `flash apply` MUST require the
exact plan id, the plan SHA-256 and the complete acknowledgement token set the
`plan` step returned; a missing or unexpected acknowledgement is
`ACKNOWLEDGEMENT_REQUIRED` / `UNEXPECTED_ACKNOWLEDGEMENT`.

### AF-CLI-005 — device selection is exact
status: normative
tests: []

Mutating commands take an observation id. Zero, several, or an identity change
since the observation refuses before any mutation (`DEVICE_NOT_FOUND`,
`OBSERVATION_NOT_FOUND`, `TARGET_LINEAGE_CONFLICT`).

### AF-CLI-006 — read-only commands never mutate
status: normative
tests: []

`doctor`, `device list/show/probe/wait`, `artifact inspect/list/show`, `flash
assess/plan`, `job list/show/watch`, `rescue list/inspect/read` MUST NOT send
any mutating or destructive action; `artifact import` mutates only the host
store (`host_store_mutated`, `device_accessed: false`).

### AF-CLI-007 — hardware campaigns are named, never defaulted
status: normative
tests: []

A combination that is not production-verified executes only when the operator
names a campaign (`--hardware-campaign <id>` on the daemon). The flag is not a
`--force`; it is sealed into the plan (AF-PLAN-021) and never persisted as
support.

### AF-CLI-008 — rescue is a separate domain
status: normative
tests: []

Rescue plans and receipts use their own digest domains
(`arkforge/v1/rescue-plan\0`, `arkforge/v1/rescue-receipt\0`) and never reuse a
normal plan, permit or receipt. A normal flash failure MUST NOT fall through to
rescue automatically. Rescue accepts typed actions only — never raw USB
requests, raw LBA writes, shells or vendor argv.

### AF-CLI-009 — job watch is resumable
status: normative
tests: []

`job watch --format jsonl` emits events in `sequence` order and accepts a
`--from-sequence`; the final record is `{"record": "terminal", ...}` with the
job summary.
