# `spec/state-machines/`

| file | machine | status |
|---|---|---|
| `job.yaml` | the 17-state job machine: legal edges (normative), what the reference daemon writes on each edge (draft), restart policy (draft) | mixed |
| `crash-disposition.yaml` | architecture.md §13.3 as data, derived purely from the journal | normative |
| `permit.yaml` | one permit's life: ledger states, verification order, retransmission | normative |

Precedence: these tables over the Mermaid diagram in `docs/architecture.md`
§13.1, and the fixtures (`conformance/v1/state-machine`, `crash`, `permit`)
over these tables.

## Differences between architecture.md §13.1 and the code

Found while extracting (`crates/arkforge-engine/src/lib.rs JobState::may_transition_to`):

| edge | diagram | code | note |
|---|---|---|---|
| `readOnlyDispatch → postflight` | absent | legal | |
| `readOnlyDispatch → cancelledSafe` | absent | legal | |
| `dispatching → cancelledSafe` | absent | legal | only when the work never left the queue |
| `checkpointed → confirmedFailed` | absent | legal | conclusive verification failure |
| `checkpointed → cancelledSafe` | absent | legal | queued cancellation honoured |
| `readOnlyDispatch` successors | none drawn | `preflight`, `postflight`, `cancelledSafe` | |

And between the code's type table and the daemon's use of it (`crates/arkforged/src/jobs.rs`):

- `readOnlyDispatch`, `rebindWait`, `reconciling` are never entered by the
  daemon; every step — read-only included — goes through admission (SI-005).
- `preflight → cancelledSafe` is not a legal edge, so `cancel` assigns
  `awaitingPermit` directly before moving (SI-004).
- The daemon never resumes a job after restart; it concludes it (`job.yaml`
  `restart:`), whereas `crash-disposition.yaml` rows R-005/R-006 describe what
  a resuming implementation must do (SI-003).

## Regenerating

`cargo run -p arkforge-conformance -- generate` rewrites the fixtures from the
code; `cargo test -p arkforge-conformance` fails if the committed fixtures
differ. The YAML tables are hand-maintained from the same sources and cite
them; a mismatch between a table and its fixture is a spec bug.
