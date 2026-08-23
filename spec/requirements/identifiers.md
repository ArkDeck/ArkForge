# Identifiers
status: normative
area: ID
rationale: architecture.md §15.4 ("non-conforming IDs")
conformance: permit (decode cases), plan

### AF-ID-001 — OpaqueId grammar
status: normative
tests: [AF-CONF-PERMIT-048]

Every identifier that crosses a digest, file or wire boundary (plan, job, step,
attempt, permit, action, observation, request, controller session, partition,
region, provider, profile, artifact, toolchain, fact key, authority namespace,
host OS/arch) MUST match `^[A-Za-z0-9._:-]{1,128}$` (ASCII only, measured in
bytes). An implementation MUST refuse to construct an identifier outside this
grammar and MUST report which field carried it.

### AF-ID-002 — identifiers are compared bytewise
status: normative
tests: []

No case folding, Unicode normalization or trimming is applied anywhere.
`PLAN-1` and `plan-1` are different identifiers.

### AF-ID-003 — identifiers are CBOR text strings
status: normative
tests: [AF-CONF-PERMIT-001]

In a digest body an identifier is encoded as a CBOR text string of its exact
bytes. A digest is a 32-byte CBOR byte string, never hex text, inside a digest
body; it is lowercase hex text on JSON/YAML surfaces.

### AF-ID-004 — identifiers the daemon mints
status: draft
source: crates/arkforged/src/jobs.rs JobRegistry::create, request_admission
tests: []

The reference daemon mints: `JOB-<16 uppercase hex digits of epoch ms>-<4 digit
counter>` for jobs; `ATTEMPT-<n>` (1-based step index) for attempts;
`<jobId>-<n>` for admission request ids; `<requestId>-control` for managed
control request ids. A port MAY choose other shapes **provided they satisfy
AF-ID-001 and are unique per daemon lifetime**; an authority MUST NOT parse
structure out of them.

### AF-ID-005 — fact keys are identifiers
status: normative
tests: [AF-CONF-JOURNAL-001]

Journal fact keys and protocol-identity fact keys are OpaqueIds. Fact *values*
are free UTF-8 text.
