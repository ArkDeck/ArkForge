# Verification
status: proposed

Acceptance cases in `supervisor/recovery_tests.rs`:

- A distinct grant follows consumed legacy budget while preserving old bytes.
- A complete source set consumes one epoch and binds one exact new job.
- Competing plans cannot consume the same ordinal; an orphan grants no permits.
- Sixteen consumed epochs exhaust the namespace, including failed/orphaned ones.
- Missing intermediate records, changed legacy records, corrupt seals and
  orphan correlations refuse further budget allocation.
- Exact issuance and expiry boundaries apply independently to every proof.
- Existing no-replay, source-seal, identity drift, complete-set and semantic
  receipt tests continue to pass.

Local targeted checks and CI results are recorded in the PR and execution
record. Unit tests are fixtures, not hardware evidence. Real acceptance must
run published main against the same board and retain all original journals;
it requires fresh identity proof independently of this budget change.
