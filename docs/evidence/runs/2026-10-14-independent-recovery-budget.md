# Independent recovery budget grants

The former one-reservation-per-source policy was an implementation limit.
AF-REC-001's no-replay invariant and ArkDeck POL-RECOVERY-001 remain unchanged.
The proposed authority policy and implementation add distinct grants under a
fixed cumulative cap of 16 epochs, preserving all legacy consumption bytes.

## Local targeted checks

On Windows, with two Cargo build jobs and the session's fixed external cache:

- `cargo fmt --all -- --check`: exit 0; private log `recovery-budget-fmt.log`.
- `cargo clippy --locked --offline --workspace --all-targets --all-features -- -D warnings`:
  exit 0; private log `recovery-budget-clippy.log`.
- `cargo test --locked --offline --workspace --all-targets`: exit 0, 727 tests;
  private log `recovery-budget-workspace.log`. Existing Git-for-Windows gzip
  was added to this process's PATH after the first run exposed its absence.
  Owner-only ACL and local IPC fixtures ran outside the filesystem sandbox.
- `git diff --check`: exit 0. No fixture bytes were regenerated.

Swift was not run locally on Windows; the macOS PR workflow runs the SDK tests.
These checks use fixtures and provide no new device-execution evidence.

## CI

[PR #20](https://github.com/ArkDeck/ArkForge/pull/20), head
`03977903b9b835bf6826891b8b104788fe464ae8`: Windows PR run `38571978580`,
macOS/Rust PR run `38571978778`, and push runs `38571928961` / `38571928985`
all succeeded. Merged at `2026-10-14T13:33:48Z` as
`6ecf79994a7154e841e8ce4b52024609aedea9a5` under the user's explicit all-green
merge authorization. Hardware acceptance still requires fresh proof linking
the current device to every original source identity; budget availability
cannot replace it.
