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

PR #27 passed exact-head Rust PR/push runs 38677397344/38677370940 and Windows
PR/push runs 38677397393/38677370932 before merge. This increment requires its
own exact-head CI. Swift is covered by CI, not this Windows host. No hardware
pass or production-support promotion is claimed by these host checks.
