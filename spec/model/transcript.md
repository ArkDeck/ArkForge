# Transcripts
status: draft
source: crates/arkforge-transport/src/transcript.rs, crates/arkforge-transport/src/replay.rs
schema: transcript.schema.json

A transcript is a recorded device interaction that the replay transport can
play back so the whole vertical — discover, probe, materialize, dispatch — runs
without hardware. It is evidence of *what a campaign reported*, and only a
`captured` transcript may be cited for how a protocol behaves.

## Header

| field | meaning |
|---|---|
| `schemaVersion` | `arkforge.transcript/v1` (anything else is refused) |
| `transcript.id` | OpaqueId; campaign ids are used for golden transcripts |
| `transcript.provenance` | `captured` / `derived-from-published-receipts` / `synthetic` |
| `transcript.source` | free text: where the record came from |
| `transcript.profileId` | the profile the recording applies to |

## Records

Records are ordered, `sequence` contiguous from 1, at least one record.
`kind` ∈ `observation`, `attach`, `detach`, `rebind`, `invocation`;
`status` ∈ `ok`, `failed`, `timeout`, `disconnected`. An `invocation` MUST name
an `action`; `observation`/`attach`/`rebind` MUST carry an `observation`.
Optional `requestBytes`/`requestDigest`/`responseBytes`/`responseDigest` describe
payloads by length and hash only; `semantic` is a list of typed `{key, value}`
facts.

## Digest

`transcriptDigest = SHA-256("arkforge/v1/transcript\0" || cbor(transcript))`
over `{schemaVersion, id, provenance, source, profileId, records: [record…]}`
where each record is `{sequence, kind, atEpochMs, durationMs, status,
observation|null, action|null, requestBytes|null, requestDigest|null,
responseBytes|null, responseDigest|null, semantic: [{key, value}…]}` and an
observation is `model/digest-bodies.cddl#device-observation` (with
`observedAtEpochMs` taken from the record's `atEpochMs`). Fixture
AF-CONF-PLAN-003 pins the digest of the published GJ-4 transcript.

## Replay semantics

The replay transport answers `discover` with the observations the transcript
contains (first stable observation per mode), `open_exact` against a recorded
observation, `wait_for_rebind` by evaluating the recorded attach/rebind
observations with the real rebind rule (`requirements/transport.md`), and a
provider action by looking up the recorded `invocation` with the same `action`
and returning its recorded status and digests. An action the transcript never
recorded is `TRANSPORT_UNSUPPORTED`. A replay toolchain's backend digest is
the SHA-256 of the transcript file bytes and its kind is `replay`, which can
never publish `productionVerified` (AF-TRN-021).

## Golden transcripts in this repository

| file | provenance | origin |
|---|---|---|
| `transcripts/dayu200-gj4-ecamp-96effff15.yaml` | derived-from-published-receipts | ArkDeck GJ-4 campaign ECAMP-96EFFF15, 13-step receipt chain, 2026-08-05 |
| `transcripts/dayu200-gj4-ecamp-31e041bc.yaml` | derived-from-published-receipts | ArkDeck GJ-4 campaign ECAMP-31E041BC |
| `transcripts/dayu600-research-synthetic.yaml` | synthetic | DAYU600 research (no device measured) |

Every digest in a derived transcript is `SHA-256("arkforge/derived-transcript/v1|<id>|<field>|<basis>")`
and is recomputed by `crates/arkforge-transport/tests/golden_transcript_parity.rs`,
so a derived file cannot drift into looking like a capture.
