# Contributing to ArkForge

Thanks for helping. ArkForge is a device-neutral firmware operations platform.
Read [README.md](README.md) for what it does today and which devices are
actually supported. The Chinese readme is [README.zh-CN.md](README.zh-CN.md).
Community documents in this repository are English.

## Ground rules

- The Rust workspace has no third-party runtime dependencies. SHA-256,
  deterministic CBOR, DEFLATE, tar, and the Protobuf wire codec are implemented
  in-tree. See [AFD-0001](docs/decisions/AFD-0001-zero-dependency-core.md). Do
  not add a crates.io dependency to a workspace member. The separate `fuzz/`
  workspace may use libFuzzer tooling; it is not part of the runtime graph.
- The language-neutral specification outranks the Rust code. Precedence is
  conformance fixture bytes, then machine-readable models, state machines, and
  errors, then requirements, then architecture documents, then the Rust
  implementation. Record disagreements in [spec/ISSUES.md](spec/ISSUES.md).
  Follow [spec/AUTHORING.md](spec/AUTHORING.md) when you change the spec, the
  wire format, digests, or generated fixtures.
- A pull request that builds and tests a change is not permission to flash,
  reset, or rescue a device, and it does not publish hardware support. Device
  work uses the typed `arkforge` CLI and daemon, a sealed plan, and an
  authorized hardware campaign. Do not bypass admission by calling a vendor
  flashing tool.
- This repository does not publish git tags or GitHub Releases. Consumers pin
  a full git commit. Contract identity is described in
  [docs/arkdeck-contract.md](docs/arkdeck-contract.md).

## Set up

The toolchain is pinned by `rust-toolchain.toml` (Rust stable, minimum
1.99.0, Edition 2024). Supported CI hosts are macOS and Windows x64. Linux
has no native USB backend.

```bash
git clone https://github.com/ArkDeck/ArkForge.git
cd ArkForge
cargo build --workspace
```

Swift SDK tests need a Swift toolchain and are run from the repository root
(`Package.swift` stays at the root):

```bash
swift test
```

[`just`](https://github.com/casey/just) is optional. The root `justfile`
wraps the commands below. It is not a runtime dependency.

## Build, test, lint, format

| Recipe | Command |
| --- | --- |
| `just build` | `cargo build --workspace` |
| `just test` | `cargo test --workspace --all-targets` |
| `just lint` | `cargo clippy --workspace --all-targets --all-features -- -D warnings` |
| `just fmt` | `cargo fmt --all` |
| `just ci` | The host checks in [`.github/workflows/rust.yml`](.github/workflows/rust.yml) |

`just ci` checks formatting, runs the preboot and boot-loop script tests,
clippy, `cargo test --workspace --all-targets`, and `swift test`. Windows
packaging parse and no-hardware acceptance guards live in
[`.github/workflows/windows.yml`](.github/workflows/windows.yml) and run on
the Windows runner.

For a tight loop while editing one crate:

```bash
cargo test -p <crate> <test-filter>
```

Before you open a pull request that changes Rust runtime behavior or crosses
crate boundaries, run the same three checks CI treats as the Rust gate:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
```

Also run `cargo test -p arkforge-conformance` when you change the spec or an
encoding. Regenerate fixtures with
`cargo run -p arkforge-conformance -- generate` only when the byte change is
the intended spec revision, and review that diff. Do not regenerate fixtures
to hide a behavior change.

Swift wire changes need `swift test` from the repository root as well as the
affected Rust crate tests.

## Pull requests

Use a focused branch. Maintainers rebase-merge. Do not merge your own device
or release side effects into the branch: no git tags, no GitHub Releases, and
no hardware mutation claimed as evidence unless the campaign record is already
the point of the change.

In the pull request:

- Say what changed and why.
- Call out spec, wire, digest, or ArkDeck contract impact. If there is none,
  say so.
- List the commands you ran and what you could not run.
- Link a device report or evidence record when the change depends on hardware.

Use the pull request template. It is the checklist, not a substitute for the
diff.

## Issues

- Bugs: the bug report template.
- Features and spec changes: the feature template.
- A specific board, flash, rescue, or recovery run: the device-report template.
  Do not paste pairing secrets, private keys, or firmware you cannot share.

Security reports go through [SECURITY.md](SECURITY.md), not a public issue.

## Conduct

This project follows [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
