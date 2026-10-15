# Native staging for pinned ramdisk restoration

Status: host validation only; device records remain local and separate.

The native dispatcher required all profile image members even when the sealed
plan was the separately admitted one-partition ramdisk restoration. Host input
preparation therefore refused before write I/O. It now accepts exactly one
DAYU200 ramdisk write with the pinned complete restoration image, semantic
partition, destructive class and matching profile sector/byte range. Full
restore keeps exact profile-member parity; arbitrary partial sets do not pass.
Extraction and full staged-content validation remain unchanged.

## Local targeted checks

Windows release/static-CRT format, workspace Clippy with all targets/features
and warnings denied, daemon build, and workspace tests all exit 0. Cargo used
the existing fixed session cache, two build jobs, locked/offline execution and
the x86_64-pc-windows-msvc target. Tests took 117.543 s. Logs: local campaign
`preboot-userdata-20261015/staging-checks-01.{json,log}`.

The regression exercises the production host-preparation entry. Pinned input
metadata passes set selection; unrelated fixture bytes still fail complete
staged-image validation before device I/O. Altered digest, size, start, semantic
target, effect class, source member, duplicate writes and other profile IDs
are rejected. No test changes an accepted safety assertion.

## CI

[PR #28](https://github.com/ArkDeck/ArkForge/pull/28) merged after all
four workflows succeeded on head `a9726f74acc7981cd6f8776b4b6c6a7a92f47fc6`.

- Rust/Swift: [38678459207](https://github.com/ArkDeck/ArkForge/actions/runs/38678459207), [38678446942](https://github.com/ArkDeck/ArkForge/actions/runs/38678446942).
- Windows: [38678459520](https://github.com/ArkDeck/ArkForge/actions/runs/38678459520), [38678446983](https://github.com/ArkDeck/ArkForge/actions/runs/38678446983).

This CI follow-up preserves the findings and validation scope recorded above.
CI success does not rewrite Runtime outcomes or promote hardware support.
