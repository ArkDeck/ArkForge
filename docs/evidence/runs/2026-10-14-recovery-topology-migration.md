# Recovery across a Windows topology encoding change

The budget-grant implementation is published in PR #20. Earlier Windows Normal
jobs used location-path topology, while current observation uses canonical
companion-connector topology. A new narrow proof derives both encodings from
the same live node and recomputes the original full stable identity; all facts
other than observation id/topology must stay unchanged. It does not associate
a Loader serial with a historical Normal serial.

## Local targeted checks

- `cargo fmt --all -- --check`: exit 0; private log `recovery-topology-fmt.log`.
- `cargo clippy --locked --offline --workspace --all-targets --all-features -- -D warnings`:
  exit 0; private log `recovery-topology-clippy.log`. The local incremental
  cache used copying when sandbox ACLs prevented hard links; no source lint failed.
- `cargo test --locked --offline --workspace --all-targets`: exit 0, 731 tests;
  private log `recovery-topology-workspace.log`. Windows owner-only ACL/IPC
  fixtures ran with the normal user token, existing gzip on the process PATH,
  two build jobs and the session's fixed external Cargo cache.
- `git diff --check`: exit 0. No fixture bytes or public IPC schema changed.

Swift was not run locally on Windows; macOS PR CI covers it. These tests use
fixtures and do not establish hardware acceptance.

## CI

[PR #21](https://github.com/ArkDeck/ArkForge/pull/21), head
`6b978ee34a511acf92ae55208b25e5aaffc56e69`: Windows PR `38573350155`,
macOS/Rust PR `38573350190`, and push runs `38573295280` / `38573295273`
all succeeded. Merged at `2026-10-14T13:41:22Z` as
`fd49c54a468cffdf5f098b764fcd91b31898b834` under the user's explicit all-green
merge authorization. The published-source admission result follows in
`2026-10-14-recovery-budget-admission.md`.
