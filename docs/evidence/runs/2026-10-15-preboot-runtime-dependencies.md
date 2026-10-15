# Preboot runtime dependency closure

The original acceptance ramdisk's toybox has a `DT_NEEDED` dependency on
`libcrypto_openssl.z.so`, but the ramdisk omits that library. Its dynamic loader
refuses unresolved digest symbols before any applet can run. The derivative
now includes the original firmware's pinned library in a pinned compressed
newc archive. The original gzip bytes, other firmware members and full-extent
restoration stay unchanged. No persistent mount supplies the dependency.

The host producer and image recognizer require the complete dependency archive
byte-for-byte. The CLI requires `--support-archive`, records both dependency
digests and bounds its input. The firmware preparation helper accepts only the
pinned raw library, generates the closed archive and checks its compression pin.

## Local targeted checks

- `cargo fmt --all -- --check`: exit 0.
- `cargo clippy --locked --offline --release --target x86_64-pc-windows-msvc
  -j 2 --workspace --all-targets --all-features -- -D warnings`: exit 0.
- Release/static-CRT daemon build: exit 0.
- `cargo test --locked --offline --release --target x86_64-pc-windows-msvc
  -j 2 --workspace --all-targets`: exit 0, 46.855 seconds. The initial sandbox
  attempt could not change test ACLs or start system gzip; the normal-host run
  used identical assertions and source. Logs:
  `preboot-userdata-20261015/dependency-checks-01.{json,log}` and
  `preboot-userdata-20261015/dependency-checks-02.{json,log}`.
- An isolated ARM userspace probe reproduced the missing dependency, then
  passed toybox startup, an independently checked SHA-256 vector, exact byte
  reads and read-only FD holder execution with the original library added.
  Logs: `preboot-userdata-20261015/qemu-host/`. This is host emulation.
- Offline derivative validation passed: original bytes preserved, support
  archive decompresses to the exact library, scripts retain their closed
  content, restoration clears all 4 MiB, corrupted support is refused before
  output. Record: `preboot-userdata-20261015/offline-dependencies-03/result.json`.
  Earlier attempts used a still-building CLI and sandbox-blocked Bash; their
  directories remain separate. No device was accessed by these checks.
- Swift was not run locally; no Swift or wire schema changed.

## CI

Pending the exact PR head's Rust/Swift and Windows workflows. Device execution
uses a new build from merged main after all checks pass. Hardware records stay
separate and local; host emulation does not promote production support.
