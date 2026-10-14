# Independent recovery after an unknown initial mode switch

Status: implementation and fixture validation. This record does not report a
new hardware flash pass or publish production support.

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

The implementation PR's exact head must pass Windows and macOS Rust workflows,
including macOS Swift tests, before merge and any published-runtime recovery.
PR number, run ids and the actual hardware result will be recorded after those
steps finish. Local fixtures do not establish `REAL_DEVICE_PASS`.
