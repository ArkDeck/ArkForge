# Identify retained RAM reports by filesystem magic

The report reader requires RAMFS or TMPFS backing. Using `stat -f %T`
incorrectly makes that decision depend on a utility's display-name table;
the pinned toybox omits RAMFS and labels it `unknown`.

Use `stat -f %t` and accept exactly Linux RAMFS_MAGIC (`858458f6`) and
TMPFS_MAGIC (`1021994`). The allowed filesystems do not change. Unknown and
persistent filesystem numbers remain rejected even when a utility displays
a RAM name. Holder, path, ownership, mode, size, nonce, boot, storage identity
and range checks remain unchanged.

The constants are defined by the [Linux filesystem ABI](https://github.com/torvalds/linux/blob/v6.6/include/uapi/linux/magic.h).

## Local targeted checks

- `python hack/test-preboot-report.py`: exit 0, four tests with 17 paths.
  The complete production query runs against isolated host applet fixtures.
  RAMFS with an unknown display name and TMPFS with a live report path pass.
  F2FS, ext4, overlayfs, unknown or malformed numbers fail without exposing
  the report. Missing/duplicate holders, extra argv, wrong owner/mode, empty
  or oversized files, nonregular files and an unexpected path also fail.
- The same RAMFS fixture with the unmodified published query exits 1 and
  emits no report, demonstrating that the regression is detected.
- `python hack/test-preboot-init.py`, `sh hack/test-preboot-holder.sh`
  and `git diff --check`: exit 0. Logs:
  `preboot-userdata-20261015/fs-magic-shell-checks-01.{json,log}`.
- `cargo fmt --all -- --check`: exit 0. Workspace Clippy with all targets
  and features and warnings denied: exit 0. Release daemon build: exit 0.
  `cargo test --locked --offline --release --target x86_64-pc-windows-msvc
  -j 2 --workspace --all-targets`: exit 0, 90.944 seconds. Logs:
  `preboot-userdata-20261015/fs-magic-rust-checks-01.{json,log}`.
- Swift was not run locally on Windows; no Swift or public wire schema changed.

These are host fixtures, not physical-device evidence. Previous failed
verification records remain failures and are not rewritten by this fix.

## CI

[PR #35](https://github.com/ArkDeck/ArkForge/pull/35) merged after all
four workflows succeeded on head `c4ba167e6cec1225d16f6a4b1dbab21ce3d203bc`.

- Rust/Swift: [38705225056](https://github.com/ArkDeck/ArkForge/actions/runs/38705225056), [38705174872](https://github.com/ArkDeck/ArkForge/actions/runs/38705174872).
- Windows: [38705224812](https://github.com/ArkDeck/ArkForge/actions/runs/38705224812), [38705174853](https://github.com/ArkDeck/ArkForge/actions/runs/38705174853).

This CI follow-up preserves the findings and validation scope recorded above.
CI success does not rewrite Runtime outcomes or promote hardware support.
