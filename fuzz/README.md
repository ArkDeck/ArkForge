# fuzz

`cargo-fuzz` targets for the in-house decoders that accept external bytes.
`libfuzzer-sys` lives only in this directory. `fuzz/` is its own Cargo
workspace and is excluded from the runtime workspace, so a plan digest still
does not depend on a third-party crate (AFD-0001).

The deterministic mutation tests stay in the crates. They run on every
`cargo test` and are the regression gate. These targets are the explorer.
A crash found here should be reduced and added as a unit test next to the
parser, then fixed.

| target | entry points |
|---|---|
| `tar` | `arkforge_artifact::tar::TarReader` |
| `deflate` | `InflateReader` and `GzipReader` |
| `cbor` | `arkforge_core::digest::decode_canonical` |
| `protobuf` | protobuf `Reader`, `Hello` / `Request` / `Response` decode, length-prefixed frames |
| `artifact-import` | DAYU200 `inspect`, PAC `inspect`, `parse_parameter` |
| `profile` | strict YAML `parse` and `profile::load` |

Content-addressed import writes the host filesystem, so it is not a fuzz
entry point. The parsers a container passes through on import are.

```bash
rustup toolchain install nightly --profile minimal --component rust-src
cargo install cargo-fuzz --version 0.13.2 --locked
cd fuzz
cargo +nightly fuzz run tar -- -max_total_time=30 -max_len=4096
```

CI runs each target for a few seconds (`fuzz.yml`). That is a smoke pass,
not an overnight campaign. `fuzz-nightly.yml` runs the same targets for five
minutes each. Both jobs upload `fuzz/artifacts` when a target crashes.

Reviewed seeds live in `fuzz/corpus/<target>/seed-*`. Each file is under 64
KiB and is copied from a conformance sample, a transcript, or the in-repo
DAYU200 fixture shape (`spec/conformance/GROWTH.md`). libFuzzer reads that
directory at the start of a run. Campaign files it adds are gitignored.

## Mutation tests

These still run under `cargo test`:

| target | location | coverage |
|---|---|---|
| DAYU200 archive | [`crates/arkforge-artifact/tests/parser_fuzz.rs`](../crates/arkforge-artifact/tests/parser_fuzz.rs) | gzip container + tar framing + parameter.txt syntax |
| gzip / DEFLATE | same file | seeded mutations |
| tar | same file | seeded mutations |
| parameter syntax | same file | seeded mutations |

The property under test is that any input returns a typed error instead of
panicking, hanging, or allocating without a bound. arkforged holds device
permissions, so a parser panic is a denial of service (architecture.md 20.1).

The mutator is seeded (xorshift64\*). A failure prints the seed and reproduces.
