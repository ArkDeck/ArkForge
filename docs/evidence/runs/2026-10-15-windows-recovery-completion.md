# Windows automatic recovery and durable completion

Scope: the existing named DAYU200 Windows campaign and runtime namespace.
This records the observed campaign result, not a production-support promotion.

Published source `8ee7ede54dd41218fb565389fc25fdadb140286a` completed all 23
steps in Runtime epoch 17. The clean CLI SHA-256 was
`eb7d1a01fe91f757213549dc9965c5be85fc41d58a38a8a1ebe25dd0493aca66`;
daemon SHA-256 was
`fed4e3d44db3c722d67a36f14cf851f24aa8fef7ddd378dba4a5c1498f5c7741`.
Runtime admitted a new complete-overwrite plan covering all three unknown
sources and consumed independent grant 3. Normal-to-Loader transition, nine
partition writes, verification, reset, return to Normal and final postflight
completed automatically. The terminal Job was `succeeded` at 23/23 steps.

`uboot` and `resource` have full-hash verification receipts; the other seven
have explicit `skipped-lba-read-window` receipts under the existing read-domain
limit. The successful postflight receipt reports `OpenHarmony-7.0.0.43` and
model `ohos`. Runtime wrote the grant's completion record covering all three
sources. Their original unknown outcomes and previous budget consumption
remain intact. No cancelled or unknown historical Job is relabeled successful.

A restart preserved the successful Job and its 23 receipts, but an old-source
recovery request unexpectedly materialized another plan. That plan was never
applied: no fourth grant or new Job was created. Comparison of original and
replayed receipts showed identical semantic values but different fact-list
order in 20 receipts. The authority had compared the completion record's
protobuf bytes with the journal's canonical-map replay order.

The fix normalizes only receipt fact order during comparison, after validating
the semantic body and closed local protobuf encoding. All other receipt and
completion-envelope fields remain exact, and existing source, reservation,
correlation and successful-terminal checks still run. Unknown or duplicate
protobuf fields and duplicate fact keys fail closed. Existing records are
read without rewriting them. Both legacy and independent-grant completion use
the same comparison. This repair requires no additional flash or budget.

Private raw records include `postflight-readiness-recovery-plan.json`,
`postflight-readiness-job-success.json`, `postflight-readiness-restart-status.json`,
`postflight-readiness-restarted-job-success.json` and
`postflight-readiness-completion-admission-check.json`. They retain the
successful run and the subsequent failed closure check separately.

## Local targeted checks

`cargo fmt --all -- --check`,
`cargo clippy --locked --offline --workspace --all-targets --all-features -- -D warnings`
and `cargo test --locked --offline --workspace --all-targets` all exited 0;
736 tests passed. Private logs are `completion-receipts-{fmt,clippy,workspace}.log`.
The targeted completion filter passed both tests, exit 0, in
`completion-receipts-targeted-2.log`; its initial compile exposed an incorrect
test-only trait import, which was fixed before the passing runs.
Regression coverage replays canonical receipt bodies, reads an unchanged
persisted completion, and rejects
changes to every receipt field, envelope fields, duplicate keys and unknown
wire fields. `git diff --check` passed. Swift is covered by macOS CI rather
than this Windows host. Published restart validation will read the original completion
and require covered sources to be refused before any new plan is returned.

## CI

[PR #24](https://github.com/ArkDeck/ArkForge/pull/24) merged after all
four workflows succeeded on head `f9568f6e975457c1f12484cf1ee66e8fa1bd7c3c`.

- Rust/Swift: [38666422961](https://github.com/ArkDeck/ArkForge/actions/runs/38666422961), [38666417521](https://github.com/ArkDeck/ArkForge/actions/runs/38666417521).
- Windows: [38666422918](https://github.com/ArkDeck/ArkForge/actions/runs/38666422918), [38666417549](https://github.com/ArkDeck/ArkForge/actions/runs/38666417549).

This CI follow-up preserves the findings and validation scope recorded above.
CI success does not rewrite Runtime outcomes or promote hardware support.
