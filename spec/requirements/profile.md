# DeviceProfile
status: draft
area: PROF
rationale: architecture.md §18, AD-006, AD-008, AD-009
conformance: plan (AF-CONF-PLAN-002), strict-yaml
schema: model/profile.schema.json

A DeviceProfile is data: every fact about a board that is not derivable from
the artifact or the device at runtime. It is loaded through the strict YAML
subset, typed by the loader, validated against the invariants below, and
digested over its canonical model so that the plan binds the exact profile.

### AF-PROF-001 — digest over the canonical model
status: normative
tests: [AF-CONF-PLAN-002]

`profileDigest = SHA-256("arkforge/v1/device-profile\0" || cbor(device-profile))`
with `model/digest-bodies.cddl#device-profile`. YAML comments, key order and
quoting do not affect it. `profile.expectedDigest`, when present, MUST equal the
computed digest or the profile is refused.

### AF-PROF-002 — identity
status: normative
tests: [AF-CONF-PLAN-002]

A profile's identity is `{id, version, digest}`; a plan seals it and a changed
profile (any field) is a different profile requiring a new plan.

### AF-PROF-003 — schema version
status: normative
tests: []

`schemaVersion` MUST be exactly `arkforge.device-profile/v1`; any other value is
refused (`PROFILE_REJECTED`).

### AF-PROF-004 — required blocks and typing
status: draft
source: crates/arkforge-core/src/profile.rs load
tests: []

Required: `profile{id, version}`, `identity{productModels (≥1), soc{vendor,
family}, hardwareRevisions}`, `providers[]` (each `id`, `backend`,
`minimumVersion`, `maximumVersionExclusive`), `artifactCompatibility.formats`,
`modes[]`, `storage{kind, logicalBlockSize}`, `readDomain{write, read,
erasedMediumFiller}`, `dataImpact{userdata, calibration, nonVolatileConfig,
secureStorage}`. Optional: `usbIdentities`, `modeTransitions`,
`allowedTargets`, `protectedTargets`, `recovery`, `evidenceRefs`,
`artifactCompatibility.knownMetadataMembers`. A missing required key is an error
naming the path; `unknown` is the only way to say "not measured" for
`logicalBlockSize` and `erasedMediumFiller`, and an omitted key is never read as
unknown.

### AF-PROF-010 — no wildcard hardware revision
status: normative
tests: []

`identity.hardwareRevisions.allow` containing `*` MUST be refused at load
(`WildcardHardwareRevision`). An empty list is accepted and blocks execution
(`NoHardwareRevisionMeasured`). `anyRevisionEvidence: <evidenceRef>` is the only
way to claim revision independence.

### AF-PROF-011 — allowed and protected targets are disjoint and unique
status: normative
tests: []

A partition listed in both `allowedTargets` and `protectedTargets`, or twice
in `allowedTargets`, MUST be refused.

### AF-PROF-012 — write order
status: normative
tests: [AF-CONF-PLAN-002]

`writeOrder` values MUST be exactly `1..n` and, in that order, `offsetSectors`
MUST strictly ascend.

### AF-PROF-013 — verification may not exceed the read domain
status: normative
tests: []

When `readDomain.read` is `characterize-at-runtime`, every allowed target MUST
declare at least one fallback (`writeCompletionSemantics` or
`buildPostflight`). A profile may not claim readback strength it cannot reach.

### AF-PROF-014 — declared modes
status: normative
tests: []

Every mode named by a transition or a USB identity MUST be declared in
`modes`. `rebind.toleranceWindowMs` MUST be greater than 0. Two USB identities
with the same vendor/product id MUST NOT map to different modes.

### AF-PROF-015 — block size
status: normative
tests: []

`storage.logicalBlockSize: 0` is refused (zero is a wrong answer, not unknown).

### AF-PROF-020 — execution blockers
status: normative
tests: [AF-CONF-PLAN-004]

A loaded profile reports, and an executable plan is refused on, any of: a
data-impact axis `unknown`; `logicalBlockSize` unknown; `erasedMediumFiller`
unknown; no measured hardware revision; no allowed targets; no mode
transitions. The research profile `profiles/dayu600.yaml` trips all six by
design.

### AF-PROF-021 — mode aliases and tolerance are profile facts
status: normative
tests: [AF-CONF-REBIND-009, AF-CONF-REBIND-004]

`modes[].aliases`, `modeTransitions[].serialPolicy`, `topologyPolicy` and
`rebind{requireDisconnect, toleranceWindowMs, tolerateTransientMalformed}` are
the only source of those facts for the transport (AF-TRN-015, AF-TRN-017).

### AF-PROF-022 — the allowlist is the only writable set
status: normative
tests: []

A target that is not in `allowedTargets` is not writable whatever the archive
contains; a `protectedTargets` entry can never be overridden by a caller. The
profile allowlist, the observed partition table and the artifact manifest MUST
agree before a write target enters a plan.

### AF-PROF-023 — published profiles are data, not policy
status: normative
tests: [AF-CONF-PLAN-002]

`profiles/dayu200.yaml` and `profiles/dayu600.yaml` are part of the spec
inventory (`manifest.yaml`). A change to either is a spec revision.
