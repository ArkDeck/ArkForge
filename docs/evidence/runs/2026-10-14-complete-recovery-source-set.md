# Complete outstanding recovery source set

Status: implementation merged as PR #18; published-runtime read-only validation
completed. The current Windows campaign remains blocked by its consumed source
allowance. No hardware flash pass or production support is claimed.

The standalone authority previously sealed only the selected source job. An
additional unknown job made the lane check refuse without checking whether the
same distinct replacement could cover every source. It now enumerates the full
outstanding set and assesses the exact sealed candidate against each verified
journal, old authority plan, spent control permit where required, immutable
content and fresh target lineage. Each source must independently pass the
published complete-overwrite coverage checks. The authority-local proof binds
all sources to one candidate, with a bounded canonical source list. The CLI
`--job` selects the anchor, not a subset; machine help explains the complete-set
requirement. Existing
single-source proof bytes remain readable and unchanged.

Every admission rechecks the exact complete outstanding set. An added or omitted
unknown, changed source, pending cancellation, incomplete coverage or different
physical identity refuses before any further effect. Each source contributes
its existing one-use replacement allowance. All source reservations and job
correlations must match the same proof before a permit is eligible. Partial
publication is conservatively spent and cannot authorize an orphan. Only the
replacement's complete semantic receipts and successful postflight establish
completion relations for the covered sources; original journals stay immutable.

No ancestry rule turns an already-used source allowance into a new one. The
current campaign's original source already has an immutable reservation for the
failed independent recovery. Its proof was issued at 12:06:08.824 UTC and expires
at 13:06:08.799 UTC on 2026-10-14. The proof and reservation each remain 1,058 bytes,
SHA-256 `a4f878c20cb4c760aa9d089f4a3a8eb8dde4977eb267c3e9c083aa5c83487b6d`.
The expiry is a deadline, not remaining epoch allowance. Selecting the newer
unknown job cannot replenish the older source. The new read-only refusal names
the exhausted source and the complete outstanding set. Historical target
identities also remain sealed; no topology alias or migration is introduced.

## Local targeted checks

Commands use the fixed external Cargo cache, two build jobs, static CRT and
`--locked --offline --release --target x86_64-pc-windows-msvc -j 2`.

- `cargo test -p arkforge-standalone supervisor::recovery`: exit 0, 11 passed.
  Log: `complete-recovery-set-targeted.log`.
- `cargo clippy -p arkforge-standalone -p arkforge-cli --all-targets
  --all-features -- -D warnings`: exit 0.
  Log: `complete-recovery-set-clippy.log`.
- `cargo fmt --all -- --check`: exit 0.
- After updating machine help, CLI Clippy and all-target tests passed again
  (exit 0, 56 tests); `complete-recovery-set-cli-final.log`.
- `cargo test -p arkforge-standalone -p arkforge-cli -p arkforge-conformance
  --all-targets`: exit 0, 124 passed, 0 failed, 1 ignored child-only
  helper (its parent launches and validates it).
  Log: `complete-recovery-set-tests.log`.

Logs remain in the local `windows-inbox-winusb-20261014` campaign directory;
private host paths, USB identifiers and machine information are not committed.
Tests cover all-source sealing, exact-set drift, duplicated/nested/oversized
proofs, shared deadline mismatch, one-use reservations, historical consumption
across re-reads, partial publication, correlation loss, and existing identity,
coverage and semantic-completion guards. No fixture is hardware evidence.
Swift and the full cross-platform workspace are delegated to PR CI.

## CI

PR [#18](https://github.com/ArkDeck/ArkForge/pull/18) passed all four checks on
final head `ed0d39953b7b60a9fcc04a157b893106fde1ec6f`:

- PR Windows: [38567139025](https://github.com/ArkDeck/ArkForge/actions/runs/38567139025).
- PR macOS Rust/Swift: [38567139002](https://github.com/ArkDeck/ArkForge/actions/runs/38567139002).
- Push Windows: [38567130576](https://github.com/ArkDeck/ArkForge/actions/runs/38567130576).
- Push macOS Rust/Swift: [38567130647](https://github.com/ArkDeck/ArkForge/actions/runs/38567130647).

The PR merged at 12:56:06 UTC as
`2c27f28bb143f13cdd8120a08c0a117834cba66b`. A clean checkout of that published
revision produced the frozen binaries for read-only validation:

- `arkforge.exe`: SHA-256
  `6acf9264f902e3806c2c29c500a159e475bfb7da99da5520817df0b3f1a8b04c`.
- `arkforged.exe`: SHA-256
  `c9f773a78f1cf5eaae4823ab27a706472dd7219815680088556781ea0ed419c2`.

Build exit 0; `published-complete-recovery-set-build.log` and its JSON inventory
remain local. These unsigned source builds serve the explicitly named campaign;
they are not a formal signed Windows release acceptance.

## Published Runtime read-only result

Pairing epoch 13 opened the existing Runtime namespace. Deep device discovery
and probe succeeded with the current Loader observation and strong serial plus
topology identity. Status reported three terminal jobs, two unknown, and zero
active jobs. No historical identity was migrated or changed.

`job recover` selected the newer unknown job but the authority enumerated both
unknown sources. It exited 3 with `RECOVERY_PROOF_REQUIRED`, explicitly naming
the original source's exhausted allowance and the complete two-job set. Both
`plan` and `apply_command` were null. This is a budget refusal, not evidence that
all later identity/coverage gates passed. No new job or device dispatch occurred.
The result is recorded in `complete-recovery-set-authority-blocker.json`.

After the request returned, typed `daemon stop` succeeded. All three historical
journals retained their exact byte counts and SHA-256 values; the single proof,
reservation and correlation retained their original hashes. No completion
relation appeared. The final status and hash comparison are stored in
`runtime-complete-recovery-set-final-status.json` and
`complete-recovery-set-final-integrity.json`. The shared HDC server was left in
place. No additional device operation remains running.

The software path now proves a complete source set when every source is eligible
and still has its original allowance. For this board, covering the newer unknown
necessarily includes the already-consumed original source. A new plan id,
source selection, runtime restart, user acknowledgement or later deadline cannot
restore that allowance. Under the requested preservation constraints and
POL-RECOVERY-001, further destructive dispatch stays zero. Windows full automatic
flash acceptance remains incomplete.
