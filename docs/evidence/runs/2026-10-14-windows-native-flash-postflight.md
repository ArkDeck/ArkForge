# Windows native flash and postflight follow-up

## Final disposition

Native Windows RockUSB performed all nine firmware writes using Microsoft's
inbox WinUSB binding. It read back and matched the complete `uboot` and
`resource` images, then reset the board. Independent exact-target HDC reads
confirmed OpenHarmony 7.0.0.43. The complete automated workflow is **not passed**:
the first job was safely cancelled at its post-reset rebind boundary, and a
subsequent normal-start job has an immutable unknown mode-control outcome.
The current Runtime refused a superseding plan; no new device dispatch followed.

| Runtime result | Scope |
| --- | --- |
| Nine semantic-success writes | `uboot`, `resource`, `boot_linux`, `ramdisk`, `system`, `vendor`, `updater`, `chip_ckm`, `userdata` |
| Full-hash readback | `uboot`: 4,194,304 bytes; `resource`: 5,652,480 bytes |
| Explicit verification skip | The other seven images: `skipped-lba-read-window`; not verified |
| Reset | `DEVICE_RESET` semantic-success receipt |
| System version | Independent read-only HDC diagnosis; no Runtime postflight receipt |

The final read-only snapshot observed `hdc-normal` with `serialAndTopology`
identity and no active jobs. The task-owned Runtime was stopped through the
typed CLI. No external HDC server was stopped. Raw local records remain
unchanged. This is neither AF-W1 qualification nor production Windows support.

## Published run

[PR #9](https://github.com/ArkDeck/ArkForge/pull/9) was published as
`1cbfcef812408e41f005003d9322b05b8bfc6bfc` after Windows PR/push runs
38531967221/38531962699 and Rust/macOS/Swift PR/push runs
38531967214/38531962655 succeeded on its exact reviewed head.

An unsigned native Windows build of that published revision ran the operator's
named hardware campaign using the existing Microsoft inbox `winusb.inf` and
`WINUSB` binding. No Rockchip bulk driver, custom driver package or raw flash
command was used. This remains a campaign build, not a production release.

The selected OpenHarmony 7.0.0.43 archive has SHA-256
`781d2eaebe2f8d10a25d13102a38259b134210480b2153786032a3bdea675fed`.
Typed planning sealed a 22-step full-restore plan. Runtime journal receipts
report semantic success for all nine partition writes (4,017,513,001 source
bytes), full-hash readback for `uboot` and `resource`, seven explicit
`skipped-lba-read-window` results, and a successful `DEVICE_RESET`.
The seven skipped ranges are not full-hash passes.

The original job stopped progressing in `rebindWait`: Windows reported the normal-mode HDC
device, but the published Windows enumerator sees only the private Loader
interface and vendor-bound Loaders. An independent, exact-target read-only HDC
diagnosis returned `const.ohos.fullname=OpenHarmony-7.0.0.43` and
`const.product.model=ohos`. That diagnosis is not a Runtime postflight receipt
or hardware qualification. The original job, intent and receipts are retained;
none of its writes is replayed.

## Published observation and campaign corrections

[PR #10](https://github.com/ArkDeck/ArkForge/pull/10) was published as
`cad59911abf4fd527ab31f1b6d4b2a3c4f68035a`. Windows PR/push runs
38536342297/38536287168 and Rust/macOS/Swift PR/push runs
38536342258/38536287176 all succeeded on exact head
`b92f0e761afee16271cb973777a64b7eb24a71d2`.

Observe the existing WinUSB-bound normal personality through the same bounded,
read-only hub descriptor path used for vendor-bound Loaders. Every PnP/hub
descriptor and driver correlation still has to agree; observation-only records
cannot open bulk pipes. Errors identify which identity field failed without
including its private value. A local read-only API test isolated an
embedded-null refusal to the hub driver-key structure's zero padding. Its
terminated WCHAR key is now decoded separately from padding; nonzero data
after the terminator, missing termination and a PnP key mismatch still refuse.
USB identity strings retain their original strict parser. End-to-end Runtime
postflight still requires an end-to-end succeeded execution.

Derive Windows topology from validated PnP physical location paths for both
observation paths. The prior device-instance hash changed with PID/serial and
could not represent a mode-independent port. Existing sealed identity facts
are not rewritten or migrated. Missing/malformed physical paths fail closed.

Expose the existing explicit campaign check on device list/wait, watch, cancel
and job list/show/reconcile. Missing and mismatched names still refuse before
the request. Cancellation retains its exact optimistic sequence and safe
boundary semantics. The cancel-help fixture adds only that optional parameter;
its generator, bytes and manifest pins change together.

## Local targeted checks

Native Windows USB unit tests cover normal observation
without bulk claims, unchanged physical topology across personality changes,
port changes, malformed paths, padded driver keys and exact PnP/hub product matching. The paired
campaign command regression passed, preserving the same processes/epoch and
zero jobs across missing, wrong and matching campaign calls. Device commands
in that regression stop at the refusal gate so they do not enumerate hardware.

The first version of that test did enumerate the attached device and found the
identity parse refusal above; it is not hardware acceptance. The existing
read-only API regression reproduced it and now includes the stable error
detail in its assertion. Original compile, parse, input-validation and Clippy
failure logs are retained. No existing assertion was relaxed.

Final commands all exited 0, with the fixed session cache, two build jobs,
native Windows target and static CRT. Exact argv and logs are in local
`windows-inbox-winusb-20261014/normal-rebind-final-checks.log` and `.json`:

- `cargo fmt --all -- --check` (1.068 seconds).
- `cargo clippy --release --target x86_64-pc-windows-msvc --all-targets
  --all-features -- -D warnings`, selecting `arkforge-usb`, `arkforge-cli`,
  `arkforged`, `arkforge-standalone` and `arkforge-conformance` (5.590 seconds).
- Sibling `arkforged` release build (14.770 seconds), then `cargo test` for the
  same five crates with `--all-targets`: 246 passed, no failures (99.420 seconds).
  The final run includes the existing autostart and read-only API tests; neither
  is skipped. Their host USB metadata reads are not hardware qualification.
- `git diff --check`.

The conformance regeneration changed only cancel-help stdout, its case digest
and the fixture-manifest pins. Full workspace/macOS/Swift checks run in CI.

## CI

PR #9 and #10 results are recorded above.
This record does not assert an end-to-end succeeded job, AF-W1 qualification,
production support, or `REAL_DEVICE_PASS`.

## Subsequent normal-start run and HDC correction

The published CLI safely cancelled the original job at its rebind boundary,
after 21 of 22 steps, using its exact event sequence. All write/readback/reset
receipts remain intact. The replacement published Runtime observed the normal
personality with `serialAndTopology` identity and sealed a new 23-step plan.
Its first managed-control request expired after 120 seconds, before any
partition writes. The job is `outcomeUnknown`; the original intent is not
replayed. Typed read-only reconciliation retained `stillUnknown`. The public
recovery guide is not a successful recovery-plan assessment; the actual
materialization result after publishing the correction is recorded below.

Read-only HDC diagnostics isolated the Windows environment prerequisite:
`list targets -v` returned in 62 ms with the inherited environment, while an
empty environment timed out at 10 seconds. Supplying only `SystemRoot`
returned in 51 ms. `WINDIR` alone and `TEMP`/`TMP` alone each timed out at
3 seconds. These are diagnostic samples, not performance or hardware
qualification. Local logs are `hdc-readonly-diagnostic.json` and
`hdc-minimum-environment-diagnostic.json` in the campaign directory.
An independent fixed-argv read-only query with only `SystemRoot` matched the
Runtime's exact case-sensitive serial digest and returned OpenHarmony 7.0.0.43
and model `ohos`; `hdc-properties-minimum-environment.json` preserves that
diagnosis. It is not a Runtime postflight receipt.

The correction retains only `SystemRoot` on Windows. A real child-process
regression asserts that it is present and all other inherited environment
variables are absent. The current SDK also returns the documented trailing
`hdc` column and unrelated UART `Ready` rows; parser regressions cover these,
malformed rows, and exact USB/Connected selection. The official format is
documented in [OpenHarmony HDC](https://github.com/openharmony/docs/blob/master/zh-cn/application-dev/dfx/hdc.md#查询设备列表).

### Local targeted checks

Final commands exited 0 using the same fixed cache, two build jobs, native
Windows target and static CRT. Exact commands and logs are in
`windows-inbox-winusb-20261014/hdc-environment-final-checks.log` and `.json`:

- `cargo fmt --all -- --check` (1.060 seconds).
- Release/native-target Clippy, all targets/features with `-D warnings`, for
  `arkforge-standalone` and its direct consumer `arkforge-cli` (3.011 seconds).
- Sibling daemon build (0.248 seconds), then all-target tests for those two
  crates: 102 passed, no failures (47.178 seconds). The ignored Windows child
  assertion is explicitly executed by its parent test, which passed.
- `git diff --check`.

Full workspace/macOS/Swift checks are delegated to CI. No protocol, profile,
permit, recovery-coverage declaration or conformance fixture was changed.

### CI

[PR #11](https://github.com/ArkDeck/ArkForge/pull/11) was published as
`0b270deca3631c96caf4c5b1452ab6fcebc5f3d7`. All checks succeeded on exact head
`9f5faaec7c937e309849e4e2714b0d8027d6894c`: Windows PR/push
38538474805/38538416977; Rust/macOS/Swift PR/push 38538474499/38538416781.

## Recovery assessment after the HDC correction

The clean published build retained the same mechanics daemon SHA-256
`44460dd3fd57c1f0535e524574e7f27c5e4e85bdbd18b862195fbefdc370ab8f`;
the corrected CLI SHA-256 is
`e54d9b39d24178a17c5df7b44fc1f16b87e337158b8dff949ff6e8cb5bbbd540`.
Both are unsigned campaign binaries. The restarted Runtime observed the same
normal device and original terminal jobs, then refused typed `job recover`
with `nothingToRecover`, exit 3. There is no recovery plan, acknowledgement
set, apply or additional flash dispatch.

At that published revision, the mechanics `PlanStore` kept private plans only
for the daemon's lifetime (`crates/arkforge-engine/src/lib.rs`). Journals
reloaded after restart, but the original private plan did not. The service could not load
the job's recovery contract via `stored_plan_for_job`; its closed managed-control
fallback refused supersession. The public guide's `eligible` field was not
proof of materialization eligibility: the final guide had no contract and
`complete_overwrite_supported=false`. These are the observed implementation
limits, not permission to reconstruct or edit authority facts manually.

The subsequent [private-plan persistence and restart validation](2026-10-14-durable-private-plans.md)
fixes storage for newly materialized plans. It does not recover the missing
historical bytes or change this campaign's unknown outcome.

[AF-REC-001](../../../spec/requirements/recovery.md#af-rec-001--never-replay)
prohibits re-sending the old intent or calling another full flash a retry.
No safety predicate, recovery declaration or journal was changed to get past
this refusal. Completing the automated acceptance requires a reviewed recovery
path with the missing durable proof; chat confirmation cannot supply it.

Local records under `windows-inbox-winusb-20261014/` include
`flash-apply-request-lifetime.json`, `old-job-safe-stop.json`,
`flash-apply-normal-start.json`, `normal-start-after-reconcile.json`,
`recovery-plan-hdc-environment.json`, `final-unknown-job.json`,
`final-runtime-status.json` and `final-runtime-stop.json`. They preserve both
attempts and the refusal, rather than replacing them with a successful summary.

Final record-only checks: exact receipts, CI run identities, local log names,
relative links and `git diff --check`. No additional Rust tests or hardware
execution were performed for this documentation-only follow-up.
