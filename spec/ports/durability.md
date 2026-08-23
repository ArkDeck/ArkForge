# Port: durable append (journal storage)
status: normative (guarantee), informative (platform notes)
source: crates/arkforge-engine/src/durable.rs; AD-017
requirements: AF-JRN-010..018, AF-CRASH-001

## Purpose
Give the engine one primitive — *append this record and do not return until it
is on stable storage* — on which every crash-window argument rests.

## Operations
| op | input | output |
|---|---|---|
| `open(path)` | path of the journal file | handle + `{recordsReplayed, tornTailBytes, existed}` or error class |
| `append(record, policy)` | canonical record bytes, `durable` / `buffered` | the record digest; returns only after the policy's guarantee holds |
| `sync()` | — | flushes buffered records; shutdown convenience, never required for correctness |

## Ownership and lifetime
One writer per file for the file's lifetime. The handle owns the open file;
closing it without `sync()` may lose buffered records only.

## Thread-safety
The handle is not shared between threads; the engine serializes appends per
job. An implementation MAY use a lock; it MUST NOT interleave two appends'
bytes.

## Durability guarantee (what "returned" means)
- `durable`: the frame (length + body) has been written and the file has been
  synchronized to the storage device's notion of stable storage (`fsync(2)` /
  `FlushFileBuffers`) before `append` returns. If `append` did not return, no
  external effect may follow.
- `buffered`: written to the OS; ordered behind the next durable append.
- The write of length and body is one write call, so a crash leaves a short
  file, never a valid length over absent bytes.

## What is NOT promised
Durability is stated to **process death**, not power loss. `fsync` on macOS
does not flush the drive's write cache (`F_FULLFSYNC` does, and the reference
implementation cannot reach it without `libc`, AFD-0001). A port that can issue
a full flush SHOULD, and MUST say in `mappings/<lang>.yaml` which level it
reaches; no layer may claim power-loss safety on the strength of this port.

## Crash / retry
`open` replays and verifies the whole chain; a torn tail is truncated and
reported; any other inconsistency refuses the file. `append` is not retried
after an I/O error — the job stops (the error is a refusal, not unknown
outcome, because nothing reached the device without a durable record first).

## Error classes
`JOURNAL_IO`, `JOURNAL_NOT_A_JOURNAL`, `JOURNAL_FRAME_LENGTH_INVALID`,
`JOURNAL_RECORD_TOO_LARGE`, `JOURNAL_SEQUENCE_BROKEN`, `JOURNAL_CHAIN_BROKEN`,
`JOURNAL_RECORD_TAMPERED`, `JOURNAL_FSYNC_POLICY_MISDECLARED`,
`JOURNAL_UNKNOWN_SCHEMA_VERSION`, `JOURNAL_RECORD_MALFORMED`, `JOURNAL_CBOR`.

## Conformance hooks
The exhaustive torn-tail table (AF-CONF-JOURNAL-004) and the tamper cases
(AF-CONF-JOURNAL-005..013) exercise `open` on byte-exact files; a port runs
them against its own implementation with the fixture files.
