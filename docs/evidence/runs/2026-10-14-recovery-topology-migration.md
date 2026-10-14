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

Exact-head PR results are pending at this revision. The next execution record
will record them without amending a green head. Device acceptance still needs
fresh original-mode identity proof before any new grant can be consumed.
