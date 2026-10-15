# Pre-mount userdata verification
status: proposed

Normal boot mounts userdata read-write, so its later hash cannot prove the
just-written image. The measured stock U-Boot READ_LBA window and the pinned
USB loader both stop at 32 MiB on eMMC. Neither provides this read path.

For the explicitly authorized DAYU200 acceptance campaign, derive a temporary
ramdisk from the original immutable image. Linux's documented concatenated
newc initramfs format permits an additive overlay: replace the `/init` symlink
with a regular wrapper and add a bounded shell measurement using the ramdisk's
existing tools. Preserve the gzip base and all other firmware members exactly.

The wrapper mounts only procfs and sysfs, discovers the pinned MMC partition,
checks its range, label, holders and absence of all MMC mounts, reads the full
1,468,006,400 image bytes and emits one kernel-RAM report. It then removes its
virtual mounts and executes the original init. It never repairs or mounts
userdata. Missing reports fail verification while the bounded wrapper still
permits normal boot and a separately admitted restoration.

`artifact preboot-ramdisk` creates the host derivative with a random nonce.
Ordinary typed full restore writes the derived bundle. `flash verify
--preboot-ramdisk <digest>` measures the eight static ranges normally and
validates the pre-mount userdata report against the sealed write, nonce,
current boot and storage identity. No expected digest enters device commands.
The independent record names each measurement stage and retains prior records.

`flash restore-ramdisk` seals a distinct original-ramdisk-only plan. Apply
requires its exact digest and acknowledgements and the ordinary Runtime
authority. A final Normal read proves the original ramdisk restored. Restoration
does not rewrite userdata and is never called complete-overwrite recovery.
Both temporary and restoration images cover the full 4 MiB ramdisk extent.
The restoration retains the original 2,373,467-byte gzip member and fills all
remaining bytes with zeros. Rewriting only the original image length would
leave temporary CPIO entries behind; its prefix hash alone is insufficient.
The restoration pin is the hash of the complete original-plus-zero image.
Neither phase upgrades production support. Unknown effects are never replayed.

The Core overlap prohibition requires two plans; it is not relaxed. A failed
measurement still requires restoration through a new admitted plan if identity
and outcome gates permit. If they do not, report the temporary image as still
installed and stop dangerous dispatch; no silent fallback or manual raw write.

The native Provider lowering identity includes the new intent. Generated
conformance changes are limited to that identity, its maturity-key projection
and resulting plan/fixture digests; existing full-restore steps and effects
retain their semantics. No authority support record is added.

Host preparation uses `scripts/prepare-preboot-firmware.py` with a published
CLI, the original archive and its independent SHA-256. It emits two derived
bundles and a provenance record without modifying the source. Import the
acceptance bundle for the ordinary full restore; after independent preboot
verification, import the restoration bundle for `flash restore-ramdisk`, apply
its exact plan and verify that restoration job. If the preboot read fails,
retain that failure and still restore when normal identity/admission permits.

Sources: [Linux initramfs format](https://docs.kernel.org/driver-api/early-userspace/buffer-format.html),
[Linux v5.10 unpacker](https://github.com/torvalds/linux/blob/v5.10/init/initramfs.c).
The pinned firmware's startup init executes its required mounts only after
`SystemPrepare` starts; this wrapper runs before that executable.
