# Independent recovery after an unknown initial mode switch

Status: implementation merged; one published-runtime recovery attempted and
stopped with an unknown initial mode-switch outcome. No partition write was
dispatched by that recovery. This record does not report a hardware flash pass
or publish production support.

The existing Windows campaign has one unknown job whose original private plan
predates durable plan storage. Its verified journal contains exactly one initial
`enter-updater` admission, intent, consuming marker and external dispatch marker,
followed by timeout and read-only reconciliation. There is no partition write,
checkpoint or semantic receipt in that job. The original outcome remains
unknown; the control is not classified as not executed.

The published Runtime's fresh read-only discovery and probe reproduced the
original strong serial, descriptor, topology and protocol identity. Independent
read-only HDC diagnosis returned OpenHarmony 7.0.0.43 and product model `ohos`.
These diagnostic files are local observations, not caller-provided recovery
authority. The old journal remained 4,633 bytes with SHA-256
`5479a09b0dad3361eed62d09846e714bd8bd294b1ea382036226676197fdde0b`.
No private historical action was reconstructed and no device write was made
during the investigation.

The new candidate assessment uses the closed semantic control identity already
in that journal. It checks a distinct stored `supersedingRecovery` plan against
the exact published Profile: every writable partition has a content-bound write
and later verification, all possible byte ranges are covered, and reset, exact
rebind and artifact-derived postflight remain required. Missing historical
private write actions remain unbounded. No Provider coverage or Profile changes
are part of this implementation.

The standalone authority additionally correlates the old public step with its
spent permit and durable target lineage, requires fresh strong physical
identity, and preserves the exact immutable write contents. It writes its own
proof and a single-use reservation, then correlates the newly created job before
any permit is eligible. Source/candidate facts and the reservation are checked
again before admission and managed control. The replacement budget is one epoch
and at most four hours, further bounded by the sealed plan expiration. A crash
before correlation cannot authorize an orphan job; an unknown replacement does
not trigger another attempt.

Only complete replacement receipts and successful postflight produce a separate
`SupersedingRecoveryEpoch` relation. Missing, duplicate, failed or mismatched
required receipts leave the old lane obstruction in place. Typed readback skips
retain their reason and remain distinct from full-hash verification. The old
unknown journal is never changed, and its original plan cannot be started again
even after a successful independent recovery.

## Local targeted checks

Native Windows x86_64 release, static CRT, two Cargo jobs and the fixed session
cache. Owner-only filesystem and IPC fixtures ran in the host environment; no
fixture accesses the board or campaign Runtime.

The changed crates and direct consumers (`arkforge-ipc`, `arkforge-client`,
`arkforged`, `arkforge-standalone`, `arkforge-cli`, `arkforge-arkdeck-adapter`,
`arkforge-conformance`) passed targeted clippy with all targets/features and
`-D warnings`. The matching `cargo test --all-targets` invocation passed 322
tests, failed zero, and reported one opt-in child helper ignored (its parent
executes it explicitly). The final standalone lane checks were rechecked with
clippy and the recovery tests. `cargo fmt --all -- --check` and
`git diff --check` also pass. All commands use `--locked --offline --release
--target x86_64-pc-windows-msvc -j 2` where applicable.

Coverage includes the original source surviving two daemon restarts without
recreating its private plan; no dispatch from assessment; rejection of early,
cancelled, multi-intent, expired and incomplete candidates; identity and permit
drift; single-use reservations; crash-before-correlation; and per-step completion
receipts. Existing conformance and Swift wire vectors are unchanged. An initial
clippy run found one redundant test struct update; it was removed without
changing the guard or assertion.

Private local logs under `windows-inbox-winusb-20261014/` are
`independent-recovery-targeted-tests.log`, `independent-recovery-final-tests.log`,
`independent-recovery-clippy-final.log`, `independent-recovery-last-clippy.log`
and `independent-recovery-last-tests.log`. Diagnosis is recorded separately in
`recovery-independent-readonly-diagnosis.json`. Raw target identifiers, host
paths, private plans and permit bytes are not exported here.

## CI

PR [#15](https://github.com/ArkDeck/ArkForge/pull/15), head
`826dfc7925de261557f826a5895cc11591180e51`, passed Windows PR run
[38560275580](https://github.com/ArkDeck/ArkForge/actions/runs/38560275580)
and macOS Rust/Swift PR run
[38560275787](https://github.com/ArkDeck/ArkForge/actions/runs/38560275787).
Push runs Windows 38560268167 and Rust/Swift 38560268251 also passed.
The merged source is `75629a1592567a9eae1b7d0919b6e587afb2866c`.
Local fixtures do not establish `REAL_DEVICE_PASS`.

## Published-runtime attempt

The named local campaign used an unsigned build of that merged source, CLI
SHA-256 `a682e6a71f310e9bb4f8c0ef324d427378fa5c2c7e32b7b4cdea53a4b6b0ed0a`
and daemon SHA-256
`d0d8d98cc86739eabd67aba3c97061a14a765e3505732fc8c20d5b91649f0330`.
The Runtime accepted its original-source proof, sealed a distinct 23-step
complete-overwrite plan for the same immutable OpenHarmony 7.0.0.43 artifact,
and wrote its own one-use reservation and exact new-job correlation. The user
explicitly approved covering all nine partitions, including userdata, before
apply. These facts do not confer formal AF-W1 or production qualification.

The initial `enter-updater` control reached its 120-second deadline with
`UNIQUE_LOADER_REBIND_NOT_OBSERVED`. The new job is `outcomeUnknown`, completed
0/23 steps, with no semantic receipt, checkpoint, partition write or subsequent
dispatch. Its eight-record journal is 3,138 bytes with SHA-256
`1f7c0788e2a880236a701a7ddedddd99f3c6925ce74f1901edb0f58afa50b61b`.
The two earlier journals retain their exact byte counts and hashes. The Runtime
has a proof, consumed reservation and correlation, but no `.complete` relation.
It was stopped through `daemon stop` after all jobs were terminal; the shared
HDC server was left running.

Read-only native discovery then found Loader with strong descriptor/serial
identity but a different topology digest. Windows PnP metadata explains the
mismatch: HDC-normal uses a SuperSpeed root port and Loader uses its high-speed
companion. Hashing each speed-specific PnP location path made one physical
connector look like two locations. The control's historical outcome remains
unknown; observing Loader later does not rewrite it into success.

The follow-up fix resolves physical connector identity from reciprocal Windows
hub facts in both WinUSB and observation-only discovery. It retains exact-port,
uniqueness, descriptor and serial checks, and does not widen Profile policy.
Its tests cover both speed directions, neighboring ports, different hubs,
one-port connectors, nonreciprocal/changing mappings and malformed query bytes.
Local targeted checks for this follow-up passed: `cargo fmt --all -- --check`
and `git diff --check` exit 0; clippy for `arkforge-usb`, `arkforge-transport`,
`arkforged` and `arkforge-standalone`, all targets/features with `-D warnings`,
exit 0; the same crates plus `arkforge-conformance`, `cargo test --all-targets`,
exit 0 with 251 passed, zero failed and one explicitly launched child helper
ignored by the harness. This includes six new connector tests and 25 existing
USB tests. The fixed-cache Windows release/static-CRT flags above apply.
Logs are `companion-topology-targeted-tests.log`,
`companion-topology-clippy.log` and `companion-topology-final-tests.log` in the
same private campaign directory. macOS Rust/Swift and full Windows verification
run on the follow-up PR; exact run ids and conclusions are recorded in its
delivery. No new hardware write is part of this fix's validation.

The existing single-use recovery reservation is exhausted, and the old sealed
topology is not translated or replaced. A further destructive dispatch remains
blocked; neither a new caller confirmation nor starting another Runtime can
replace the missing proof or replenish that reservation. Private local results
are `independent-recovery-job-final.json`,
`independent-recovery-loader-readonly.json`,
`independent-recovery-pnp-topology-readonly.json` and
`independent-recovery-journals-after.json`. No raw device instance, connect key,
serial, machine/account name or user path is exported in this record.
