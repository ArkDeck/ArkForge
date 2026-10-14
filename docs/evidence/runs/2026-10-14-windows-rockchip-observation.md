# Windows Rockchip-bound Loader observation

Date: 2026-10-14. Software implementation and regression record only.
Base: `2d579d693ca05c54ca0bb2a9fbd08248df6834de` (kernel catalog verification fix).

## Behavior

Previously, native Windows discovery could enumerate only the private ArkForge
WinUSB interface. A Loader already bound to the Rockchip driver was absent from
that census. The added path uses documented Windows USB hub queries to obtain
the complete descriptor identity without opening a Rockusb device or pipe.

The PnP driver key must equal the key reported by the exact parent hub port.
Device and binding snapshots surround two reads of configuration and USB string
descriptors. A final PnP census rejects hotplug or rebinding during observation.
The exact selector checks VID/PID, interface class/subclass/protocol and the
Loader bit; a Maskrom device sharing the VID/PID is not emitted as a Loader.
Unavailable identity fails the census rather than silently hiding a candidate.
Vendor-bound records always refuse bulk claims before opening a vendor handle.

Only one vendor observation worker can be outstanding. The caller has a
five-second budget. If a synchronous Windows query outlives that budget, its
worker keeps ownership of the handle and buffer until return, refuses follow-on
queries after cancellation, and blocks another census. Caller timeout is not
represented as completed kernel cancellation.

## Regressions

The Windows-only scripted tests exercise complete descriptor identity,
Loader/Maskrom discrimination, driver-key mismatch and truncation, changed USB
address/Loader bit/driver key/serial/configuration, every truncated configuration
prefix, invalid and duplicate interfaces/endpoints, absent versus malformed USB
strings, exact PnP filtering, expired/cancelled budgets, timed-out resource
retention, refusal of another worker, worker failure cleanup, duplicate exact
matches and refusal of vendor bulk claims. Fixtures perform no device I/O.

## Local targeted checks

- `cargo fmt --all -- --check`: exit 0. Log:
  `rockusb-observation-fmt-20261014.log` in the local task tools directory.
- `git diff --check`: exit 0.
- Local Rust compilation/tests were not run while the independent 24-hour
  Windows soak was active. Native compilation, Clippy and workspace tests are
  delegated to the existing GitHub Windows workflow; the Rust/Swift workflow
  also checks the shared workspace. Results are recorded on the PR.

## CI

Pending the first push at the time this record was committed. Read the PR's
current-head checks for the actual run IDs and outcomes; this record does not
claim a pending check passed.

## Qualification boundary

No driver was installed, rebound or opened through its vendor interface, and
no Loader transition, vendor transfer, flash, reset, or hardware acceptance ran.
No new hardware evidence or supported production transport is declared here.
AF-W1 still requires its exact trusted production package, cross-account checks
and physical WinUSB-bound Loader acceptance. Rockchip bulk adaptation separately
needs a verified interface contract and reviewed transfer, timeout/cancellation,
error-reset and exclusivity behavior before any hardware dispatch. This change
does not substitute driver signature validity for those proofs.
