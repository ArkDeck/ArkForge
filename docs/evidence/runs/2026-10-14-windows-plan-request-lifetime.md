# Windows firmware-plan request lifetime

## Observed trigger

The Windows inbox-binding correction was published by [PR #8](https://github.com/ArkDeck/ArkForge/pull/8)
as `a3324ca386e983ca4ddb2676c9518a5580d21080`. Its exact head passed Windows
run 38528200589 and Rust/Swift run 38528200571, plus both push runs.

The elevated host-binding call persisted intent, returned from Windows driver
installation, then failed to observe the restarting Loader. That original
result remains `unknown`; no installation was replayed. After the operator
reconnected the board in Loader mode, an independent read-only check matched
the original device instance, location and hardware identity, with system
`winusb.inf`, `WINUSB` service, the ArkForge interface GUID and PnP problem 0.
The published Runtime then discovered that Loader through its native WinUSB
transport. These are host-binding and discovery observations only.

The operator-selected OpenHarmony 7.0.0.43 archive was imported with expected
SHA-256 `781d2eaebe2f8d10a25d13102a38259b134210480b2153786032a3bdea675fed`
and size 725,737,667 bytes. The published Profile covers all nine writable
targets. Its first typed flash-plan call resolved the artifact and exact
Loader but returned `SUPERVISOR_IO_FAILED` during assessment, with no plan or
apply command. The supervisor subsequently ended with the same error family.
No flash job or firmware write had been dispatched.

## Correction

The supervisor request helper used the ten-second lifecycle read timeout for
archive assessment and materialization too. Those calls now use the existing
controller client's 900-second materialization idle-read budget. Lifecycle
and other short requests retain their previous bound. This changes no plan,
permit, acknowledgement, freshness, identity or recovery rule.

An abandoned or malformed caller connection is now contained to that one
request. A failed response write does not terminate the paired owner or its
durable jobs, and no failed request is replayed. An accepted stop still requires
zero active jobs and a successfully written response.

The new sequential-client regression exposed a separate Windows listener gap:
only an accepted pipe instance remained until the next `accept`. A following
client could fail with error 231 while the previous instance closed. The
listener now publishes its next pending instance before returning an accepted
stream, retaining the same owner-only ACL and rejecting remote clients. A
host-primitive regression connects the second client while the first is still
being served, in both blocking and nonblocking modes, without a retry delay.

## Local targeted checks

All final commands exited 0, using the existing fixed Cargo cache, two build
jobs, native Windows target and static CRT. Log and exact argv:
local `windows-inbox-winusb-20261014/plan-lifetime-ready-checks.log` and `.json`.

- `cargo fmt --all -- --check`.
- `cargo clippy --release --target x86_64-pc-windows-msvc --all-targets
  --all-features -- -D warnings`, selecting `arkforge-platform`,
  `arkforge-artifact`, `arkforge-client`, `arkforge-standalone`, `arkforge-cli`
  and `arkforged` with `-p`.
- Native release `arkforged` build so the process-level regression uses an
  actual sibling daemon, followed by `cargo test` for the same six crates and
  `--all-targets` (26.067 seconds for the final test command).
- `git diff --check`.

The regressions exercise an archive reply after the lifecycle deadline,
continuous listening in both modes and a real paired host-only runtime
surviving truncated/malformed frames and an abandoned response. They do not
flash a device. The original tests first caught the listener gap; the isolated
reproduction also failed before its fix. A later broader run required the
already-installed Git `gzip` directory on test PATH. Both original logs remain
preserved; no assertions were relaxed. macOS and Swift checks run in CI.

## CI

Pending at commit time; the PR records the exact head and actual run results.

## Qualification

The original failed plan and binding records remain unchanged in local run
storage. No successful firmware-write/readback result, AF-W1 qualification,
production support or `REAL_DEVICE_PASS` is asserted by these software checks.
