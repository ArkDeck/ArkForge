# Authoring conventions for `spec/`

These rules apply to every file under `spec/`. They exist so that a document
written by one person (or one agent) can be read, checked and extended by
another without re-deriving the conventions.

## 1. Status of a statement

Every normative file declares `status:` in its front matter or header:

| status | meaning |
|---|---|
| `normative` | Implementations MUST conform. Backed by at least one conformance case or an explicit `tests: []` with a `gap:` note. |
| `draft` | Extracted from the reference implementation and believed correct, but not yet reviewed against hardware evidence or a conformance case. Implementations SHOULD conform; divergence must be reported as a spec issue, not silently chosen. |
| `informative` | Rationale, history, examples. Never a requirement. |

Inside a file, a single requirement may override the file status with its own
`status:` field.

## 2. Requirement IDs

`AF-<AREA>-<NNN>` — stable for the life of the spec. Never renumbered, never
reused; a withdrawn requirement keeps its ID with `status: withdrawn`.

| AREA | scope |
|---|---|
| `DIG` | SHA-256, HMAC, deterministic CBOR, domain separation |
| `ID`  | identifier grammar (OpaqueId and its newtypes) |
| `ART` | artifact import, CAS, parsers (tar/gzip/parameter.txt/PAC) |
| `PROF`| DeviceProfile document and invariants |
| `PLAN`| PlanAssessment / FlashPlanEnvelope / plan digest |
| `PROJ`| public ↔ private projection |
| `EFF` | EffectSet vocabulary and executability rules |
| `AUTH`| authority boundary, StepAdmission, StepPermit |
| `ENG` | job state machine |
| `JRN` | journal record model and durability |
| `CRASH`| crash windows and dispositions |
| `REC` | outcomeUnknown, reconcile, superseding recovery |
| `VER` | verification tri-state and strengths |
| `TRN` | transport, device identity, rebind, transcript |
| `CTL` | managed device control (HDC) |
| `IPC` | framing, sessions, Protobuf evolution |
| `CLI` | agent-native CLI contract |
| `PORT`| OS port contracts |

Transition IDs: `AF-ENG-T-<NNN>`; crash rows: `AF-CRASH-R-<NNN>`; permit
dispositions: `AF-AUTH-P-<NNN>`; conformance cases: `AF-CONF-<SUITE>-<NNN>`.

Numbers are grouped by tens (`010`, `020`, …) with gaps left for insertion; a
range such as `AF-EFF-001..011` means "every defined ID in that range", not
that every number in between exists.

## 3. Requirement record shape (Markdown)

~~~markdown
### AF-JRN-003 — fsync policy is a function of record kind
status: normative
rationale: architecture.md §13.2
tests: [AF-CONF-JOURNAL-004]
impl: mappings/rust.yaml#AF-JRN-003

A record whose `fsyncPolicy` differs from the policy its `kind` requires MUST
be rejected on replay with `JOURNAL_FSYNC_POLICY_MISDECLARED`.
~~~

Use RFC 2119 keywords (MUST / MUST NOT / SHOULD / MAY) in capitals. One
requirement states one checkable fact. If a sentence needs "and", it is
usually two requirements.

## 4. Machine-readable files

- YAML files under `state-machines/`, `errors/`, `mappings/`, and
  `manifest.yaml` MUST be in the strict YAML subset of `model/strict-yaml.md`
  (block mappings, block sequences, flow sequences, plain/quoted scalars,
  comments; no anchors, aliases, tags, flow mappings, multi-line scalars, tabs
  or duplicate keys). The same reader that loads a DeviceProfile loads them.
- CDDL files under `model/` follow RFC 8610. Every map is a CBOR map with
  text keys; the encoded map is RFC 8949 §4.2 deterministic, so key order in
  the CDDL is documentation only.
- JSON Schema files are draft 2020-12.

## 5. Provenance markers

Where a fact was extracted from the Rust reference implementation rather than
from a design document or hardware evidence, say so in a `source:` field with
the file path and symbol, e.g. `source: crates/arkforge-engine/src/lib.rs
JobState::may_transition_to`. When architecture.md and the code disagree, the
spec records the code's behaviour under `status: draft` and files the
disagreement in `spec/ISSUES.md`; it does not pick one silently.

## 6. Wire spellings

Any string that crosses a process, file or digest boundary is quoted exactly
as spelled on the wire (`"stepIntentDurable"`, not "StepIntentDurable" or
"step intent durable"). Rust enum names are not wire spellings.

## 7. What the spec does not contain

- Rust code. A Rust snippet may appear only in `mappings/rust.yaml` or in an
  `informative` example block, never as the definition of a type.
- Implementation status, task ledgers, hardware campaign logs. Those live in
  `TASKS.md` and `docs/evidence/`.
- Product/UI decisions and ArkDeck-side policy. Those live in
  `docs/architecture.md` and the ArkDeck repository.
