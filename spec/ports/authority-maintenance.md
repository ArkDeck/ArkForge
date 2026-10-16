# Authority-owned DAYU200 maintenance
status: draft
source: adapters/arkforge-arkdeck-adapter/src/maintenance.rs

This is an in-process authority port, not a new daemon endpoint or a grant to
access the standalone authority. ArkDeck implements the ports inside its
protected Runtime. UI, CLI, candidate code and imported documents MUST NOT
supply Source, Identity, Reservation, read results or capability facts.

## Source and verification

The Runtime MUST hold the device-exclusive lane for the complete call. It MUST
load the sealed plan, job and semantic receipts from its durable correlation
with its own arkforged generation. The adapter requires the latest source job,
a complete supported partition set (or the exact original ramdisk), matching
plan digest, profile, campaign, authority namespace, completed steps and unique
successful receipts. No historical outcome or strength is rewritten.

The Runtime MUST reobserve the exact durable identity before and after every
read. Mode transitions require an already proven durable binding; discovery
similarity is insufficient. The read port MUST independently check the block
mapping, sealed offset and length, holders and writable mounts before and after
hashing. It MUST reject short reads and must never send caller paths or expected
hashes to a device. Full equality yields fullHash; a writable mount is the
mounted-writable-after-boot typed skip; errors and mismatches are failures.
Loss of continuity invalidates all earlier successes and stops later reads.

Before any read the Runtime MUST fsync a new unique verification intent with
source job/plan/digest, target identity, tool digest, budget and sealed effects.
It MUST publish the separate arkforge.flash-verification/v1 document only after
fsync. The document MUST include each target's range, expected/observed digest,
outcome, strength and reason, plus aggregate counts. Exits remain 0/9/7;
persistence or source failure is an error, not a successful result.

## Restoration

The input is a source job reference, immutable Artifact lease resolved to its
SHA-256 by Runtime, and bounded duration. Invalid input returns before any port
call or record (exit 2). A valid request's failure produces a durable stopped
record (exit 1). Failure to persist that record remains an explicit error.

Source admission permits only confirmed full-flash completion or a confirmed
final reboot with postflight outstanding. Failed/unknown source receipts are
ineligible. Runtime MUST materialize a distinct restoreRamdisk plan through its
paired controller, check its entire projection and tool/artifact/profile facts,
and require exactly the pinned 4 MiB original ramdisk at offset 121634816.
It MUST NOT claim complete-overwrite recovery coverage or write userdata.

The reservation port MUST re-read source/identity/plan/artifact/tool facts under
exclusion, reserve and durably consume a matching RuntimeCapability, and fsync
intent before returning its opaque token. A prior stopped/unknown intent MUST
NOT be automatically retried. The token is consumed by value exactly once,
through existing StepPermit admission; Rust ownership does not substitute for
the durable cross-restart use ledger. Budget includes execution and verification.

Success (exit 0) requires a distinct succeeded restoration job and a separate
verification of exactly the original ramdisk with fullHash, zero skipped and
zero failed targets. Userdata is neither written nor verified. All errors stop;
no code path in the coordinator retries execution or changes old outcomes.

## Integration status

The adapter provides orchestration and typed results, with fake port fault
injection tests. It does not provide ArkDeck's trusted persistence, capability,
HDC or target-lease implementations. These ports must be implemented and tested
in ArkDeck before a Catalog operation can be enabled. This API is not hardware
evidence or permission to execute a candidate Catalog on a device.
