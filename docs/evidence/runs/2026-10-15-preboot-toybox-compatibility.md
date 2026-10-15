# Preboot holder applet compatibility

The pinned toybox omits `tr`. Both holder readiness and normal-mode discovery
used that unavailable applet, so the reader skipped every process before
examining its report. Match the complete NUL-delimited command with `od` and
shell builtins instead. A 30-byte bound includes one byte beyond the expected
29-byte argv, rejecting matching prefixes with suffixes or extra arguments.
Report metadata, nonce, boot, storage and range checks remain unchanged.

## Local targeted checks

- `bash scripts/test-preboot-holder.sh`: exit 0. Both shipped matchers execute
  against an applet shim that permits only `od`, accepting the exact argv and
  rejecting eight malformed, oversized or missing inputs. macOS and Windows
  CI now run this regression.
- `cargo fmt --all -- --check`: exit 0.
- Workspace Clippy with all targets/features and warnings denied: exit 0.
- Release/static-CRT daemon build: exit 0.
- `cargo test --locked --offline --release --target x86_64-pc-windows-msvc
  -j 2 --workspace --all-targets`: exit 0, 162.173 seconds.
  Logs: `preboot-userdata-20261015/applet-checks-01.{json,log}`.
- Candidate CLI offline preparation: exit 0; closed script bytes match the
  source, base/dependency bytes stay exact, complete restoration is unchanged,
  and corrupted support remains refused. Record:
  `preboot-userdata-20261015/offline-applets-01/result.json`.
- Isolated ARM execution reproduced the missing applet. The fixed wrapper and
  reader then retained and read a deliberately failed measurement report both
  before and after unlinking its RAM paths. This used the firmware's actual
  ARM userspace with a generic ARM64 virtual kernel, no attached disk/network,
  and a test-only storage-identity file. It is host emulation, not a successful
  userdata measurement or hardware evidence. Logs:
  `preboot-userdata-20261015/boot-path/qemu-{holder,candidate-reader}.log`.
- Swift was not run locally; no Swift or wire schema changed.

## CI

Pending this PR head's Rust/Swift and Windows workflows. Device execution
uses a build from merged main after all checks pass. Prior Runtime failures
remain separate and local; this change does not promote hardware support.
