# Bounded preboot measurement timeout

Default TERM did not stop the pinned shell while it waited for a blocked hash
pipeline under PID 1. Use `timeout -s KILL 300` so this read-only measurement
cannot ignore the timeout signal. The wrapper still replaces partial output
with a failure marker, removes its virtual mounts and executes stock init.
No persistent write requires a graceful measurement shutdown.

## Local targeted checks

- `bash scripts/test-preboot-holder.sh`: exit 0; both exact command matchers and
  their eight rejection cases remain intact.
- `cargo fmt --all -- --check`: exit 0.
- Workspace Clippy with all targets/features and warnings denied: exit 0.
- Release/static-CRT daemon build: exit 0.
- `cargo test --locked --offline --release --target x86_64-pc-windows-msvc
  -j 2 --workspace --all-targets`: exit 0, 164.314 seconds. Logs:
  `preboot-userdata-20261015/timeout-checks-01.{json,log}`.
- The actual ARM userspace in an isolated generic ARM64 virtual machine used
  a test-only blocked pipeline and a shortened three-second timeout. Default
  TERM had not handed off to stock init after 120 seconds; KILL handed off at
  11.4 guest seconds, including wrapper readiness and boot overhead. The empty
  virtual machine then stopped at its intentionally absent second-stage root.
  Logs: `preboot-userdata-20261015/boot-path/qemu-candidate-timeout.log` and
  `qemu-candidate-timeout-kill.log`. No disk, network or physical device was
  attached. These are host diagnostics, not hardware acceptance.
- Candidate CLI offline preparation: exit 0; generated init bytes match the
  source, pinned inputs and full restoration stay unchanged, and corrupted
  support is refused. Record:
  `preboot-userdata-20261015/offline-timeout-01/result.json`.
- Swift was not run locally; no Swift or wire schema changed.

## CI

[PR #31](https://github.com/ArkDeck/ArkForge/pull/31) merged after all
four workflows succeeded on head `c74649005d75c6cf58e433576e81c8b1d754d35a`.

- Rust/Swift: [38688585868](https://github.com/ArkDeck/ArkForge/actions/runs/38688585868), [38688551336](https://github.com/ArkDeck/ArkForge/actions/runs/38688551336).
- Windows: [38688585867](https://github.com/ArkDeck/ArkForge/actions/runs/38688585867), [38688551332](https://github.com/ArkDeck/ArkForge/actions/runs/38688551332).

This CI follow-up preserves the findings and validation scope recorded above.
CI success does not rewrite Runtime outcomes or promote hardware support.
