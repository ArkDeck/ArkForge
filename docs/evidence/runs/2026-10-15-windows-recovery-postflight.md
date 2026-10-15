# Windows independent recovery reaches postflight

Scope: the existing named DAYU200 Windows campaign and unchanged runtime
namespace. The operator restored Normal boot while retaining USB connection.
This record is an execution diagnosis, not a production-support declaration.

Published source `23e00e519f6a05d355d872c591da7e1e237568be` passed all four
PR #22 Windows/Rust workflows before merge: `38574741434`, `38574741465`,
`38574685600`, `38574685291`. The clean published build's CLI SHA-256 was
`0c59cc07c2fcce9e30dcf3b610807ff4ed2828e5e221b0935d3acbfadbe7e5cd`; daemon
SHA-256 was `83299e2c790ed682e099a4d3f03d70d7f92b03549e49e3410d71e902445e7ce7`.

Runtime epoch 16 verified the original Normal identity and both outstanding
sources, then sealed a distinct complete-overwrite plan. It consumed independent
grant 2, preserving legacy grant 1. Automatic Normal-to-Loader rebind succeeded;
all nine partition writes succeeded. `uboot` and `resource` verified with full
hashes. The remaining seven verification receipts explicitly recorded
`skipped-lba-read-window`, the existing board read-domain limit. Reset and
return to Normal completed, but the final read-build-facts request was refused
with `HDC_PROPERTY_MISMATCH`; the job ended `outcomeUnknown` at 22/23 steps.
No superseding completion record was written and no old outcome was changed.

A subsequent independent, exact-target read-only HDC diagnosis returned both
`OpenHarmony-7.0.0.43` and `ohos`, matching the sealed expected facts. The failed
request did not retain raw property output, so its precise transient reply is
unknown. The later diagnosis is not a Runtime receipt and does not turn the
failed job into a pass.

All three prior journal hashes and the legacy proof/reservation/correlation
hashes remain byte-for-byte unchanged. The new grant and job correlation remain
spent. Runtime was stopped normally without stopping the external HDC server.
Private raw records include `normal-return-recovery-plan.json`,
`normal-return-recovery-apply.json`, `normal-return-watch-40-b.jsonl`,
`normal-return-watch-112.jsonl`, `normal-return-job-terminal.json`,
`normal-return-property-diagnosis.json` and `normal-return-historical-integrity.json`.

The follow-up retains exact property equality while allowing the same admitted
read-only request to wait within its original deadline. It also adds the narrow
stored-plan proof needed to include an expired read-only postflight tail in a
new complete recovery. It neither replays this job nor refunds either grant.

## Local targeted checks

Targeted daemon recovery assessment tests: 7 passed, exit 0, private
`postflight-recovery-targeted-2.log`. Targeted HDC tests: 14 passed, exit 0,
`postflight-hdc-targeted.log`; the child-process fixture is exercised by its
parent test. Final `cargo fmt --all -- --check`,
`cargo clippy --locked --offline --workspace --all-targets --all-features -- -D warnings`
and `cargo test --locked --offline --workspace --all-targets` all exited 0:
735 tests passed. Logs are `postflight-final-2-{fmt,clippy,workspace}.log`.
The first full run found missing traceability entries for the two new draft
requirements; their mappings were added and the unchanged traceability checks
passed. The final suite also checks that an expired command cannot open or
spawn HDC. `git diff --check` passed. Swift is covered by macOS CI rather than
this Windows host.

## CI

The new follow-up's exact-head CI is pending. Windows automatic full-flash
acceptance remains incomplete until a published build returns a successful
Runtime postflight and durable independent recovery completion.
