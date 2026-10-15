# DAYU200 pre-mount verification implementation

Status: host validation only; no hardware result claimed by this record.

The stock eMMC Loader read face remains windowed. A temporary acceptance
initramfs now measures the complete userdata image range before stock init,
mounts only procfs/sysfs and emits a bounded kernel-RAM report. Independent
verification binds the exact derivative, sealed write range, nonce, boot ID,
storage CID and current target lineage. A separately admitted restoration
writes the original gzip bytes plus zero fill over the full 4 MiB ramdisk
partition so no temporary CPIO suffix survives. It never rewrites userdata.
Historical jobs, skips, recovery records and support declarations are retained.

## Local targeted checks

On Windows x86_64, `cargo fmt --all -- --check`, workspace Clippy with
`--all-targets --all-features -- -D warnings`, the daemon release build, and
`cargo test --workspace --all-targets` all exited 0. Cargo commands used
`--locked --offline --release --target x86_64-pc-windows-msvc -j 2`, the existing
fixed session cache and static CRT flags. Full test execution took 175.654 s.
Local log references: `preboot-userdata-20261015/checks-03.{json,log}`.

The first attempts exposed an invalid new help example and a forbidden
CLI-to-platform dependency. Both were fixed; the latter moved preparation into
the existing standalone layer. No architecture guard or assertion was weakened.
Sandbox CAS permissions and Git Bash initialization required the existing
unsandboxed host-test path, not a device privilege bypass.

An offline check against the real pinned ramdisk confirmed byte preservation,
regular-file CPIO replacement of `/init`, LF script bytes, shell syntax, exact
4 MiB outputs, and an original-plus-zero restoration with SHA-256
`bdd1f4eacbe545f5a553b529980bc487723d37554148058dd551c241abf86de4`.
The original and restored gzip streams decompress identically. Log reference:
`preboot-userdata-20261015/offline-derived-02/result.json`. This is host evidence,
not real-device acceptance. Swift was not run on the Windows host.

The conformance generator changed five files for the new Provider lowering
identity. Diagnostic structure, full-restore actions and effects are unchanged;
only 64-hex identity/plan/manifest hashes and their CBOR bytes differ.

## CI

PR and run IDs will be recorded after submission. macOS Rust/Swift and native
Windows CI must both pass before merge. Board execution uses a subsequent
build from merged main; this document does not claim that happened.
