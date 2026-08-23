# Transport, device identity, rebind, transcript
status: draft
area: TRN
rationale: architecture.md §11, AD-008, AD-009, AD-020, #1067, #1068
conformance: rebind, plan (AF-CONF-PLAN-003)

### AF-TRN-001 — an observation is a typed fact set
status: normative
tests: [AF-CONF-PLAN-003]

A `DeviceObservation` is `{observationId, observedAtEpochMs, mode,
topologyDigest, descriptorDigest, serialEvidence{kind, digest|null},
protocolIdentity[{key, value}], providerCandidates[{providerId, confidence}],
identityStrength, malformedDescriptor}` (`model/digest-bodies.cddl#device-observation`).
VID/PID may inform `mode` but MUST NOT by themselves form a stable target.

### AF-TRN-002 — digests of identity inputs
status: draft
source: crates/arkforge-transport/src/usb.rs, crates/arkforge-transport/src/lib.rs DeviceObservation::admission_facts_digest
tests: [AF-CONF-PLAN-003]

`topologyDigest = SHA-256("arkforge/v1/device-facts\0" || locationId as 4-byte
big-endian)` on IOKit-style hosts; `serialEvidence.digest = SHA-256(domain ||
serial bytes)`; `descriptorDigest = SHA-256(domain || descriptor payload)`; the
admission `deviceFactsDigest` is `SHA-256(domain || cbor({mode, topologyDigest,
descriptorDigest, serialEvidence, protocolIdentity, identityStrength,
malformedDescriptor}))` — note it excludes `observationId`, `observedAtEpochMs`
and `providerCandidates`, so two observations of the same device at different
times hash the same. The `device-facts` domain is shared by several preimage
shapes (ISSUES SI-008).

### AF-TRN-003 — identity strength is ordered
status: normative
tests: [AF-CONF-REBIND-010, AF-CONF-REBIND-011]

`classOnly < serialAsserted < serialAndTopology < protocolConfirmed`. A
discovery filter's `minimumIdentityStrength` and a rebind expectation's floor
compare against this order.

### AF-TRN-004 — exact open
status: normative
tests: []

`open_exact` MUST open the device whose observation was given and MUST refuse
when zero or more than one device matches (`TRANSPORT_NO_DEVICE`,
`TRANSPORT_AMBIGUOUS{count}`). A transport never picks the first match.

### AF-TRN-005 — the transport session digest is the continuity fact
status: normative
tests: [AF-CONF-ADMISSION-007, AF-CONF-ADMISSION-008]

An open session has a digest under `"arkforge/v1/transport-session\0"`; a
re-open, a detach or a re-enumeration changes it. Admission freshness treats any
change as a broken continuity (AF-AUTH-003).

## Rebind (AF-CONF-REBIND-001..018)

### AF-TRN-010 — settle on the first stable matching observation
status: normative
tests: [AF-CONF-REBIND-001]

Observations are evaluated in the order made. The first stable observation in
the expected mode (or an alias) that passes identity, serial, topology and
uniqueness checks settles the rebind.

### AF-TRN-011 — no candidate
status: normative
tests: [AF-CONF-REBIND-002]

### AF-TRN-012 — uniqueness
status: normative
tests: [AF-CONF-REBIND-003, AF-CONF-REBIND-017]

If any *other* stable observation (different `descriptorDigest`) is also in
the expected mode, the result is `ambiguous{count}` and the rebind stops.
Repeated observations of the same descriptor are one candidate.

### AF-TRN-013 — transient tolerance window
status: normative
tests: [AF-CONF-REBIND-004..006, AF-CONF-REBIND-018]

The window is measured from the first observation's timestamp. Inside it
(`elapsed <= toleranceWindowMs`), a malformed observation is tolerated only
when the profile declares `tolerateTransientMalformed`; a malformed observation
outside the window, or any malformed observation when not tolerated, ends the
rebind with `toleranceWindowExhausted{transientObservations}`. Running out of
observations after at least one transient one is also `toleranceWindowExhausted`.

### AF-TRN-014 — expected mode not reached
status: normative
tests: [AF-CONF-REBIND-007, AF-CONF-REBIND-008]

A stable observation in another mode is transient inside the window and
`expectedModeNotReached{observed}` outside it.

### AF-TRN-015 — mode aliases are profile facts
status: normative
tests: [AF-CONF-REBIND-009]

A mode name counts as the expected mode only if it is the expected mode or one
of the aliases the DeviceProfile declares for it. A transport MUST NOT invent
equivalences.

### AF-TRN-016 — identity may not weaken
status: normative
tests: [AF-CONF-REBIND-010, AF-CONF-REBIND-011]

A settled candidate's identity strength MUST be at least the expectation's
floor and at least the previous observation's strength; otherwise
`identityWeakened{before, after}`. Strength is compared only between stable
observations.

### AF-TRN-017 — serial and topology policies
status: normative
tests: [AF-CONF-REBIND-012..014]

With `must-match`, the serial digest (respectively topology digest) MUST equal
the previous observation's; with `may-change` it carries no identity weight.
Both policies are DeviceProfile facts per transition.

### AF-TRN-018 — check order
status: normative
tests: [AF-CONF-REBIND-015, AF-CONF-REBIND-016]

stable? → mode (or alias)? → identity strength → serial → topology → uniqueness.

## Transcript

### AF-TRN-020 — transcript digest
status: draft
source: crates/arkforge-transport/src/transcript.rs Transcript::digest
tests: [AF-CONF-PLAN-003]

A transcript is digested under `"arkforge/v1/transcript\0"` over its canonical
model (`model/transcript.md`). The replay transport's toolchain backend digest is
the SHA-256 of the transcript file bytes, so a plan materialized against a
transcript names the exact recording.

### AF-TRN-021 — replay is never production
status: normative
tests: []

A toolchain of kind `replay` MUST never publish `productionVerified` and a plan
carrying it MUST be refused at `startExecution` by the toolchain-digest check.

### AF-TRN-022 — what a transcript records
status: normative
tests: []

By default a transcript records request/response kinds, lengths, payload
hashes, parsed semantic fields, status, timing, attach/detach/rebind and
execution evidence hashes. Full firmware payloads MUST NOT appear in a
transcript.
