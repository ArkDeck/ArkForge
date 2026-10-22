# Conformance fixture growth
status: informative

`conformance/v1/` is the highest-authority artifact in the spec, and it is
also a git tree people clone. As of 2026-10-16 it is about 5 MB and under
800 files. Most of that is the canonical-CBOR, permit, and plan suites. New
cases should stay reviewable at that scale.

## What to add

Add a case when a requirement needs a byte a port can check without reading
the Rust implementation: a digest, an encoding, a rejection, or a decision.
Update the requirement's `tests:` range, the suite `cases:` count in
`docs/spec/manifest.yaml`, and the suite table in `conformance/README.md`. Then
regenerate and review the diff as a spec change.

Prefer extending an existing suite. A new suite also needs a row in that
table and in `manifest.yaml`.

## What to leave out

Do not commit an upstream corpus whole. A full CAVP response file, a fuzz
corpus, a device image, or every length in a publisher's set does not belong
in `conformance/v1/`. Commit a reviewed subset that hits the boundaries the
requirement names (empty, one block, the padding boundary, one long sample).

Large regular inputs are described, not stored. The one-million-byte SHA-256
vector is `repeatByte` plus `count` and has no `input.bin`. Use that shape
when the bytes are a repetition or a formula.

Each binary file gets a `.hex` twin, so a stored input costs about twice its
size in review. Keep one new case's stored bytes, twin included, under 64 KiB
unless the requirement is specifically about one long official sample. Then
commit that one sample, not the series around it.

`fuzz/artifacts` and `fuzz/target` stay untracked. A crash worth keeping
becomes one small fixture or one unit-test input, not the campaign that found
it. `fuzz/corpus/<target>/seed-*` may hold a reviewed seed copied from an
existing conformance sample, transcript, or in-repo fixture. Each seed stays
under 64 KiB. Other files libFuzzer writes into that directory stay untracked.

The "committed corpus of parser-rejected archives" noted in
`conformance/README.md` is the same kind of subset: enough rejects to pin the
typed errors, not the mutation campaign's output.

## How expected bytes are chosen

`cargo run -p arkforge-conformance -- generate` rewrites the tree from the
reference implementation. For a published vector (NIST, RFC, or another
external oracle), the expected digest in the generator is that published
value. The generator checks the implementation against it. Regenerating is
not a way to accept a mismatch.

If a fixture and the prose disagree, that is a spec defect. Record it in
`docs/spec/ISSUES.md`. Do not edit an expected value to hide a behavior change.
