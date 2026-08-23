# `spec/state-machines/`

| file | machine | status |
|---|---|---|
| `job.yaml` | the 16-state job machine: legal edges (normative), what the reference daemon writes on each edge (draft), restart policy (draft) | mixed |
| `crash-disposition.yaml` | architecture.md §13.3 as data, derived purely from the journal | normative |
| `permit.yaml` | one permit's life: ledger states, verification order, retransmission | normative |

Precedence: these tables over the Mermaid diagram in `docs/architecture.md`
§13.1, and the fixtures (`conformance/v1/state-machine`, `crash`, `permit`)
over these tables.

## Diagram and daemon coverage

`docs/architecture.md` §13.1 now draws the same closed edge set as
`JobState::may_transition_to` and AF-CONF-STATEMACHINE-001. The daemon enters
`rebindWait` for sealed mode changes and `reconciling` for provider-selected
read-only observations. `readOnlyDispatch` was removed: even read-only plan
steps use the uniform admission binding. The daemon still never resumes an
external intent after restart; it concludes from the durable crash reducer.

## Regenerating

`cargo run -p arkforge-conformance -- generate` rewrites the fixtures from the
code; `cargo test -p arkforge-conformance` fails if the committed fixtures
differ. The YAML tables are hand-maintained from the same sources and cite
them; a mismatch between a table and its fixture is a spec bug.
