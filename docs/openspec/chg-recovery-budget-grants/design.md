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

## Existing Windows topology encoding migration

Some durable Normal identities precede canonical USB 2/3 companion-connector
encoding. The live Windows hub observer can compute both encodings from the
same present PnP node while checking that its binding and descriptors remain
unchanged during the census. A projection is usable only for one Normal node
whose current observation id, serial, descriptor and topology digests all
match. Ghost nodes, caller values and shared-port guesses are not inputs.

Runtime changes only observation id and topology encoding in copies of the
fresh discovery/probe facts, then requires the full recomputed stable digest
to equal the original durable identity exactly. Serial, mode, descriptor,
profile, protocol facts and strength remain unchanged. The new plan binds
the current canonical identity. A closed migration seal records both identity
digests in each source proof; before start the live proof must match again.
Subsequent effects retain the ordinary new job's bound mode lineage.

This permits an encoding migration, not a Normal/Loader identity substitution.
An old Normal-only lineage cannot be recovered from a currently different
Loader serial without a real rebind relation. No historical record is edited.

## Read-only postflight tail

If all writes, verification and reboot receipts are durable but the final
property read is unknown, its bounded persistent effect set is empty. That
does not erase the source from the complete outstanding set or permit the old
read to be replayed. The daemon may assess a distinct full replacement when
the stored source public/private plan identifies the final read-only
`read-build-facts` action, all earlier steps have unique semantic-success
receipts and checkpoints, and exactly one unresolved permit correlates through
acceptance, intent, consumption and dispatch. Its original deadline must have
expired. Missing source storage, cancellation or a second unknown fails closed.
The existing complete candidate validation, new grant and source seals apply.

For new executions, the current admitted property request may repeat only its
fixed read-only query, on the exact target and within its original deadline,
until every expected value matches. A mismatching value never becomes an
accepted receipt. No mutating retry, deadline extension, old-job completion
or hardware support promotion is introduced.
