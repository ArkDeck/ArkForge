# Preserve stock /dev initialization during preboot handoff

The original initramfs contains an empty `/dev`. The holder readiness probe's
`2>/dev/null` creates a regular file there before stock init runs. The pinned
stock init checks only whether `dev/null` exists before mounting `/dev` as
tmpfs. Consequently it skips that mount, and its root switch removes the
device nodes instead of moving a `/dev` mount into the new root.

Append readiness diagnostics to the existing RAM measurement log. The original
init remains responsible for `/dev`; measurement, timeout, report validation
and restoration boundaries are unchanged.

## Local targeted checks

- `python hack/test-preboot-init.py`: exit 0, three tests covering four
  paths: success, missing readiness, measurement failure and timeout. The
  complete wrapper runs against host-only applet fixtures and must hand off
  without creating `/dev/null`. Its failure markers and inherited argument
  and umask are checked. The same fixture with the unmodified published
  wrapper exits 65 at the stock-init mount guard, demonstrating that the
  regression is detected.
- `sh hack/test-preboot-holder.sh`: exit 0; exact argv and eight rejection
  cases for both matchers. Logs for these checks and `git diff --check`:
  `preboot-userdata-20261015/dev-init-shell-checks-01.{json,log}`.
- In isolated ARM64 emulation, both variants use the pinned original ARM
  `init_early` executable and the same RAM-only synthetic root and report.
  The original wrapper skips `/dev` mounting; after the real root switch,
  `/dev/null`, `/dev/random` and `/dev/urandom` are absent. Changing only the
  stderr redirect makes stock init mount `/dev`; the second stage sees the
  tmpfs and character device `1:3`, and still reads the retained RAM report.
  Both completed in about 13.2 host seconds. Logs:
  `preboot-userdata-20261015/offline-startup-investigation/dev-null-{baseline,fixed}-console.{json,log}`.
  No network, disk or physical device was attached. The generic guest kernel
  and synthetic second stage make this a host regression, not hardware proof.
- `cargo fmt --all -- --check`: exit 0. Workspace Clippy with all targets and
  features and warnings denied: exit 0. Release/static-CRT daemon build:
  exit 0. `cargo test --locked --offline --release --target
  x86_64-pc-windows-msvc -j 2 --workspace --all-targets`: exit 0, 195.069 seconds.
  Logs: `preboot-userdata-20261015/dev-init-rust-checks-01.{json,log}`.
- Candidate CLI offline preparation: exit 0. Generated init bytes exactly
  match the tested source; the pinned base and support library are retained,
  the full restoration image is unchanged, and corrupted support is refused.
  Record: `preboot-userdata-20261015/offline-dev-init-01/result.json`.
- Swift was not run locally on Windows; no Swift or public wire message changed.

The upstream [device initialization](https://github.com/openharmony/startup_init/blob/970d25f5bda91afa7e02fce69d134b716d0b141d/services/init/standard/device.c)
and [root switch](https://github.com/openharmony/startup_init/blob/970d25f5bda91afa7e02fce69d134b716d0b141d/interfaces/innerkits/fs_manager/switch_root/switch_root.c)
explain the branch confirmed using the shipped binary.

## CI

[PR #34](https://github.com/ArkDeck/ArkForge/pull/34) merged after all
four workflows succeeded on head `93d6f6512b7f56ac16dbba821575f63060cb89d4`.

- Rust/Swift: [38702184624](https://github.com/ArkDeck/ArkForge/actions/runs/38702184624), [38702137203](https://github.com/ArkDeck/ArkForge/actions/runs/38702137203).
- Windows: [38702184588](https://github.com/ArkDeck/ArkForge/actions/runs/38702184588), [38702137247](https://github.com/ArkDeck/ArkForge/actions/runs/38702137247).

This CI follow-up preserves the findings and validation scope recorded above.
CI success does not rewrite Runtime outcomes or promote hardware support.
