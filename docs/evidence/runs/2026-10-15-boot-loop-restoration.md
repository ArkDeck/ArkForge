# Bounded boot-loop ramdisk restoration

The explicit host script listens through the existing published typed CLI and
restores only the pinned complete original ramdisk. It requires a confirmed
full-flash final reboot, source-bound device identity and exact acknowledgement.
It does not replay the source job or retry an uncertain restoration apply.

## Local targeted checks

- `python -m unittest discover -s scripts -p test_restore_boot_loop.py`: exit 0,
  14 host-only tests. Cases include delayed Loader and Normal appearance,
  no USB, incorrect/ambiguous/weak identity, incomplete or uncertain source
  receipts, disappearance during planning, changed restoration effects,
  one apply after a read-only observation expiry, no retry after a lost apply
  reply, full-hash-only completion and refusal to reuse an output directory.
- `python scripts/restore-boot-loop.py --help`: exit 0.
- `git diff --check`: exit 0.
- Logs: `preboot-userdata-20261015/boot-loop-checks-01.{json,log}`.
- Rust and Swift were not rerun locally because no Rust, Swift or wire code
  changed. Both CI workflows run the new Python suite alongside their existing
  platform tests.

All orchestration tests use synthetic responses. They do not establish USB
visibility, write success, device recovery or hardware support.

## CI

Pending this PR head's Rust/Swift and Windows workflows. The PR body records
their exact run IDs and final result without amending a green commit.
