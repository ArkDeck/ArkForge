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
source: cmd/arkforged/src/jobs.rs complete_dispatch
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

### AF-VER-008 — independent Normal-mode image verification
status: draft
source: crates/arkforge-standalone/src/supervisor/verification.rs
tests: []
gap: Cross-language hardware-independent conformance vectors are not yet published; Rust source admission, bounded HDC and immutable record tests cover the reference port.

The standalone authority MAY independently verify a completed DAYU200 job's
image ranges in Normal mode, as defined by `ports/standalone-verification.md`.
It MUST derive every target, range and expected digest from that job's sealed
plan, require fresh exact target lineage, and retain the original job outcome
and receipts unchanged. A complete matching image SHA-256 is `verified` with
`fullHash`; an observed writable mount is `typedSkip` with
`mounted-writable-after-boot`. Failed reads, changed mappings, lost identity
and content mismatches MUST NOT become skips or success. Any skip makes the
aggregate partial, never fully verified. This read domain does not expand the
measured Loader read domain or turn an old skip into an old verification.

### AF-VER-009 — pre-mount userdata measurement
status: draft
source: crates/arkforge-artifact/src/preboot.rs; crates/arkforge-standalone/src/hdc_control/preboot.rs
tests: []
gap: Hardware acceptance is recorded separately; fixture output is not hardware evidence.

A separately authorized acceptance ramdisk MAY measure the complete sealed
userdata image range before stock init or any MMC filesystem mount. Its base
and overlays MUST match the published pinned derivation byte-for-byte,
including the compressed crypto dependency required to start the original
toybox before system is mounted. Missing or changed dependencies MUST be
refused on the host; dependency completion MUST NOT mount persistent storage. The
read-only verifier MUST bind the derivative to the successful job's ramdisk
write, and bind its RAM report to the nonce, current boot, storage CID,
exact range and fresh target lineage. Missing, duplicate, stale or malformed
reports MUST fail. Expected hashes MUST remain on the host. The resulting
fullHash proves bytes at the pre-mount point, not their state after Normal boot.

Report retention MUST survive normal boot's kernel log turnover without any
persistent filesystem write. A bounded holder MAY retain a read-only FD to
the original initramfs file after switch_root unlinks it. The verifier MUST
find exactly one closed holder command, a small root-owned read-only regular
file at its designated FD, and a RAM filesystem. Holder expiry, ambiguity or
a failed measurement marker MUST fail; they MUST NOT become a typed skip.

Restoring the original ramdisk MUST use a separate sealed destructive plan
with exactly that pinned partition image, original identity/permit/journal
rules and no userdata write. It MUST NOT claim complete-overwrite recovery
coverage. Original jobs and verification records MUST remain immutable.

### AF-VER-010 — consumer authority verification port
status: draft
source: crates/arkforge-arkdeck-adapter/src/maintenance.rs
tests: []
gap: Rust fake-port fault injection exists; cross-language fixtures and real authority integration remain pending.

A consumer authority MAY use the adapter maintenance port described in
`ports/authority-maintenance.md`. It MUST derive read ranges from a correlated
sealed successful source, retain target exclusion, persist intent before reads,
recheck identity around each read and persist separate typed results. It MUST
NOT use the standalone authority's namespace, records or endpoint.
