# Verification
status: proposed

- Reject unsuccessful, incomplete, foreign-plan and uncorrelated source jobs,
  missing/duplicate receipts, arbitrary targets and changed image digests.
- Reject wrong identity, mode, profile, campaign, missing HDC binding, active
  jobs, invalid budgets, unsafe mapping, incorrect offset and insufficient size.
- Detect writable partitions by device number, including bind mounts,
  superblock options and whole-disk mounts. Skip before hashing.
- Hash the complete bounded image range, including 2 GiB and partial blocks;
  reject malformed/truncated output, remote errors, missing success marker,
  content differences and mapping changes. Never send the expected hash.
- Drain HDC output concurrently with a hard byte cap and process deadline,
  including output larger than Windows pipe capacity.
- Persist a fresh intent and result without replacing any historical file;
  partial results are not exit-0 complete verification.
- Keep existing CLI conformance bytes and Loader behavior unchanged. Run
  affected Rust tests, workspace fmt/clippy/tests and both host CI workflows.
- After all CI is green and the authorized PR is merged, build clean published
  main and run the new typed command on the same completed job and board.
  Check every full-range digest, the explicit userdata skip, immutable source
  journals and old recovery records. Record the actual result separately from
  read-only development diagnosis. Do not manufacture a ProductionVerified
  state or rewrite old job evidence.
