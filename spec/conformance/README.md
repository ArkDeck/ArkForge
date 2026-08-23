# `spec/conformance/`
status: informative (the fixtures themselves are normative)

`v1/` holds the conformance fixtures for spec version 1. They are generated
from the Rust reference implementation by `crates/arkforge-conformance` and
committed; `manifest.json` lists every file with its SHA-256.

## Layout

```text
v1/
├── manifest.json
└── <suite>/
    └── AF-CONF-<SUITE>-<NNN>/
        ├── case.json          id, title, requirements[], kind, description, input, expected, files{}
        ├── <input files>      e.g. signing-body.cbor, journal.bin, archive.tar.gz
        └── <file>.hex         32-bytes-per-line hex twin of every binary file, for diffs
```

`kind` ∈ `encode` (value → bytes), `decode` (bytes → value or rejection),
`digest` (bytes/value → digest), `verify` (inputs → accept/reject with code),
`derive` (inputs → decision), `replay` (file → replay report), `table` (a
normative table to compare against).

## How a port runs the suite

1. Read `manifest.json`; verify every listed file's SHA-256. Refuse to run on a
   partial or modified tree.
2. For each `case.json`, build the input from `input` and the referenced files,
   run the operation named by `kind`/`description`, and compare with
   `expected`. Where `expected.errorClass` / `expected.code` is given, the
   *rejection* is normative and the class SHOULD match.
3. Report per suite. A stage of the porting order is complete when its suites
   pass (README §3).

The runner is the port's own code; it MUST NOT call the Rust implementation.

## Suites (v1)

| suite | cases | what it pins |
|---|---:|---|
| `sha256` | 7 | FIPS 180-4 vectors incl. 1 MB |
| `hmac-sha256` | 8 | RFC 4231 + key-length edges |
| `canonical-cbor` | 69 | encode bytes, key ordering, depth limit, every rejection class |
| `permit` | 49 | full signing-body bytes for the three published vectors, every verification outcome and order, strict decoding |
| `admission` | 15 | snapshot digest (with/without session), freshness precedence |
| `journal` | 13 | record bodies and chain, fsync policy table, on-disk framing, exhaustive torn-tail table, tamper refusals |
| `crash` | 21 | journal prefix → crash disposition and permit ledger |
| `state-machine` | 5 | states, flags, complete legal-edge set, named invariants |
| `action-receipt` | 5 | canonical semantic receipt body/digest plus invalid and duplicate fact-key refusals |
| `protobuf` | 31 | message bytes, enum values, decoder refusals, framing, negotiation |
| `rebind` | 18 | rebind evaluation over observation sequences |
| `reconcile` | 6 | possible-effect-set digest and closed read-only reconciliation reducer |
| `transcript-dispatch` | 2 | golden transcript step → canonical receipt digest and missing-action refusal |
| `strict-yaml` | 33 | accepted value trees and refusals |
| `plan` | 9 | DAYU200 fixture archive → manifest → profile → observation → private actions → public steps → projection → effect set → sealed plan, every digest with its exact preimage |
| `cli` | 4 | process-boundary stdout/stderr/status bytes for version, help and errors |

## Remaining corpus work (not a semantic gap)

- A committed corpus of parser-rejected archives. The same rejection surface
  is currently exercised by the seeded mutation campaign in
  `crates/arkforge-artifact/tests/parser_fuzz.rs`.

## Regenerating

```bash
cargo run -p arkforge-conformance -- generate
cargo run -p arkforge-conformance -- check
cargo run -p arkforge-conformance -- validate
cargo test -p arkforge-conformance
```

The test fails on any drift between the committed tree and what the reference
implementation produces. A diff is a spec change: bump `spec/manifest.yaml`,
explain it in the affected requirement, and treat it as breaking for every
other implementation unless it only adds cases.
