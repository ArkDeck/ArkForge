# Verification: three outcomes, explicit strength
status: normative
area: VER
rationale: architecture.md §16.4, AD-006, AD-019
conformance: protobuf (AF-CONF-PB-015); receipt fixtures planned

### AF-VER-001 — three outcomes
status: normative
tests: [AF-CONF-PB-015]

A verification step reports exactly one of `verified`, `typedSkip`, `failed`
(`verification-outcome` vocabulary). A step that verifies nothing reports none.

### AF-VER-002 — strength only with `verified`
status: normative
tests: [AF-CONF-PB-015]

`verificationStrength` ∈ {`fullHash`, `sampledRanges`, `prefixHash`,
`semanticOnly`} is present only when the outcome is `verified`. A typed skip is
never any strength, and `prefixHash` MUST NOT be reported or summarised as full
verification.

### AF-VER-003 — read domain is measured, never assumed
status: normative
tests: []

Before any readback the implementation MUST characterize the medium's read
domain at runtime. The window size is an observation of one session and MUST
NOT be a profile constant. Readback of a range the read domain does not cover
is a `typedSkip` with reason `skipped-lba-read-window` (or
`profile-declares-unreachable`), not a failure.

### AF-VER-004 — erased-medium filler is classified separately
status: normative
tests: []

When a readback inside the read domain returns the profile's
`erasedMediumFiller` byte uniformly, the failure classification is
`erased-medium-filler`; only a genuine content difference is
`content-mismatch`. Uniform filler MUST NOT be reported as a hash mismatch.

### AF-VER-005 — fallback evidence is named
status: normative
tests: [AF-CONF-PLAN-002]

A profile target declares `maxStrengthWhenReadable` and a `fallback`
(`writeCompletionSemantics`, `buildPostflight`). A typed skip counts toward
none of the verified strengths; the fallback evidence is what the receipt
carries instead.

### AF-VER-006 — conclusive verification failure ends the job
status: draft
source: crates/arkforged/src/jobs.rs complete_dispatch
tests: []

A `failed` verification outcome after a checkpointed write concludes the job
`confirmedFailed` with `failureClassification` in the record; it does not
become `outcomeUnknown`, because the write and its readback both completed.

### AF-VER-007 — postflight facts
status: normative
tests: []

Postflight MUST confirm exact target lineage, normal/HDC mode, product model,
and runtime build against the artifact manifest's build facts, through the
managed device control port (`ports/device-control.md`), never through a path
the daemon opens itself.
