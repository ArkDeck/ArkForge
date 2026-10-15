# Restore a boot-loop source after Runtime restart

Restoration planning can constrain its target with `--source-job`. Runtime
checks the source's complete success receipts through the final reboot and
matches the fresh device against its durable original or current binding.
The listener accepts sparse recovered event cursors and no longer depends on
transient admission events. It checks the plan before cancelling a waiting
source, and still submits at most one restoration apply.

## Local targeted checks

- `cargo fmt --all -- --check`: exit 0.
- `cargo clippy --locked --offline --release --target x86_64-pc-windows-msvc -j 2 --workspace --all-targets --all-features -- -D warnings`: exit 0.
- `cargo build --locked --offline --release --target x86_64-pc-windows-msvc -j 2 -p arkforged --bin arkforged`: exit 0.
- `cargo test --locked --offline --release --target x86_64-pc-windows-msvc -j 2 --workspace --all-targets`: exit 0.
- Rust logs: `preboot-userdata-20261015/boot-loop-durable-checks-03.{json,log}`.
- `python -m unittest discover -s scripts -p test_restore_boot_loop.py -v`:
  exit 0, 17 tests. Output is retained in the local task execution record.
- `git diff --check`: exit 0.

New cases exercise reloaded durable bindings, original Normal and current
Loader identity, changed discovery or same-handle probe facts, missing/duplicate
receipts, unconfirmed outcomes and sparse snapshots without admission events.
Probe fixtures use the daemon's unversioned profile ID; the source keeps its
versioned reference, and both are checked without changing the stable digest.
An initial Rust type-comparison error was fixed before the passing run. Python
tests initially encountered sandbox temporary-directory permissions and passed
unchanged outside that sandbox. These are host tests, not hardware acceptance.
Swift was not run locally on Windows; no public daemon wire message changed.

## CI

Pending this PR head's Rust/Swift and Windows workflows. Exact run IDs and final
results are recorded in the PR body without amending a green commit.
