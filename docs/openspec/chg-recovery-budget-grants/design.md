# Grant lifecycle
status: proposed

1. Read and validate all legacy reservations and their exact proofs/correlations.
   Identical complete-set reservation bytes count as one old epoch, including a
   partially published source set. Hash the exact immutable records.
2. Read ordered new grants. Every ordinal must follow its predecessor and its
   seal must match the previous history digest. Count every grant, including
   grants with no job correlation. Missing intermediate records, changed legacy
   records, malformed proofs and orphan correlations fail closed.
3. The typed recovery request checks the complete outstanding source set and
   fresh identity, materializes a distinct plan and seals the next ordinal plus
   prior history hash in the authority-local proof. Planning consumes nothing.
4. Before start, revalidate the exact plan, source proofs, identity, lifetime and
   serial lane. Atomically publish the ordinal's grant by a non-overwriting hard
   link. Competing plans have the same ordinal and only one publication wins.
5. Correlate the returned new job in a separate immutable record. A crash before
   this record consumes the grant but admits zero permits. Every subsequent
   effect rechecks proof, full source set, ledger and exact job correlation.
6. Complete only from all required semantic receipts and successful postflight.
   Store completion under the new grant. The original journals, outcomes,
   legacy reservations and correlations are never edited. Completion can
   release the lane obstruction, but never refunds a grant or replays a plan.

The namespace total is 16; each grant is one epoch, at most four hours and no
longer than its plan. The same bound applies across restarts and chosen anchors.
The owner-only runtime directory remains the existing storage trust boundary;
this is not a claim of protection against an administrator replacing the entire
runtime with a historical backup. Agent/caller manipulation of that directory
is not an authorization path. Older proof bytes remain readable for history
and completion, but newly starting a legacy plan cannot allocate new budget.

No new wire fields or generic override flags are exposed. No old source identity
check is relaxed to make an exhausted budget usable.
