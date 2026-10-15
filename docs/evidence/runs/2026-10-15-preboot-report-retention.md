# Preboot report retention and restoration profile

Status: host validation; hardware records remain separate and local.

Early kernel messages can disappear before Normal-mode verification. The
acceptance wrapper now retains its small initramfs report on an inherited
read-only FD in a ten-minute sleeping process. The verifier requires the exact
unique holder command, designated report path, RAM backing, regular file,
root ownership, 0400 mode and bounded size before validating all original
nonce/boot/storage/range fields. A missing, expired or failed report fails.
The wrapper preserves its original umask when starting stock init.

`flash restore-ramdisk` now accepts the explicit profile required by existing
identity admission. It still requires the exact device, pinned full-extent
ramdisk, campaign, plan digest, acknowledgements and ordinary Runtime permits.

## Local targeted checks

Windows release/static-CRT `cargo fmt --all -- --check`, workspace Clippy with
`--all-targets --all-features -- -D warnings`, daemon build and workspace tests
all exit 0. Cargo used `--locked --offline --release --target
x86_64-pc-windows-msvc -j 2` and the existing fixed session cache. The final
workspace test run took 172.016 s. Logs: local campaign
`preboot-userdata-20261015/retention-checks-02.{json,log}`.

Regression tests cover explicit restoration profile acceptance and malformed
profile rejection, missing RAM storage attestation, failed/prefixed/duplicate
reports and the existing nonce, boot, storage and range checks. The first host
test attempt used a non-SHA artifact placeholder; the fixture input was fixed
without changing production validation or weakening assertions.

## CI

[PR #27](https://github.com/ArkDeck/ArkForge/pull/27) merged after all
four workflows succeeded on head `0a4ad61da0e12299eb3c406cdb3ae3398e1916c3`.

- Rust/Swift: [38677397344](https://github.com/ArkDeck/ArkForge/actions/runs/38677397344), [38677370940](https://github.com/ArkDeck/ArkForge/actions/runs/38677370940).
- Windows: [38677397393](https://github.com/ArkDeck/ArkForge/actions/runs/38677397393), [38677370932](https://github.com/ArkDeck/ArkForge/actions/runs/38677370932).

This CI follow-up preserves the findings and validation scope recorded above.
CI success does not rewrite Runtime outcomes or promote hardware support.
