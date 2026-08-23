# Artifacts: import, content-addressed store, parsers, manifest
status: draft
area: ART
rationale: architecture.md §10, AFD-0001
conformance: plan (AF-CONF-PLAN-001)

### AF-ART-001 — content addressing
status: normative
tests: [AF-CONF-PLAN-001]

An imported artifact is identified by the SHA-256 of its bytes
(`contentDigest`) and its size. Plans and jobs reference the artifact id and
digest, never a caller path; the daemon MUST NOT re-open a caller-supplied path
for a destructive plan.

### AF-ART-002 — the manifest is digested, and the digest enters the plan
status: normative
tests: [AF-CONF-PLAN-001, AF-CONF-PLAN-009]

`manifestDigest = SHA-256("arkforge/v1/artifact-manifest\0" || cbor(manifest))`
with `model/digest-bodies.cddl#artifact-manifest`. The plan's
`artifact.manifestDigest` binds the parsed facts, so a re-parse that yields
different facts invalidates the plan even for identical bytes.

### AF-ART-003 — parsers are pure
status: normative
tests: []

A parser has no USB, no network, no process execution, does not decide
authority, does not emit vendor options, and outputs only facts, unknowns and a
confidence (`researchOnly` | `productionManifest`).

### AF-ART-004 — parsers never panic, hang or allocate unboundedly
status: normative
tests: []
gap: the Rust mutation campaign (`crates/arkforge-artifact/tests/parser_fuzz.rs`, seeded xorshift64*) is the current evidence; a committed corpus of rejected inputs is planned.

Every rejection MUST be a typed error. Size fields are bounds-checked before
allocation; nesting and member counts are bounded.

### AF-ART-010 — archive member rules (DAYU200 gzip/tar)
status: normative
tests: [AF-CONF-PLAN-001]

The archive is read streaming. Members MUST be rejected with the stable codes:
`ARC003_PATH_ABSOLUTE`, `ARC004_PATH_TRAVERSAL` (`..`), `ARC005_PATH_INVALID`
(backslash, NUL, control characters), `ARC006_PATH_DUPLICATE`,
`ARC007_LINK_UNSUPPORTED` (symlink, hardlink), `ARC008_MEMBER_TYPE_UNSUPPORTED`
(device nodes, FIFOs, unknown types), `ARC009_MEMBER_SIZE_MISMATCH`;
`ARC002_ARCHIVE_INVALID` covers framing and I/O failures, `ARC001_IDENTITY_MISMATCH`
an archive that is not the one a plan is bound to.

### AF-ART-011 — DAYU200 facts
status: draft
source: crates/arkforge-artifact/src/dayu200.rs
tests: [AF-CONF-PLAN-001]

The parser computes the archive hash and every member hash; parses
`parameter.txt` into a partition table (`device`, `logicalBlockSize`, ordered
entries with `offsetSectors`, `sizeSectors | null`, `attribute ∈ {bootable,
grow} | null`, `grammarBranch ∈ {fixed, fixedBootable, remainderGrow}`);
validates overlap, order and that only the last partition is a remainder;
classifies members as `partitionTable`, `loader`, `imageCandidate`, `metadata`
or `unclassified`; extracts build facts (`const.ohos.fullname`,
`const.product.model`) from hashed image members; and never guesses a version
from a file name. Unknown partition attributes fail closed.

### AF-ART-012 — execution-relevant unknowns
status: normative
tests: [AF-CONF-PLAN-001, AF-CONF-PLAN-004]

`RK-A01` (no partition table), `RK-A02` (unclassified members), `RK-A03` (no
build facts) are execution-relevant unknowns. An unclassified member blocks an
executable plan unless the DeviceProfile lists it under
`artifactCompatibility.knownMetadataMembers`; the nine-partition allowlist lives
in the profile, never in the parser.

### AF-ART-013 — CAS lifecycle
status: draft
source: crates/arkforge-artifact/src/cas.rs
tests: []

The store MUST: preflight available space against a quota before import; refuse
an import whose bytes do not match an expected digest; hold a lease per active
plan/job and refuse to evict a leased object; and be crash-safe (an interrupted
import leaves no object under its final name).

### AF-ART-014 — PAC (DAYU600) is research-only
status: normative
tests: []

The PAC parser reports structure candidates, offsets, lengths and hashes with
confidence `researchOnly` on every input, and the twelve unknowns UNI-U01..U12.
No input can raise its confidence; a production manifest for PAC requires the
eighteen evidence gates of architecture.md §17.5.
