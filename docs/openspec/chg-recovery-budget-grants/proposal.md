# Independent bounded recovery grants
status: proposed
scope: ArkForge standalone authority only

The existing one-reservation-per-source policy prevents a distinct complete
overwrite after any previous replacement fails. AF-REC-001 forbids replay of
unknown intent, but AF-REC-004 explicitly allows a distinct recovery plan
subject to authority admission. ArkDeck POL-RECOVERY-001 similarly requires
mechanical proof and valid budget; it does not impose one lifetime attempt.

Introduce AF-REC-009 and an append-only standalone grant ledger. Each new grant
requires all existing identity, complete-source, immutable artifact, published
coverage and verification checks. Prior consumption remains immutable and is
included in a fixed total cap of 16 epochs per runtime namespace. Each grant
expires with its plan or after four hours. There is no refund or caller budget
administration API. The finite cap limits repeated new recovery requests even
when fresh evidence remains available; it is a policy bound, not an automatic
retry count or a hardware claim.

No ArkDeck source, Profile coverage, public IPC, conformance fixture, transport
or device support declaration changes. A missing Normal/Loader identity
relationship still blocks admission. The PR contains the reviewable policy,
implementation and tests; this proposal does not itself approve execution.
