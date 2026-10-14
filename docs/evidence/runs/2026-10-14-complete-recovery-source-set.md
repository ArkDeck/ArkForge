# Complete outstanding recovery source set

Status: software implementation under validation. The current Windows campaign
remains blocked; this change does not reset a consumed recovery reservation or
claim a hardware flash pass.

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

Pending PR creation and current-head Windows / macOS Rust + Swift results.
Device validation uses only a clean protected-main build after all checks pass.
