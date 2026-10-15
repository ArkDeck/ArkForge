# Port: independent Normal-mode verification
status: draft
requirements: AF-VER-008, AF-CLI-010
source: crates/arkforge-standalone/src/supervisor/verification.rs; crates/arkforge-standalone/src/hdc_control/partition.rs

The owner-only standalone supervisor accepts `verify-flash` with exactly a
source job reference, decimal read budget and explicit campaign name. It
derives device identity, profile, targets, bounds and image digests from its
immutable source plan and current durable target lineage. This is a read-only
authority operation, separate from mechanics `ManagedControlAction` and its
four existing wire values. It creates no flash plan, permit, recovery grant,
mechanics receipt or hardware-support declaration.

Admission requires a terminal successful job with every sealed step completed,
matching plan id/digest and a unique correlated semantic-success receipt per
step. The supported profile is `org.openharmony.dayu200@1.0.0`; the plan must
contain exactly the nine unique image write targets `uboot`, `resource`,
`boot_linux`, `ramdisk`, `system`, `vendor`, `updater`, `chip_ckm`, `userdata`.
Each range must have a canonical SHA-256, positive length no greater than
16 GiB, nonoverflowing end and sector-aligned start. The original plan and
current runtime must have the same explicitly named campaign. No active job
may run concurrently in the owning runtime.

Before and after each partition read the authority requires fresh strong
Normal-mode discovery and same-handle probe facts matching the entire stored
stable identity. It invokes only the hash-bound HDC executable with an exact
USB target and fixed argument arrays. Only allowlisted partition names and
validated decimal lengths enter the fixed remote read-only query. Neither
caller shell fragments nor expected image hashes are sent to the device.

The query resolves the partition symlink to a block node, checks its sysfs
major/minor, start and capacity against the sealed range, and reads mountinfo
by device number, including bind mounts and superblock options. Block holders
and unsupported mappings fail closed. Any writable mount of the partition or
whole disk causes `typedSkip` before reading its contents. The metadata is
rechecked after hashing. The exact image length is read with byte-counted
`dd`, including lengths at and above 2 GiB and non-sector-aligned tails; the
hash pipeline requires `pipefail`, one canonical digest and a success marker.
Short reads cannot match the expected full-image digest. Raw mount paths,
connect keys and raw output are not retained in the public record.

The authority durably creates a unique
`authority/flash-verifications/<record-id>/intent.json` before partition I/O.
After the bounded reads it creates `result.json`; neither file may replace an
existing file. A crash with only an intent establishes no verification and
does not resume automatically. A new explicit request creates a new record.
The original source plan, job journal, receipts and recovery history are never
rewritten. The record binds source job/plan/digest, plan-record digest, stable
target/topology, profile, HDC digest, current epoch, campaign, timestamps,
budget, read domain and each image range. It is not production support evidence.

`arkforge.flash-verification/v1` reports per-range `outcome` (`verified`,
`typedSkip`, `failed`), nullable `verification_strength`, nullable
`observed_sha256`, and nullable `reason`. Only a complete matching digest has
strength `fullHash`. Writable filesystems use reason
`mounted-writable-after-boot`; source mismatch is `content-mismatch`.
Other errors remain failed with a bounded static reason. Target continuity
loss invalidates the aggregate and prevents reads of remaining targets.
Aggregate `outcome` is `verified`, `partial` or `failed`; counts are explicit.

The source plan's `range_length` is the image length, not unwritten trailing
partition space. Normal-mode verification does not prove what the Loader
could read earlier, and cannot reconstruct a writable filesystem's bytes
before its first boot. No remount, freeze, reboot, firmware patch, rescue or
unrestricted-loader fallback is part of this operation.
