# Published recovery budget admission

Scope: the existing named Windows hardware campaign, same runtime namespace,
same board, retained historical records. This is a read-only/admission check,
not a full-flash acceptance or a hardware-support upgrade.

Published source `fd49c54a468cffdf5f098b764fcd91b31898b834` was built from a
clean checkout with the fixed Cargo cache and frozen without overwriting any
earlier binary. Unsigned local development images:

| Image | SHA-256 |
|---|---|
| CLI | `be9e7ec4fb1321d6a36fbcc1781c41134562241fcd44dede7487fb3e55a2503c` |
| daemon | `83299e2c790ed682e099a4d3f03d70d7f92b03549e49e3410d71e902445e7ce7` |

Runtime epoch 14 observed the board still in Loader. Both unresolved sources
retain Normal-only durable identities. The typed `job recover` request passed
the former budget-exhaustion obstruction and was refused with
`RECOVERY_PROOF_REQUIRED`: a different USB mode without a durable rebind cannot
establish the original strong identity. No plan/apply command was returned,
no new job or grant was created, and no write/reboot was dispatched. Runtime
was stopped normally; the external HDC server was left alone.

The three original journal hashes and all legacy recovery proof/reservation/
correlation hashes are byte-for-byte unchanged. The complete baseline and
comparison stay in private `recovery-budget-integrity-before.json` and
`recovery-budget-integrity-after.json`. There are still three job journals and
no `authority/recovery-grants` directory.

The real observation also exposed an overly strict string mismatch in the new
Normal encoding-migration guard: daemon serial evidence is `"descriptor"`,
not the fixture spelling `"usbDescriptor"`. The follow-up corrects the guard
and fixtures and explicitly rejects the noncanonical spelling. No serial or
other identity digest is changed. This fixes a false rejection; it cannot make
the currently missing Normal/Loader identity relationship valid.

## Local targeted checks

The published release build exited 0 (`published-recovery-budget-build.log`).
Typed status and deep device list exited 0; recovery planning exited 3 with the
identity proof refusal; daemon stop exited 0. Private raw results:
`runtime-recovery-budget-status.json`, `recovery-budget-device-readonly.json`,
`recovery-budget-authority-blocker.json`, `runtime-recovery-budget-stopped.json`.

Serial-evidence correction: `cargo fmt --all -- --check`,
`cargo clippy --locked --offline --workspace --all-targets --all-features -- -D warnings`
and `cargo test --locked --offline --workspace --all-targets` all exited 0
(731 tests), using private `recovery-serial-{fmt,clippy,workspace}.log` files.
No local Swift run is available on Windows; macOS CI covers the SDK.

## CI

PR #20 and #21 exact-head Windows/macOS checks all passed; their run ids and
merge commits are recorded in the preceding execution records. The correction
PR's exact-head results are pending at this revision.

## Remaining hardware prerequisite

The board must expose its original Normal-mode identity again (normal boot with
the USB connection retained), after which Runtime can re-observe and prove the
exact source identities. Operator confirmation is not that proof. Without it,
POL-RECOVERY-001 requires zero new dispatches. Windows full automatic flashing
acceptance remains incomplete, and neither original unknown is reclassified.
